use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use dbconnector_core::{
    new_connection_id, parse_target, redact_secrets, DatabaseKind, DbError, ErrorCode, ServerConfig,
};
use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock, Semaphore};

#[cfg(feature = "mongo")]
use dbconnector_mongo::{MongoLimits, MongoSession};
#[cfg(feature = "redis")]
use dbconnector_redis::RedisSession;
#[cfg(feature = "sql")]
use dbconnector_sql::{QueryLimits, SqlDialect, SqlSession};

enum Backend {
    #[cfg(feature = "sql")]
    Sql(SqlSession),
    #[cfg(feature = "mongo")]
    Mongo(MongoSession),
    #[cfg(feature = "redis")]
    Redis(RedisSession),
    /// Present so a build with no database features still has an inhabited enum.
    #[allow(dead_code)]
    Unsupported,
}

struct Entry {
    id: String,
    kind: DatabaseKind,
    read_only: bool,
    host: Option<String>,
    database: Option<String>,
    created_at: String,
    last_used: StdMutex<Instant>,
    backend: Backend,
    sem: Arc<Semaphore>,
}

pub struct AppState {
    pub config: ServerConfig,
    connections: RwLock<HashMap<String, Arc<Entry>>>,
    global: Arc<Semaphore>,
}

impl AppState {
    pub fn new(config: ServerConfig) -> Self {
        let global = Arc::new(Semaphore::new(config.global_concurrency.max(1)));
        Self {
            config,
            connections: RwLock::new(HashMap::new()),
            global,
        }
    }

    pub async fn open(&self, database_url: &str, read_only: bool) -> Result<Value, DbError> {
        if !read_only {
            self.config.ensure_write(false)?;
        }
        let target = parse_target(database_url)?;
        self.config.authorize_target(&target)?;
        {
            let guard = self.connections.read().await;
            if guard.len() >= self.config.max_connections {
                return Err(DbError::new(
                    ErrorCode::ResourceLimitExceeded,
                    "maximum number of connections is already open",
                ));
            }
        }
        let id = new_connection_id();
        let backend = self
            .connect_backend(database_url, &target, read_only)
            .await?;
        let version = backend_version(&backend);
        let capabilities = capability_names(target.kind, read_only, &self.config);
        let entry = Arc::new(Entry {
            id: id.clone(),
            kind: target.kind,
            read_only,
            host: target.host.clone(),
            database: target.database.clone().or_else(|| {
                if target.sqlite_memory {
                    Some(":memory:".into())
                } else {
                    None
                }
            }),
            created_at: dbconnector_core::now_rfc3339(),
            last_used: StdMutex::new(Instant::now()),
            backend,
            sem: Arc::new(Semaphore::new(
                self.config.per_connection_concurrency.max(1),
            )),
        });
        self.connections.write().await.insert(id.clone(), entry);
        Ok(json!({
            "connection_id": id,
            "kind": target.kind.as_str(),
            "status": "connected",
            "read_only": read_only,
            "server_version": version,
            "capabilities": capabilities,
        }))
    }

    pub async fn close(&self, connection_id: &str) -> Result<Value, DbError> {
        let entry = self
            .connections
            .write()
            .await
            .remove(connection_id)
            .ok_or_else(|| {
                DbError::new(ErrorCode::ConnectionNotFound, "connection was not found")
            })?;
        close_backend(&entry.backend).await;
        Ok(json!({"connection_id": connection_id, "closed": true}))
    }

    pub async fn list(&self) -> Value {
        let guard = self.connections.read().await;
        let connections: Vec<Value> = guard.values().map(|entry| summary(entry)).collect();
        json!({"connections": connections})
    }

    pub async fn info(&self, connection_id: &str) -> Result<Value, DbError> {
        let entry = self.entry(connection_id).await?;
        touch(&entry);
        let mut value = summary(&entry);
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "server_version".into(),
                Value::String(backend_version(&entry.backend)),
            );
            object.insert(
                "writes_enabled".into(),
                Value::Bool(self.config.allow_writes && !entry.read_only),
            );
            object.insert(
                "destructive_enabled".into(),
                Value::Bool(self.config.allow_destructive),
            );
            object.insert(
                "query_timeout_ms".into(),
                json!(self.config.query_timeout.as_millis() as u64),
            );
            object.insert("max_rows".into(), json!(self.config.max_rows));
            object.insert(
                "max_result_bytes".into(),
                json!(self.config.max_result_bytes),
            );
        }
        Ok(value)
    }

    pub async fn capabilities(&self, connection_id: &str) -> Result<Value, DbError> {
        let entry = self.entry(connection_id).await?;
        touch(&entry);
        Ok(json!({
            "connection_id": connection_id,
            "kind": entry.kind.as_str(),
            "read_only": entry.read_only,
            "server_version": backend_version(&entry.backend),
            "features": capability_names(entry.kind, entry.read_only, &self.config),
            "limitations": limitations(entry.kind),
        }))
    }

    pub async fn sql_query(
        &self,
        connection_id: &str,
        sql: &str,
        params: Option<&[Value]>,
        transaction_id: Option<&str>,
        limit: Option<u64>,
        timeout_ms: Option<u64>,
    ) -> Result<Value, DbError> {
        #[cfg(feature = "sql")]
        {
            let entry = self.sql_entry(connection_id).await?;
            let _permits = self.permits(&entry).await?;
            let limits = self.query_limits(limit, timeout_ms)?;
            let rows = sql_backend(&entry)?
                .query_read(sql, params, transaction_id, &limits)
                .await?;
            serde_json::to_value(rows).map_err(internal)
        }
        #[cfg(not(feature = "sql"))]
        {
            let _ = (
                connection_id,
                sql,
                params,
                transaction_id,
                limit,
                timeout_ms,
            );
            Err(DbError::unsupported(
                "this build does not include SQL tools",
            ))
        }
    }

    pub async fn sql_meta(&self, connection_id: &str, op: SqlMeta) -> Result<Value, DbError> {
        #[cfg(feature = "sql")]
        {
            let entry = self.sql_entry(connection_id).await?;
            let _permits = self.permits(&entry).await?;
            let limits = self.query_limits(None, None)?;
            let session = sql_backend(&entry)?;
            let value = match op {
                SqlMeta::Databases => serde_json::to_value(session.list_databases(&limits).await?)
                    .map_err(internal)?,
                SqlMeta::Schemas { database } => {
                    serde_json::to_value(session.list_schemas(database.as_deref(), &limits).await?)
                        .map_err(internal)?
                }
                SqlMeta::Tables {
                    schema,
                    include_views,
                } => serde_json::to_value(
                    session
                        .list_tables(schema.as_deref(), include_views, &limits)
                        .await?,
                )
                .map_err(internal)?,
                SqlMeta::Describe { schema, table } => {
                    session
                        .describe_table(schema.as_deref(), &table, &limits)
                        .await?
                }
                SqlMeta::Stats { schema, table } => {
                    session
                        .table_stats(schema.as_deref(), &table, &limits)
                        .await?
                }
                SqlMeta::Explain { sql, params } => {
                    session.explain(&sql, params.as_deref(), &limits).await?
                }
            };
            Ok(value)
        }
        #[cfg(not(feature = "sql"))]
        {
            let _ = (connection_id, op);
            Err(DbError::unsupported(
                "this build does not include SQL tools",
            ))
        }
    }

    pub async fn sql_execute(
        &self,
        connection_id: &str,
        sql: &str,
        params: Option<&[Value]>,
        transaction_id: Option<&str>,
        allow_destructive: bool,
        timeout_ms: Option<u64>,
    ) -> Result<Value, DbError> {
        #[cfg(feature = "sql")]
        {
            let entry = self.sql_entry(connection_id).await?;
            self.config.ensure_write(entry.read_only)?;
            let _permits = self.permits(&entry).await?;
            let limits = self.query_limits(None, timeout_ms)?;
            let output = sql_backend(&entry)?
                .execute(
                    sql,
                    params,
                    transaction_id,
                    allow_destructive,
                    self.config.allow_destructive,
                    &limits,
                )
                .await?;
            Ok(json!({"rows_affected": output.rows_affected, "status": "ok"}))
        }
        #[cfg(not(feature = "sql"))]
        {
            let _ = (
                connection_id,
                sql,
                params,
                transaction_id,
                allow_destructive,
                timeout_ms,
            );
            Err(DbError::unsupported(
                "this build does not include SQL tools",
            ))
        }
    }

    pub async fn sql_begin(
        &self,
        connection_id: &str,
        read_only: Option<bool>,
    ) -> Result<Value, DbError> {
        #[cfg(feature = "sql")]
        {
            let entry = self.sql_entry(connection_id).await?;
            let read_only = read_only.unwrap_or(entry.read_only);
            if !read_only {
                self.config.ensure_write(entry.read_only)?;
            }
            let id = sql_backend(&entry)?.begin(read_only).await?;
            Ok(
                json!({"transaction_id": id, "connection_id": connection_id, "read_only": read_only}),
            )
        }
        #[cfg(not(feature = "sql"))]
        {
            let _ = (connection_id, read_only);
            Err(DbError::unsupported(
                "this build does not include SQL tools",
            ))
        }
    }

    pub async fn sql_finish(&self, transaction_id: &str, commit: bool) -> Result<Value, DbError> {
        #[cfg(feature = "sql")]
        {
            let guard = self.connections.read().await;
            for entry in guard.values() {
                if let Backend::Sql(session) = &entry.backend {
                    let result = if commit {
                        session.commit(transaction_id).await
                    } else {
                        session.rollback(transaction_id).await
                    };
                    match result {
                        Ok(()) => {
                            return Ok(json!({
                                "transaction_id": transaction_id,
                                "status": if commit { "committed" } else { "rolled_back" },
                            }))
                        }
                        Err(err) if err.code == ErrorCode::TransactionNotFound => continue,
                        Err(err) => return Err(err),
                    }
                }
            }
            Err(DbError::new(
                ErrorCode::TransactionNotFound,
                "transaction was not found",
            ))
        }
        #[cfg(not(feature = "sql"))]
        {
            let _ = (transaction_id, commit);
            Err(DbError::unsupported(
                "this build does not include SQL tools",
            ))
        }
    }

    pub async fn mongo(&self, connection_id: &str, op: MongoOp) -> Result<Value, DbError> {
        #[cfg(feature = "mongo")]
        {
            let entry = self.mongo_entry(connection_id).await?;
            let _permits = self.permits(&entry).await?;
            let limits = self.mongo_limits(None, None)?;
            let session = mongo_backend(&entry)?;
            match op {
                MongoOp::Databases => session.list_databases(&limits).await,
                MongoOp::Collections { database } => {
                    let database = session.database_or_default(database.as_deref())?;
                    session.list_collections(database, &limits).await
                }
                MongoOp::Describe {
                    database,
                    collection,
                } => {
                    let database = session.database_or_default(database.as_deref())?;
                    session
                        .describe_collection(database, &collection, &limits)
                        .await
                }
                MongoOp::Find {
                    database,
                    collection,
                    filter,
                    projection,
                    sort,
                    limit,
                    skip,
                } => {
                    let database = session.database_or_default(database.as_deref())?;
                    let limits = self.mongo_limits(limit, None)?;
                    session
                        .find(
                            database,
                            &collection,
                            &filter,
                            projection.as_ref(),
                            sort.as_ref(),
                            limits.max_docs,
                            skip,
                            &limits,
                        )
                        .await
                }
                MongoOp::Aggregate {
                    database,
                    collection,
                    pipeline,
                } => {
                    let database = session.database_or_default(database.as_deref())?;
                    session
                        .aggregate(
                            database,
                            &collection,
                            &pipeline,
                            self.config.allow_writes && !entry.read_only,
                            &limits,
                        )
                        .await
                }
                MongoOp::Insert {
                    database,
                    collection,
                    documents,
                } => {
                    self.config.ensure_write(entry.read_only)?;
                    let database = session.database_or_default(database.as_deref())?;
                    session
                        .insert(database, &collection, &documents, &limits)
                        .await
                }
                MongoOp::Update {
                    database,
                    collection,
                    filter,
                    update,
                    upsert,
                    multi,
                    allow_destructive,
                } => {
                    self.config.ensure_write(entry.read_only)?;
                    let database = session.database_or_default(database.as_deref())?;
                    session
                        .update(
                            database,
                            &collection,
                            &filter,
                            &update,
                            upsert,
                            multi,
                            allow_destructive,
                            self.config.allow_destructive,
                            &limits,
                        )
                        .await
                }
                MongoOp::Delete {
                    database,
                    collection,
                    filter,
                    multi,
                    allow_destructive,
                } => {
                    self.config.ensure_write(entry.read_only)?;
                    let database = session.database_or_default(database.as_deref())?;
                    session
                        .delete(
                            database,
                            &collection,
                            &filter,
                            multi,
                            allow_destructive,
                            self.config.allow_destructive,
                            &limits,
                        )
                        .await
                }
                MongoOp::Indexes {
                    database,
                    collection,
                } => {
                    let database = session.database_or_default(database.as_deref())?;
                    session.list_indexes(database, &collection, &limits).await
                }
            }
        }
        #[cfg(not(feature = "mongo"))]
        {
            let _ = (connection_id, op);
            Err(DbError::unsupported(
                "this build does not include MongoDB tools",
            ))
        }
    }

    pub async fn redis(&self, connection_id: &str, op: RedisOp) -> Result<Value, DbError> {
        #[cfg(feature = "redis")]
        {
            let entry = self.redis_entry(connection_id).await?;
            let _permits = self.permits(&entry).await?;
            let timeout = self.config.query_timeout;
            let session = redis_backend(&entry)?;
            match op {
                RedisOp::Scan {
                    cursor,
                    pattern,
                    count,
                } => {
                    session
                        .scan(&cursor, pattern.as_deref(), count.unwrap_or(100), timeout)
                        .await
                }
                RedisOp::KeyInfo { key } => session.key_info(&key, timeout).await,
                RedisOp::Get { key } => session.get(&key, timeout).await,
                RedisOp::Set {
                    key,
                    text,
                    base64,
                    ex,
                    px,
                    nx,
                    xx,
                } => {
                    self.config.ensure_write(entry.read_only)?;
                    session
                        .set(
                            &key,
                            text.as_deref(),
                            base64.as_deref(),
                            ex,
                            px,
                            nx,
                            xx,
                            timeout,
                        )
                        .await
                }
                RedisOp::Delete { keys, unlink } => {
                    self.config.ensure_write(entry.read_only)?;
                    session.delete(&keys, unlink, timeout).await
                }
                RedisOp::Hash {
                    action,
                    key,
                    field,
                    text,
                    base64,
                    cursor,
                    count,
                } => {
                    if matches!(action.as_str(), "hset" | "hdel") {
                        self.config.ensure_write(entry.read_only)?;
                    }
                    session
                        .hash(
                            &action,
                            &key,
                            field.as_deref(),
                            text.as_deref(),
                            base64.as_deref(),
                            cursor.as_deref(),
                            count,
                            timeout,
                        )
                        .await
                }
                RedisOp::List {
                    action,
                    key,
                    text,
                    base64,
                    start,
                    stop,
                } => {
                    if matches!(action.as_str(), "lpush" | "rpush" | "lpop" | "rpop") {
                        self.config.ensure_write(entry.read_only)?;
                    }
                    session
                        .list(
                            &action,
                            &key,
                            text.as_deref(),
                            base64.as_deref(),
                            start,
                            stop,
                            timeout,
                        )
                        .await
                }
                RedisOp::SetMembers {
                    action,
                    key,
                    text,
                    base64,
                    cursor,
                    count,
                } => {
                    if matches!(action.as_str(), "sadd" | "srem") {
                        self.config.ensure_write(entry.read_only)?;
                    }
                    session
                        .set_members(
                            &action,
                            &key,
                            text.as_deref(),
                            base64.as_deref(),
                            cursor.as_deref(),
                            count,
                            timeout,
                        )
                        .await
                }
                RedisOp::SortedSet {
                    action,
                    key,
                    member_text,
                    member_base64,
                    score,
                    start,
                    stop,
                } => {
                    if matches!(action.as_str(), "zadd" | "zrem") {
                        self.config.ensure_write(entry.read_only)?;
                    }
                    session
                        .sorted_set(
                            &action,
                            &key,
                            member_text.as_deref(),
                            member_base64.as_deref(),
                            score,
                            start,
                            stop,
                            timeout,
                        )
                        .await
                }
                RedisOp::Info => session.info(timeout).await,
            }
        }
        #[cfg(not(feature = "redis"))]
        {
            let _ = (connection_id, op);
            Err(DbError::unsupported(
                "this build does not include Redis tools",
            ))
        }
    }

    pub async fn reap(&self) {
        let stale: Vec<Arc<Entry>> = {
            let guard = self.connections.read().await;
            guard
                .values()
                .filter(|entry| {
                    entry
                        .last_used
                        .lock()
                        .unwrap_or_else(|err| err.into_inner())
                        .elapsed()
                        > self.config.idle_ttl
                })
                .cloned()
                .collect()
        };
        if !stale.is_empty() {
            let mut guard = self.connections.write().await;
            for entry in &stale {
                guard.remove(&entry.id);
            }
        }
        for entry in &stale {
            close_backend(&entry.backend).await;
        }
        #[cfg(feature = "sql")]
        {
            let guard = self.connections.read().await;
            for entry in guard.values() {
                if let Backend::Sql(session) = &entry.backend {
                    session
                        .reap_transactions(self.config.txn_idle_ttl, self.config.txn_max_lifetime)
                        .await;
                }
            }
        }
    }

    async fn connect_backend(
        &self,
        database_url: &str,
        target: &dbconnector_core::ParsedTarget,
        read_only: bool,
    ) -> Result<Backend, DbError> {
        match target.kind {
            DatabaseKind::Postgres | DatabaseKind::Mysql | DatabaseKind::Sqlite => {
                #[cfg(feature = "sql")]
                {
                    let dialect = match target.kind {
                        DatabaseKind::Postgres => SqlDialect::Postgres,
                        DatabaseKind::Mysql => SqlDialect::Mysql,
                        DatabaseKind::Sqlite => SqlDialect::Sqlite,
                        _ => unreachable!(),
                    };
                    let session = SqlSession::connect(
                        database_url,
                        dialect,
                        read_only,
                        self.config.sql_pool_max,
                    )
                    .await?;
                    Ok(Backend::Sql(session))
                }
                #[cfg(not(feature = "sql"))]
                {
                    let _ = (database_url, read_only);
                    Err(DbError::unsupported(
                        "this build does not include the SQL driver",
                    ))
                }
            }
            DatabaseKind::Mongodb => {
                #[cfg(feature = "mongo")]
                {
                    let session =
                        MongoSession::connect(database_url, read_only, target.database.clone())
                            .await?;
                    Ok(Backend::Mongo(session))
                }
                #[cfg(not(feature = "mongo"))]
                {
                    let _ = (database_url, read_only);
                    Err(DbError::unsupported(
                        "this build does not include the MongoDB driver",
                    ))
                }
            }
            DatabaseKind::Redis => {
                #[cfg(feature = "redis")]
                {
                    let session = RedisSession::connect(database_url, read_only).await?;
                    Ok(Backend::Redis(session))
                }
                #[cfg(not(feature = "redis"))]
                {
                    let _ = (database_url, read_only);
                    Err(DbError::unsupported(
                        "this build does not include the Redis driver",
                    ))
                }
            }
        }
    }

    async fn entry(&self, connection_id: &str) -> Result<Arc<Entry>, DbError> {
        self.connections
            .read()
            .await
            .get(connection_id)
            .cloned()
            .ok_or_else(|| DbError::new(ErrorCode::ConnectionNotFound, "connection was not found"))
    }

    #[cfg(feature = "sql")]
    async fn sql_entry(&self, connection_id: &str) -> Result<Arc<Entry>, DbError> {
        let entry = self.entry(connection_id).await?;
        if !matches!(
            entry.kind,
            DatabaseKind::Postgres | DatabaseKind::Mysql | DatabaseKind::Sqlite
        ) {
            return Err(DbError::unsupported(
                "SQL tools require a postgres, mysql, or sqlite connection",
            ));
        }
        touch(&entry);
        Ok(entry)
    }

    #[cfg(feature = "mongo")]
    async fn mongo_entry(&self, connection_id: &str) -> Result<Arc<Entry>, DbError> {
        let entry = self.entry(connection_id).await?;
        if entry.kind != DatabaseKind::Mongodb {
            return Err(DbError::unsupported(
                "MongoDB tools require a mongodb connection",
            ));
        }
        touch(&entry);
        Ok(entry)
    }

    #[cfg(feature = "redis")]
    async fn redis_entry(&self, connection_id: &str) -> Result<Arc<Entry>, DbError> {
        let entry = self.entry(connection_id).await?;
        if entry.kind != DatabaseKind::Redis {
            return Err(DbError::unsupported(
                "Redis tools require a redis or valkey connection",
            ));
        }
        touch(&entry);
        Ok(entry)
    }

    async fn permits(&self, entry: &Entry) -> Result<PermitGuard, DbError> {
        let timeout = self.config.query_timeout;
        let global = tokio::time::timeout(timeout, self.global.clone().acquire_owned())
            .await
            .map_err(|_| {
                DbError::new(
                    ErrorCode::ResourceLimitExceeded,
                    "timed out waiting for a global query slot",
                )
            })?
            .map_err(|_| DbError::new(ErrorCode::InternalError, "global semaphore is closed"))?;
        let local = tokio::time::timeout(timeout, entry.sem.clone().acquire_owned())
            .await
            .map_err(|_| {
                DbError::new(
                    ErrorCode::ResourceLimitExceeded,
                    "timed out waiting for a connection query slot",
                )
            })?
            .map_err(|_| {
                DbError::new(ErrorCode::InternalError, "connection semaphore is closed")
            })?;
        Ok(PermitGuard {
            _global: global,
            _local: local,
        })
    }

    #[cfg(feature = "sql")]
    fn query_limits(
        &self,
        rows: Option<u64>,
        timeout_ms: Option<u64>,
    ) -> Result<QueryLimits, DbError> {
        let timeout = match timeout_ms {
            Some(0) => return Err(DbError::invalid("timeout_ms must be greater than zero")),
            Some(ms) => Duration::from_millis(ms).min(self.config.query_timeout),
            None => self.config.query_timeout,
        };
        let max_rows = rows
            .unwrap_or(self.config.default_rows)
            .clamp(1, self.config.max_rows);
        Ok(QueryLimits {
            max_rows,
            max_bytes: self.config.max_result_bytes,
            timeout,
        })
    }

    #[cfg(feature = "mongo")]
    fn mongo_limits(
        &self,
        rows: Option<u64>,
        timeout_ms: Option<u64>,
    ) -> Result<MongoLimits, DbError> {
        let timeout = match timeout_ms {
            Some(0) => return Err(DbError::invalid("timeout_ms must be greater than zero")),
            Some(ms) => Duration::from_millis(ms).min(self.config.query_timeout),
            None => self.config.query_timeout,
        };
        Ok(MongoLimits {
            max_docs: rows
                .unwrap_or(self.config.default_rows)
                .clamp(1, self.config.max_rows),
            max_bytes: self.config.max_result_bytes,
            timeout,
        })
    }
}

struct PermitGuard {
    _global: tokio::sync::OwnedSemaphorePermit,
    _local: tokio::sync::OwnedSemaphorePermit,
}

pub enum SqlMeta {
    Databases,
    Schemas {
        database: Option<String>,
    },
    Tables {
        schema: Option<String>,
        include_views: bool,
    },
    Describe {
        schema: Option<String>,
        table: String,
    },
    Stats {
        schema: Option<String>,
        table: String,
    },
    Explain {
        sql: String,
        params: Option<Vec<Value>>,
    },
}

pub enum MongoOp {
    Databases,
    Collections {
        database: Option<String>,
    },
    Describe {
        database: Option<String>,
        collection: String,
    },
    Find {
        database: Option<String>,
        collection: String,
        filter: Value,
        projection: Option<Value>,
        sort: Option<Value>,
        limit: Option<u64>,
        skip: u64,
    },
    Aggregate {
        database: Option<String>,
        collection: String,
        pipeline: Vec<Value>,
    },
    Insert {
        database: Option<String>,
        collection: String,
        documents: Vec<Value>,
    },
    Update {
        database: Option<String>,
        collection: String,
        filter: Value,
        update: Value,
        upsert: bool,
        multi: bool,
        allow_destructive: bool,
    },
    Delete {
        database: Option<String>,
        collection: String,
        filter: Value,
        multi: bool,
        allow_destructive: bool,
    },
    Indexes {
        database: Option<String>,
        collection: String,
    },
}

pub enum RedisOp {
    Scan {
        cursor: String,
        pattern: Option<String>,
        count: Option<u64>,
    },
    KeyInfo {
        key: String,
    },
    Get {
        key: String,
    },
    Set {
        key: String,
        text: Option<String>,
        base64: Option<String>,
        ex: Option<u64>,
        px: Option<u64>,
        nx: bool,
        xx: bool,
    },
    Delete {
        keys: Vec<String>,
        unlink: bool,
    },
    Hash {
        action: String,
        key: String,
        field: Option<String>,
        text: Option<String>,
        base64: Option<String>,
        cursor: Option<String>,
        count: Option<u64>,
    },
    List {
        action: String,
        key: String,
        text: Option<String>,
        base64: Option<String>,
        start: Option<i64>,
        stop: Option<i64>,
    },
    SetMembers {
        action: String,
        key: String,
        text: Option<String>,
        base64: Option<String>,
        cursor: Option<String>,
        count: Option<u64>,
    },
    SortedSet {
        action: String,
        key: String,
        member_text: Option<String>,
        member_base64: Option<String>,
        score: Option<f64>,
        start: Option<i64>,
        stop: Option<i64>,
    },
    Info,
}

fn summary(entry: &Entry) -> Value {
    json!({
        "connection_id": entry.id,
        "kind": entry.kind.as_str(),
        "status": "connected",
        "read_only": entry.read_only,
        "host": entry.host,
        "database": entry.database,
        "created_at": entry.created_at,
    })
}

fn touch(entry: &Entry) {
    if let Ok(mut last) = entry.last_used.lock() {
        *last = Instant::now();
    }
}

fn capability_names(kind: DatabaseKind, read_only: bool, config: &ServerConfig) -> Vec<String> {
    let mut names = vec!["ping".into(), "metadata".into()];
    match kind {
        DatabaseKind::Postgres | DatabaseKind::Mysql | DatabaseKind::Sqlite => {
            names.extend([
                "schema".into(),
                "query".into(),
                "explain".into(),
                "transactions".into(),
            ]);
        }
        DatabaseKind::Mongodb => {
            names.extend(["find".into(), "aggregate".into(), "indexes".into()])
        }
        DatabaseKind::Redis => {
            names.extend(["scan".into(), "key_info".into(), "data_types".into()])
        }
    }
    if config.allow_writes && !read_only {
        names.push("writes".into());
    }
    if config.allow_destructive {
        names.push("destructive".into());
    }
    names.push("result_limits".into());
    names
}

fn limitations(kind: DatabaseKind) -> Vec<String> {
    match kind {
        DatabaseKind::Postgres => vec![
            "DDL may implicitly commit outside an explicit transaction on some engines; PostgreSQL transactional DDL is used when a transaction_id is supplied.".into(),
        ],
        DatabaseKind::Mysql => vec![
            "MySQL DDL causes an implicit commit and cannot be rolled back.".into(),
            "MariaDB uses the MySQL driver and metadata queries.".into(),
            "MySQL TEXT is returned by the SQL driver as bytes. Valid UTF-8 is tagged encoding=utf8; other bytes are base64.".into(),
        ],
        DatabaseKind::Sqlite => vec![
            "SQLite has no separate schema catalog; sql_list_schemas returns main.".into(),
            "A read-only transaction on a writable SQLite connection is enforced by the statement classifier.".into(),
        ],
        DatabaseKind::Mongodb => vec![
            "Multi-document transactions and change streams are not exposed.".into(),
            "mongo_describe_collection field types are inferred from a bounded sample.".into(),
        ],
        DatabaseKind::Redis => vec![
            "SCAN can return duplicates and can miss keys written during iteration.".into(),
            "Streams, pub/sub, EVAL, FLUSH, CONFIG, DEBUG, MODULE, and ACL commands are not exposed.".into(),
            "Cluster-wide commands are limited to what a single connection can route.".into(),
        ],
    }
}

fn backend_version(backend: &Backend) -> String {
    match backend {
        #[cfg(feature = "sql")]
        Backend::Sql(session) => session.server_version.clone(),
        #[cfg(feature = "mongo")]
        Backend::Mongo(session) => session.server_version.clone(),
        #[cfg(feature = "redis")]
        Backend::Redis(session) => format!("{} {}", session.product, session.server_version),
        Backend::Unsupported => "unknown".into(),
    }
}

#[cfg(feature = "sql")]
fn sql_backend(entry: &Entry) -> Result<&SqlSession, DbError> {
    match &entry.backend {
        Backend::Sql(session) => Ok(session),
        #[allow(unreachable_patterns)]
        _ => Err(DbError::unsupported("connection is not a SQL connection")),
    }
}

#[cfg(feature = "mongo")]
fn mongo_backend(entry: &Entry) -> Result<&MongoSession, DbError> {
    match &entry.backend {
        Backend::Mongo(session) => Ok(session),
        #[allow(unreachable_patterns)]
        _ => Err(DbError::unsupported(
            "connection is not a MongoDB connection",
        )),
    }
}

#[cfg(feature = "redis")]
fn redis_backend(entry: &Entry) -> Result<&RedisSession, DbError> {
    match &entry.backend {
        Backend::Redis(session) => Ok(session),
        #[allow(unreachable_patterns)]
        _ => Err(DbError::unsupported("connection is not a Redis connection")),
    }
}

async fn close_backend(backend: &Backend) {
    match backend {
        #[cfg(feature = "sql")]
        Backend::Sql(session) => session.close().await,
        #[cfg(feature = "mongo")]
        Backend::Mongo(_) => {}
        #[cfg(feature = "redis")]
        Backend::Redis(_) => {}
        Backend::Unsupported => {}
    }
}

fn internal(err: serde_json::Error) -> DbError {
    DbError::new(ErrorCode::InternalError, redact_secrets(&err.to_string()))
}

// Keep a named mutex import used when the compiler feature-gates the rest.
#[allow(dead_code)]
fn _mutex_marker(mutex: &Mutex<()>) {
    let _ = mutex;
}
