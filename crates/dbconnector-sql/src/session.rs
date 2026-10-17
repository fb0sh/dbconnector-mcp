use std::collections::HashMap;
use std::ops::DerefMut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex as StdMutex, Once};
use std::time::{Duration, Instant};

use dbconnector_core::{redact_secrets, DbError, ErrorCode};
use futures::TryStreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::any::{AnyArguments, AnyPoolOptions, AnyRow};
use sqlx::{Column, Executor, Pool, Row, TypeInfo, ValueRef};
use tokio::sync::Mutex;

use crate::classify::{ensure_execute, ensure_read};
use crate::dialect::SqlDialect;
use crate::value::{bind_args, cell_from_raw, parse_params, BoundValue};

static DRIVERS: Once = Once::new();

fn install_drivers() {
    DRIVERS.call_once(|| {
        sqlx::any::install_default_drivers();
    });
}

#[derive(Debug, Clone, Serialize)]
pub struct SqlColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SqlRows {
    pub columns: Vec<SqlColumn>,
    pub rows: Vec<Vec<Value>>,
    pub returned_rows: usize,
    pub truncated: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ExecOutput {
    pub rows_affected: u64,
}

pub struct QueryLimits {
    pub max_rows: u64,
    pub max_bytes: usize,
    pub timeout: Duration,
}

struct TxnSlot {
    started: Instant,
    last_used: StdMutex<Instant>,
    broken: AtomicBool,
    conn: Mutex<sqlx::pool::PoolConnection<sqlx::Any>>,
}

pub struct SqlSession {
    pub dialect: SqlDialect,
    pub read_only: bool,
    pub server_version: String,
    pool: Pool<sqlx::Any>,
    txns: Mutex<HashMap<String, std::sync::Arc<TxnSlot>>>,
}

impl SqlSession {
    pub async fn connect(
        database_url: &str,
        dialect: SqlDialect,
        read_only: bool,
        pool_max: u32,
    ) -> Result<Self, DbError> {
        install_drivers();
        let url = normalize_sqlite_url(database_url, dialect, read_only);
        let read_only_hook = read_only;
        let dialect_hook = dialect;
        let pool = AnyPoolOptions::new()
            .max_connections(pool_max.max(1))
            .acquire_timeout(Duration::from_secs(10))
            .idle_timeout(Duration::from_secs(30 * 60))
            .after_connect(move |conn, _meta| {
                Box::pin(async move {
                    if read_only_hook {
                        let sql = match dialect_hook {
                            SqlDialect::Postgres => "SET default_transaction_read_only = on",
                            SqlDialect::Mysql => "SET SESSION TRANSACTION READ ONLY",
                            SqlDialect::Sqlite => "PRAGMA query_only = ON",
                        };
                        sqlx::query(sql).execute(&mut *conn).await?;
                    }
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .map_err(connect_err)?;
        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map_err(connect_err)?;
        let server_version = read_version(&pool, dialect)
            .await
            .unwrap_or_else(|_| "unknown".into());
        Ok(Self {
            dialect,
            read_only,
            server_version,
            pool,
            txns: Mutex::new(HashMap::new()),
        })
    }

    pub async fn list_databases(&self, limits: &QueryLimits) -> Result<SqlRows, DbError> {
        let sql = match self.dialect {
            SqlDialect::Postgres => {
                // `datname` is of type `name`, which the sqlx `Any` driver cannot decode.
                "SELECT datname::text AS name FROM pg_database WHERE datallowconn ORDER BY datname"
            }
            SqlDialect::Mysql => {
                // `SHOW DATABASES` returns a `Database` column and cannot be aliased;
                // the catalog view returns a lowercase, CHAR-cast column instead.
                "SELECT CAST(schema_name AS CHAR) AS name FROM information_schema.schemata \
                 ORDER BY schema_name"
            }
            SqlDialect::Sqlite => "PRAGMA database_list",
        };
        self.query_meta(sql, None, None, limits).await
    }

    pub async fn list_schemas(
        &self,
        database: Option<&str>,
        limits: &QueryLimits,
    ) -> Result<SqlRows, DbError> {
        match self.dialect {
            SqlDialect::Sqlite => Ok(literal_rows(
                vec![col("schema", "text")],
                vec![vec![Value::String("main".into())]],
            )),
            SqlDialect::Postgres => {
                let filter = database.unwrap_or("");
                self.query_meta(
                    "SELECT schema_name::text AS name FROM information_schema.schemata \
                     WHERE schema_name NOT IN ('pg_catalog', 'information_schema') \
                     AND ($1 = '' OR schema_name = $1) ORDER BY schema_name",
                    Some(&[Value::String(filter.to_string())]),
                    None,
                    limits,
                )
                .await
            }
            SqlDialect::Mysql => {
                let filter = database.unwrap_or("");
                self.query_meta(
                    "SELECT CAST(schema_name AS CHAR) AS name FROM information_schema.schemata \
                     WHERE schema_name NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys') \
                     AND (? = '' OR schema_name = ?) ORDER BY schema_name",
                    Some(&[
                        Value::String(filter.to_string()),
                        Value::String(filter.to_string()),
                    ]),
                    None,
                    limits,
                )
                .await
            }
        }
    }

    pub async fn list_tables(
        &self,
        schema: Option<&str>,
        include_views: bool,
        limits: &QueryLimits,
    ) -> Result<SqlRows, DbError> {
        match self.dialect {
            SqlDialect::Postgres => {
                let schema = schema.unwrap_or("");
                self.query_meta(
                    "SELECT table_schema::text AS table_schema, table_name::text AS table_name, \
                     table_type::text AS table_type FROM information_schema.tables \
                     WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
                     AND ($1 = '' OR table_schema = $1) \
                     AND ($2 OR table_type = 'BASE TABLE') \
                     ORDER BY table_schema, table_name",
                    Some(&[
                        Value::String(schema.to_string()),
                        Value::Bool(include_views),
                    ]),
                    None,
                    limits,
                )
                .await
            }
            SqlDialect::Mysql => {
                let schema = schema.unwrap_or("");
                self.query_meta(
                    "SELECT CAST(table_schema AS CHAR) AS table_schema, \
                     CAST(table_name AS CHAR) AS table_name, CAST(table_type AS CHAR) AS table_type \
                     FROM information_schema.tables \
                     WHERE table_schema NOT IN ('information_schema', 'mysql', 'performance_schema', 'sys') \
                     AND (? = '' OR table_schema = ?) \
                     AND (? OR table_type = 'BASE TABLE') \
                     ORDER BY table_schema, table_name",
                    Some(&[
                        Value::String(schema.to_string()),
                        Value::String(schema.to_string()),
                        Value::Bool(include_views),
                    ]),
                    None,
                    limits,
                )
                .await
            }
            SqlDialect::Sqlite => {
                if let Some(schema) = schema {
                    if !schema.is_empty() && !schema.eq_ignore_ascii_case("main") {
                        return Ok(literal_rows(
                            vec![
                                col("table_schema", "text"),
                                col("table_name", "text"),
                                col("table_type", "text"),
                            ],
                            Vec::new(),
                        ));
                    }
                }
                self.query_meta(
                    "SELECT 'main' AS table_schema, name AS table_name, type AS table_type \
                     FROM sqlite_schema WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' \
                     AND (? OR type = 'table') ORDER BY name",
                    Some(&[Value::Bool(include_views)]),
                    None,
                    limits,
                )
                .await
            }
        }
    }

    pub async fn describe_table(
        &self,
        schema: Option<&str>,
        table: &str,
        limits: &QueryLimits,
    ) -> Result<Value, DbError> {
        validate_ident(table)?;
        let schema = self.resolve_schema(schema, limits).await?;
        let column_params = vec![
            Value::String(schema.clone()),
            Value::String(table.to_string()),
        ];
        let columns = self
            .query_read(
                match self.dialect {
                    SqlDialect::Postgres => {
                        "SELECT column_name::text AS column_name, data_type::text AS data_type, \
                         is_nullable::text AS is_nullable, column_default::text AS column_default \
                         FROM information_schema.columns \
                         WHERE table_schema = $1 AND table_name = $2 \
                         ORDER BY ordinal_position"
                    }
                    SqlDialect::Mysql => {
                        "SELECT CAST(column_name AS CHAR) AS column_name, \
                         CAST(data_type AS CHAR) AS data_type, \
                         CAST(is_nullable AS CHAR) AS is_nullable, \
                         CAST(column_default AS CHAR) AS column_default \
                         FROM information_schema.columns \
                         WHERE table_schema = ? AND table_name = ? \
                         ORDER BY ordinal_position"
                    }
                    SqlDialect::Sqlite => "SELECT 1 WHERE 0",
                },
                if self.dialect == SqlDialect::Sqlite {
                    None
                } else {
                    Some(column_params.as_slice())
                },
                None,
                limits,
            )
            .await?;
        let columns = if self.dialect == SqlDialect::Sqlite {
            self.query_meta(
                &format!("PRAGMA table_info({})", quote_ident(table, self.dialect)?),
                None,
                None,
                limits,
            )
            .await?
        } else {
            columns
        };
        let primary_key = self.primary_key(&schema, table, limits).await?;
        let foreign_keys = self.foreign_keys(&schema, table, limits).await?;
        let indexes = self.indexes(&schema, table, limits).await?;
        Ok(json!({
            "schema": schema,
            "table": table,
            "columns": rows_as_objects(&columns),
            "primary_key": primary_key,
            "foreign_keys": foreign_keys,
            "indexes": indexes,
        }))
    }

    pub async fn table_stats(
        &self,
        schema: Option<&str>,
        table: &str,
        limits: &QueryLimits,
    ) -> Result<Value, DbError> {
        validate_ident(table)?;
        let schema = self.resolve_schema(schema, limits).await?;
        match self.dialect {
            SqlDialect::Postgres => {
                let rows = self
                    .query_read(
                        "SELECT c.reltuples::bigint AS row_estimate, \
                         pg_total_relation_size(c.oid) AS total_bytes \
                         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                         WHERE n.nspname = $1 AND c.relname = $2",
                        Some(&[
                            Value::String(schema.clone()),
                            Value::String(table.to_string()),
                        ]),
                        None,
                        limits,
                    )
                    .await?;
                Ok(json!({
                    "schema": schema,
                    "table": table,
                    "stats": rows_as_objects(&rows),
                    "source": "pg_class",
                    "approximate": true,
                }))
            }
            SqlDialect::Mysql => {
                let rows = self
                    .query_read(
                        // `table_rows` and the length columns arrive as DECIMAL on MySQL 8.4,
                        // which the sqlx `Any` driver cannot decode.
                        "SELECT CAST(table_rows AS SIGNED) AS row_estimate, \
                         CAST(COALESCE(data_length, 0) + COALESCE(index_length, 0) AS SIGNED) AS total_bytes \
                         FROM information_schema.tables WHERE table_schema = ? AND table_name = ?",
                        Some(&[
                            Value::String(schema.clone()),
                            Value::String(table.to_string()),
                        ]),
                        None,
                        limits,
                    )
                    .await?;
                Ok(json!({
                    "schema": schema,
                    "table": table,
                    "stats": rows_as_objects(&rows),
                    "source": "information_schema",
                    "approximate": true,
                }))
            }
            SqlDialect::Sqlite => {
                let quoted = quote_ident(table, SqlDialect::Sqlite)?;
                let rows = self
                    .query_read(
                        &format!("SELECT COUNT(*) AS row_estimate FROM {quoted}"),
                        None,
                        None,
                        limits,
                    )
                    .await?;
                Ok(json!({
                    "schema": schema,
                    "table": table,
                    "stats": rows_as_objects(&rows),
                    "source": "sqlite_count",
                    "approximate": false,
                }))
            }
        }
    }

    pub async fn query_read(
        &self,
        sql: &str,
        params: Option<&[Value]>,
        transaction_id: Option<&str>,
        limits: &QueryLimits,
    ) -> Result<SqlRows, DbError> {
        ensure_read(self.dialect, sql)?;
        let bound = parse_params(params)?;
        self.fetch(sql, &bound, transaction_id, limits, true).await
    }

    /// Server-authored metadata SQL. Skips the user-statement classifier so
    /// SQLite PRAGMA introspection can run. Callers must not pass user SQL.
    async fn query_meta(
        &self,
        sql: &str,
        params: Option<&[Value]>,
        transaction_id: Option<&str>,
        limits: &QueryLimits,
    ) -> Result<SqlRows, DbError> {
        let bound = parse_params(params)?;
        self.fetch(sql, &bound, transaction_id, limits, false).await
    }

    pub async fn execute(
        &self,
        sql: &str,
        params: Option<&[Value]>,
        transaction_id: Option<&str>,
        allow_destructive: bool,
        server_allows_destructive: bool,
        limits: &QueryLimits,
    ) -> Result<ExecOutput, DbError> {
        if self.read_only {
            return Err(DbError::permission(
                "this connection is read-only; open a new connection with read_only=false",
            ));
        }
        let destructive = ensure_execute(self.dialect, sql)?;
        if destructive && !(allow_destructive && server_allows_destructive) {
            return Err(DbError::permission(
                "destructive SQL requires --allow-destructive and allow_destructive=true",
            ));
        }
        let bound = parse_params(params)?;
        let args = bind_args(&bound)?;
        let timeout = limits.timeout;
        let fut = self.execute_on(sql, args, transaction_id);
        match tokio::time::timeout(timeout, fut).await {
            Ok(result) => result,
            Err(_) => {
                if let Some(id) = transaction_id {
                    self.mark_broken(id).await;
                }
                Err(DbError::new(
                    ErrorCode::QueryTimeout,
                    "statement exceeded the timeout; execution state may be unknown",
                ))
            }
        }
    }

    pub async fn explain(
        &self,
        sql: &str,
        params: Option<&[Value]>,
        limits: &QueryLimits,
    ) -> Result<Value, DbError> {
        ensure_read(self.dialect, sql)?;
        let statement = crate::classify::parse_one(self.dialect, sql)?;
        let plan_sql = if matches!(statement, sqlparser::ast::Statement::Explain { .. }) {
            sql.to_string()
        } else {
            let body = sql.trim().trim_end_matches(';');
            match self.dialect {
                // FORMAT JSON returns a `json` column, which the sqlx `Any` driver cannot
                // decode. The text plan is returned as one row per plan line instead.
                SqlDialect::Postgres => format!("EXPLAIN (FORMAT TEXT) {body}"),
                SqlDialect::Mysql => format!("EXPLAIN {body}"),
                SqlDialect::Sqlite => format!("EXPLAIN QUERY PLAN {body}"),
            }
        };
        let bound = parse_params(params)?;
        let rows = self.fetch(&plan_sql, &bound, None, limits, false).await?;
        Ok(json!({
            "engine": self.dialect.as_str(),
            "executed": false,
            "plan": rows,
        }))
    }

    pub async fn begin(&self, read_only: bool) -> Result<String, DbError> {
        if self.read_only && !read_only {
            return Err(DbError::permission(
                "cannot start a writable transaction on a read-only connection",
            ));
        }
        let mut conn = self.pool.acquire().await.map_err(query_err)?;
        let begin_sql = match (self.dialect, read_only || self.read_only) {
            (SqlDialect::Postgres, true) => "BEGIN READ ONLY",
            (SqlDialect::Mysql, true) => "START TRANSACTION READ ONLY",
            (SqlDialect::Mysql, false) => "START TRANSACTION",
            (_, _) => "BEGIN",
        };
        // MySQL rejects `BEGIN`/`START TRANSACTION`/`COMMIT`/`ROLLBACK` in the
        // prepared-statement protocol (error 1295). Executing a `&str` instead of a
        // `sqlx::Query` uses the driver's simple query path, which MySQL accepts.
        (&mut *conn).execute(begin_sql).await.map_err(query_err)?;
        let id = dbconnector_core::new_transaction_id();
        let slot = std::sync::Arc::new(TxnSlot {
            started: Instant::now(),
            last_used: StdMutex::new(Instant::now()),
            broken: AtomicBool::new(false),
            conn: Mutex::new(conn),
        });
        self.txns.lock().await.insert(id.clone(), slot);
        Ok(id)
    }

    pub async fn commit(&self, transaction_id: &str) -> Result<(), DbError> {
        self.finish(transaction_id, true).await
    }

    pub async fn rollback(&self, transaction_id: &str) -> Result<(), DbError> {
        self.finish(transaction_id, false).await
    }

    pub async fn close(&self) {
        let ids: Vec<String> = self.txns.lock().await.keys().cloned().collect();
        for id in ids {
            let _ = self.rollback(&id).await;
        }
        self.pool.close().await;
    }

    pub async fn reap_transactions(&self, idle: Duration, max_lifetime: Duration) {
        let expired: Vec<String> = {
            let guard = self.txns.lock().await;
            guard
                .iter()
                .filter_map(|(id, slot)| {
                    let last = slot.last_used.lock().unwrap_or_else(|err| err.into_inner());
                    let idle_too_long = last.elapsed() > idle;
                    drop(last);
                    if idle_too_long
                        || slot.started.elapsed() > max_lifetime
                        || slot.broken.load(Ordering::Relaxed)
                    {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect()
        };
        for id in expired {
            let _ = self.rollback(&id).await;
        }
    }

    async fn resolve_schema(
        &self,
        schema: Option<&str>,
        limits: &QueryLimits,
    ) -> Result<String, DbError> {
        if let Some(schema) = schema {
            if !schema.is_empty() {
                validate_ident(schema)?;
                return Ok(schema.to_string());
            }
        }
        match self.dialect {
            SqlDialect::Sqlite => Ok("main".into()),
            SqlDialect::Postgres => {
                let rows = self
                    .query_read("SELECT current_schema()::text AS name", None, None, limits)
                    .await?;
                Ok(rows
                    .rows
                    .first()
                    .and_then(|row| row.first())
                    .and_then(Value::as_str)
                    .unwrap_or("public")
                    .to_string())
            }
            SqlDialect::Mysql => {
                let rows = self
                    .query_read("SELECT DATABASE() AS name", None, None, limits)
                    .await?;
                Ok(rows
                    .rows
                    .first()
                    .and_then(|row| row.first())
                    .and_then(Value::as_str)
                    .unwrap_or("mysql")
                    .to_string())
            }
        }
    }

    async fn primary_key(
        &self,
        schema: &str,
        table: &str,
        limits: &QueryLimits,
    ) -> Result<Vec<Value>, DbError> {
        let rows = match self.dialect {
            SqlDialect::Postgres => {
                self.query_meta(
                    "SELECT kcu.column_name::text AS column_name FROM information_schema.table_constraints tc \
                     JOIN information_schema.key_column_usage kcu \
                       ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
                     WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = $1 AND tc.table_name = $2 \
                     ORDER BY kcu.ordinal_position",
                    Some(&[Value::String(schema.into()), Value::String(table.into())]),
                    None,
                    limits,
                )
                .await?
            }
            SqlDialect::Mysql => {
                self.query_meta(
                    "SELECT CAST(column_name AS CHAR) AS column_name FROM information_schema.key_column_usage \
                     WHERE constraint_name = 'PRIMARY' AND table_schema = ? AND table_name = ? \
                     ORDER BY ordinal_position",
                    Some(&[Value::String(schema.into()), Value::String(table.into())]),
                    None,
                    limits,
                )
                .await?
            }
            SqlDialect::Sqlite => {
                self.query_meta(
                    &format!("PRAGMA table_info({})", quote_ident(table, self.dialect)?),
                    None,
                    None,
                    limits,
                )
                .await?
            }
        };
        if self.dialect == SqlDialect::Sqlite {
            let mut keys = Vec::new();
            for object in rows_as_objects(&rows) {
                let pk = object.get("pk").and_then(Value::as_i64).unwrap_or(0);
                if pk > 0 {
                    if let Some(name) = object.get("name").cloned() {
                        keys.push(name);
                    }
                }
            }
            return Ok(keys);
        }
        Ok(rows
            .rows
            .into_iter()
            .filter_map(|row| row.into_iter().next())
            .collect())
    }

    async fn foreign_keys(
        &self,
        schema: &str,
        table: &str,
        limits: &QueryLimits,
    ) -> Result<Value, DbError> {
        let rows = match self.dialect {
            SqlDialect::Postgres => self.query_meta(
                "SELECT tc.constraint_name::text AS constraint_name, kcu.column_name::text AS column_name, \
                 ccu.table_schema::text AS ref_schema, ccu.table_name::text AS ref_table, \
                 ccu.column_name::text AS ref_column \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
                 JOIN information_schema.constraint_column_usage ccu \
                   ON ccu.constraint_name = tc.constraint_name AND ccu.table_schema = tc.table_schema \
                 WHERE tc.constraint_type = 'FOREIGN KEY' AND tc.table_schema = $1 AND tc.table_name = $2",
                Some(&[Value::String(schema.into()), Value::String(table.into())]),
                None,
                limits,
            ).await?,
            SqlDialect::Mysql => self.query_meta(
                "SELECT CAST(constraint_name AS CHAR) AS constraint_name, \
                 CAST(column_name AS CHAR) AS column_name, \
                 CAST(referenced_table_schema AS CHAR) AS ref_schema, \
                 CAST(referenced_table_name AS CHAR) AS ref_table, \
                 CAST(referenced_column_name AS CHAR) AS ref_column \
                 FROM information_schema.key_column_usage \
                 WHERE referenced_table_name IS NOT NULL AND table_schema = ? AND table_name = ?",
                Some(&[Value::String(schema.into()), Value::String(table.into())]),
                None,
                limits,
            ).await?,
            SqlDialect::Sqlite => self.query_meta(
                &format!("PRAGMA foreign_key_list({})", quote_ident(table, self.dialect)?),
                None,
                None,
                limits,
            ).await?,
        };
        Ok(Value::Array(rows_as_objects(&rows)))
    }

    async fn indexes(
        &self,
        schema: &str,
        table: &str,
        limits: &QueryLimits,
    ) -> Result<Value, DbError> {
        let rows = match self.dialect {
            SqlDialect::Postgres => {
                self.query_meta(
                    "SELECT indexname::text AS name, indexdef::text AS definition FROM pg_indexes \
                 WHERE schemaname = $1 AND tablename = $2 ORDER BY indexname",
                    Some(&[Value::String(schema.into()), Value::String(table.into())]),
                    None,
                    limits,
                )
                .await?
            }
            SqlDialect::Mysql => {
                self.query_meta(
                    "SELECT CAST(index_name AS CHAR) AS name, \
                 CAST(GROUP_CONCAT(column_name ORDER BY seq_in_index) AS CHAR) AS columns, \
                 MIN(non_unique) = 0 AS is_unique \
                 FROM information_schema.statistics WHERE table_schema = ? AND table_name = ? \
                 GROUP BY index_name ORDER BY index_name",
                    Some(&[Value::String(schema.into()), Value::String(table.into())]),
                    None,
                    limits,
                )
                .await?
            }
            SqlDialect::Sqlite => {
                self.query_meta(
                    &format!("PRAGMA index_list({})", quote_ident(table, self.dialect)?),
                    None,
                    None,
                    limits,
                )
                .await?
            }
        };
        Ok(Value::Array(rows_as_objects(&rows)))
    }

    async fn execute_on(
        &self,
        sql: &str,
        args: AnyArguments<'_>,
        transaction_id: Option<&str>,
    ) -> Result<ExecOutput, DbError> {
        if let Some(id) = transaction_id {
            let slot = self.txn(id).await?;
            self.touch(&slot);
            let mut conn = slot.conn.try_lock().map_err(|_| {
                DbError::new(
                    ErrorCode::TransactionConflict,
                    "transaction is already executing a statement",
                )
            })?;
            let result = sqlx::query_with::<sqlx::Any, _>(sql, args)
                .execute(conn.deref_mut().deref_mut())
                .await
                .map_err(query_err)?;
            Ok(ExecOutput {
                rows_affected: result.rows_affected(),
            })
        } else {
            let mut conn = self.pool.acquire().await.map_err(query_err)?;
            let result = sqlx::query_with::<sqlx::Any, _>(sql, args)
                .execute(conn.deref_mut())
                .await
                .map_err(query_err)?;
            Ok(ExecOutput {
                rows_affected: result.rows_affected(),
            })
        }
    }

    async fn fetch(
        &self,
        sql: &str,
        params: &[BoundValue],
        transaction_id: Option<&str>,
        limits: &QueryLimits,
        _classified: bool,
    ) -> Result<SqlRows, DbError> {
        let args = bind_args(params)?;
        let timeout = limits.timeout;
        let max_rows = limits.max_rows;
        let max_bytes = limits.max_bytes;
        let fut = async {
            if let Some(id) = transaction_id {
                let slot = self.txn(id).await?;
                if slot.broken.load(Ordering::Relaxed) {
                    return Err(DbError::new(
                        ErrorCode::TransactionConflict,
                        "transaction is broken after a timeout or driver error and must be rolled back",
                    ));
                }
                self.touch(&slot);
                let mut conn = slot.conn.try_lock().map_err(|_| {
                    DbError::new(
                        ErrorCode::TransactionConflict,
                        "transaction is already executing a statement",
                    )
                })?;
                fetch_conn(conn.deref_mut().deref_mut(), sql, args, max_rows, max_bytes).await
            } else {
                let mut conn = self.pool.acquire().await.map_err(query_err)?;
                fetch_conn(conn.deref_mut(), sql, args, max_rows, max_bytes).await
            }
        };
        match tokio::time::timeout(timeout, fut).await {
            Ok(result) => result,
            Err(_) => {
                if let Some(id) = transaction_id {
                    self.mark_broken(id).await;
                }
                Err(DbError::new(
                    ErrorCode::QueryTimeout,
                    "query exceeded the timeout; execution state may be unknown",
                ))
            }
        }
    }

    async fn txn(&self, id: &str) -> Result<std::sync::Arc<TxnSlot>, DbError> {
        self.txns.lock().await.get(id).cloned().ok_or_else(|| {
            DbError::new(ErrorCode::TransactionNotFound, "transaction was not found")
        })
    }

    fn touch(&self, slot: &TxnSlot) {
        if let Ok(mut last) = slot.last_used.lock() {
            *last = Instant::now();
        }
    }

    async fn mark_broken(&self, id: &str) {
        if let Some(slot) = self.txns.lock().await.get(id) {
            slot.broken.store(true, Ordering::Relaxed);
        }
    }

    async fn finish(&self, transaction_id: &str, commit: bool) -> Result<(), DbError> {
        let slot = {
            let mut guard = self.txns.lock().await;
            guard.remove(transaction_id).ok_or_else(|| {
                DbError::new(ErrorCode::TransactionNotFound, "transaction was not found")
            })?
        };
        let mut conn = slot.conn.try_lock().map_err(|_| {
            DbError::new(
                ErrorCode::TransactionConflict,
                "transaction is already executing a statement",
            )
        })?;
        let sql = if commit && !slot.broken.load(Ordering::Relaxed) {
            "COMMIT"
        } else {
            "ROLLBACK"
        };
        let result = conn
            .deref_mut()
            .deref_mut()
            .execute(sql)
            .await
            .map_err(query_err);
        if commit && slot.broken.load(Ordering::Relaxed) {
            return Err(DbError::new(
                ErrorCode::TransactionConflict,
                "transaction was rolled back because an earlier statement timed out or failed",
            ));
        }
        result.map(|_| ())
    }
}

async fn fetch_conn<'e, E>(
    conn: E,
    sql: &str,
    args: AnyArguments<'_>,
    max_rows: u64,
    max_bytes: usize,
) -> Result<SqlRows, DbError>
where
    E: Executor<'e, Database = sqlx::Any>,
{
    let mut stream = sqlx::query_with::<sqlx::Any, _>(sql, args).fetch(conn);
    let mut columns: Vec<SqlColumn> = Vec::new();
    let mut rows = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    let mut warnings = Vec::new();
    while let Some(row) = stream.try_next().await.map_err(query_err)? {
        if columns.is_empty() {
            columns = (0..row.len())
                .map(|i| SqlColumn {
                    name: row.column(i).name().to_string(),
                    type_name: row.column(i).type_info().name().to_string(),
                })
                .collect();
        }
        if rows.len() as u64 >= max_rows {
            truncated = true;
            warnings.push(format!(
                "row limit of {max_rows} reached; additional rows were discarded"
            ));
            break;
        }
        let decoded = decode_row(&row);
        let encoded = serde_json::to_vec(&decoded).unwrap_or_default();
        if bytes + encoded.len() > max_bytes {
            if rows.is_empty() {
                return Err(DbError::new(
                    ErrorCode::ResourceLimitExceeded,
                    "first result row exceeds the response byte limit",
                ));
            }
            truncated = true;
            warnings.push("response byte limit reached; additional rows were discarded".into());
            break;
        }
        bytes += encoded.len();
        rows.push(decoded);
    }
    let returned_rows = rows.len();
    Ok(SqlRows {
        columns,
        rows,
        returned_rows,
        truncated,
        warnings,
    })
}

fn decode_row(row: &AnyRow) -> Vec<Value> {
    (0..row.len()).map(|i| decode_cell(row, i)).collect()
}

fn decode_cell(row: &AnyRow, index: usize) -> Value {
    let type_name = row.column(index).type_info().name().to_string();
    if let Ok(raw) = row.try_get_raw(index) {
        if ValueRef::is_null(&raw) {
            return Value::Null;
        }
    }
    let boolean = row.try_get::<bool, _>(index).ok();
    let integer = row
        .try_get::<i64, _>(index)
        .ok()
        .or_else(|| row.try_get::<i32, _>(index).ok().map(i64::from));
    let float = row.try_get::<f64, _>(index).ok();
    let text = row.try_get::<String, _>(index).ok();
    let bytes = row.try_get::<Vec<u8>, _>(index).ok();
    let json = text.as_deref().and_then(|text| {
        if type_name.to_ascii_lowercase().contains("json") {
            serde_json::from_str(text).ok()
        } else {
            None
        }
    });
    cell_from_raw(&type_name, text, bytes, json, integer, float, boolean)
}

async fn read_version(pool: &Pool<sqlx::Any>, dialect: SqlDialect) -> Result<String, DbError> {
    let sql = match dialect {
        SqlDialect::Postgres => "SELECT version()",
        SqlDialect::Mysql => "SELECT VERSION()",
        SqlDialect::Sqlite => "SELECT sqlite_version()",
    };
    let row = sqlx::query(sql).fetch_one(pool).await.map_err(query_err)?;
    row.try_get::<String, _>(0).map_err(query_err)
}

fn connect_err(err: sqlx::Error) -> DbError {
    DbError::new(
        ErrorCode::ConnectionFailed,
        redact_secrets(&err.to_string()),
    )
}

fn query_err(err: sqlx::Error) -> DbError {
    let message = redact_secrets(&err.to_string());
    let code = if message.to_ascii_lowercase().contains("timeout") {
        ErrorCode::QueryTimeout
    } else {
        ErrorCode::QueryFailed
    };
    DbError::new(code, message)
}

fn normalize_sqlite_url(url: &str, dialect: SqlDialect, read_only: bool) -> String {
    if dialect != SqlDialect::Sqlite {
        return url.to_string();
    }
    let normalized = match url.trim() {
        "sqlite://:memory:" | "sqlite::memory:" | "sqlite:///:memory:" => {
            "sqlite::memory:".to_string()
        }
        other => other.to_string(),
    };
    if normalized.contains(":memory:") || normalized.contains("mode=") {
        return normalized;
    }
    let mode = if read_only { "ro" } else { "rwc" };
    if normalized.contains('?') {
        format!("{normalized}&mode={mode}")
    } else {
        format!("{normalized}?mode={mode}")
    }
}

fn validate_ident(name: &str) -> Result<(), DbError> {
    if name.is_empty() || name.len() > 128 || name.contains('\0') {
        return Err(DbError::invalid("identifier is empty or too long"));
    }
    Ok(())
}

fn quote_ident(name: &str, dialect: SqlDialect) -> Result<String, DbError> {
    validate_ident(name)?;
    match dialect {
        SqlDialect::Mysql => Ok(format!("`{}`", name.replace('`', "``"))),
        SqlDialect::Postgres | SqlDialect::Sqlite => {
            Ok(format!("\"{}\"", name.replace('"', "\"\"")))
        }
    }
}

fn col(name: &str, type_name: &str) -> SqlColumn {
    SqlColumn {
        name: name.to_string(),
        type_name: type_name.to_string(),
    }
}

fn literal_rows(columns: Vec<SqlColumn>, rows: Vec<Vec<Value>>) -> SqlRows {
    let returned_rows = rows.len();
    SqlRows {
        columns,
        rows,
        returned_rows,
        truncated: false,
        warnings: Vec::new(),
    }
}

fn rows_as_objects(rows: &SqlRows) -> Vec<Value> {
    rows.rows
        .iter()
        .map(|row| {
            let mut object = serde_json::Map::new();
            for (column, value) in rows.columns.iter().zip(row.iter()) {
                object.insert(column.name.clone(), value.clone());
            }
            Value::Object(object)
        })
        .collect()
}
