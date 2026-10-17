use std::borrow::Cow;
use std::sync::Arc;
use std::time::Instant;

use dbconnector_core::{DbError, ServerConfig};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::state::{AppState, MongoOp, RedisOp, SqlMeta};

/// Tool result wrapper that advertises a root `object` output schema.
///
/// MCP clients reject `tools/list` entries whose `outputSchema` has no root
/// `type: "object"`. schemars maps `serde_json::Value` to a permissive schema
/// without a root type, so every tool returns this wrapper instead. Tool results
/// are always JSON objects and the inner value is serialized unchanged.
#[derive(Debug, Clone)]
pub struct StructuredJson(Value);

impl From<Value> for StructuredJson {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

impl Serialize for StructuredJson {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl JsonSchema for StructuredJson {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ToolOutput")
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "properties": {},
            "additionalProperties": true
        })
    }
}

#[derive(Clone)]
pub struct DbServer {
    state: Arc<AppState>,
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

impl DbServer {
    pub fn new(config: ServerConfig) -> Self {
        let state = Arc::new(AppState::new(config));
        let reaper = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                reaper.reap().await;
            }
        });
        let mut tool_router = Self::tool_router();
        #[cfg(feature = "sql")]
        tool_router.merge(Self::sql_tool_router());
        #[cfg(feature = "mongo")]
        tool_router.merge(Self::mongo_tool_router());
        #[cfg(feature = "redis")]
        tool_router.merge(Self::redis_tool_router());
        Self { state, tool_router }
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct OpenConnectionArgs {
    /// Database URL. Supported schemes: postgres, postgresql, mysql, sqlite, mongodb, mongodb+srv, redis, rediss.
    /// Credentials in this URL may be stored by the MCP client or the model context.
    database_url: String,
    /// When true (the default), the connection rejects writes.
    #[serde(default = "default_true")]
    read_only: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ConnectionIdArgs {
    connection_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SqlSchemaArgs {
    connection_id: String,
    database: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SqlTablesArgs {
    connection_id: String,
    schema: Option<String>,
    #[serde(default = "default_true")]
    include_views: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SqlTableArgs {
    connection_id: String,
    schema: Option<String>,
    table: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SqlQueryArgs {
    connection_id: String,
    sql: String,
    params: Option<Vec<Value>>,
    transaction_id: Option<String>,
    limit: Option<u64>,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SqlExecuteArgs {
    connection_id: String,
    sql: String,
    params: Option<Vec<Value>>,
    transaction_id: Option<String>,
    #[serde(default)]
    allow_destructive: bool,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SqlExplainArgs {
    connection_id: String,
    sql: String,
    params: Option<Vec<Value>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SqlBeginArgs {
    connection_id: String,
    read_only: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TransactionArgs {
    transaction_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MongoCollectionArgs {
    connection_id: String,
    database: Option<String>,
    collection: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MongoFindArgs {
    connection_id: String,
    database: Option<String>,
    collection: String,
    #[serde(default = "empty_object")]
    filter: Value,
    projection: Option<Value>,
    sort: Option<Value>,
    limit: Option<u64>,
    #[serde(default)]
    skip: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MongoAggregateArgs {
    connection_id: String,
    database: Option<String>,
    collection: String,
    pipeline: Vec<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MongoInsertArgs {
    connection_id: String,
    database: Option<String>,
    collection: String,
    documents: Vec<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MongoUpdateArgs {
    connection_id: String,
    database: Option<String>,
    collection: String,
    #[serde(default = "empty_object")]
    filter: Value,
    update: Value,
    #[serde(default)]
    upsert: bool,
    #[serde(default)]
    multi: bool,
    #[serde(default)]
    allow_destructive: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MongoDeleteArgs {
    connection_id: String,
    database: Option<String>,
    collection: String,
    #[serde(default = "empty_object")]
    filter: Value,
    #[serde(default)]
    multi: bool,
    #[serde(default)]
    allow_destructive: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RedisScanArgs {
    connection_id: String,
    #[serde(default = "default_cursor")]
    cursor: String,
    #[serde(rename = "match")]
    pattern: Option<String>,
    count: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RedisKeyArgs {
    connection_id: String,
    key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RedisSetArgs {
    connection_id: String,
    key: String,
    text: Option<String>,
    base64: Option<String>,
    ex: Option<u64>,
    px: Option<u64>,
    #[serde(default)]
    nx: bool,
    #[serde(default)]
    xx: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RedisDeleteArgs {
    connection_id: String,
    keys: Vec<String>,
    #[serde(default)]
    unlink: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RedisHashArgs {
    connection_id: String,
    /// One of hget, hset, hdel, hgetall, hlen, hexists, hscan.
    action: String,
    key: String,
    field: Option<String>,
    text: Option<String>,
    base64: Option<String>,
    cursor: Option<String>,
    count: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RedisListArgs {
    connection_id: String,
    /// One of llen, lrange, lpush, rpush, lpop, rpop.
    action: String,
    key: String,
    text: Option<String>,
    base64: Option<String>,
    start: Option<i64>,
    stop: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RedisSetMembersArgs {
    connection_id: String,
    /// One of scard, sismember, sadd, srem, sscan.
    action: String,
    key: String,
    text: Option<String>,
    base64: Option<String>,
    cursor: Option<String>,
    count: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RedisSortedSetArgs {
    connection_id: String,
    /// One of zcard, zrange, zadd, zrem.
    action: String,
    key: String,
    member_text: Option<String>,
    member_base64: Option<String>,
    score: Option<f64>,
    start: Option<i64>,
    stop: Option<i64>,
}

fn empty_object() -> Value {
    json!({})
}

fn default_cursor() -> String {
    "0".into()
}

#[tool_router]
impl DbServer {
    #[tool(
        description = "Connect to a PostgreSQL, MySQL/MariaDB, SQLite, MongoDB, Redis, or Valkey database using a database_url. Call this first when the user asks to access a database. Returns connection_id for later tools. Credentials inside the URL may be written to MCP client logs or the model context.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn open_connection(
        &self,
        Parameters(args): Parameters<OpenConnectionArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(self.state.open(&args.database_url, args.read_only).await)
    }

    #[tool(
        description = "Close an existing database connection and roll back its open SQL transactions. Call this when the task is finished.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn close_connection(
        &self,
        Parameters(args): Parameters<ConnectionIdArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(self.state.close(&args.connection_id).await)
    }

    #[tool(
        description = "List database connections open in this server process. Returns kind, redacted host, database name, and read-only state without the original URL or password.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_connections(&self) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        Ok(Json(self.state.list().await.into()))
    }

    #[tool(
        description = "Read engine version, health, effective permissions, and result limits for a connection_id. Does not return the database URL or password.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_connection_info(
        &self,
        Parameters(args): Parameters<ConnectionIdArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(self.state.info(&args.connection_id).await)
    }

    #[tool(
        description = "Describe implemented features and known limitations for a connection_id. Call this after open_connection and before choosing SQL, MongoDB, or Redis tools.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_capabilities(
        &self,
        Parameters(args): Parameters<ConnectionIdArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(self.state.capabilities(&args.connection_id).await)
    }
}

#[cfg(feature = "sql")]
#[tool_router(router = sql_tool_router)]
impl DbServer {
    #[tool(
        description = "List databases visible to a SQL connection_id. Use this to discover PostgreSQL, MySQL, or SQLite databases before inspecting schemas.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn sql_list_databases(
        &self,
        Parameters(args): Parameters<ConnectionIdArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .sql_meta(&args.connection_id, SqlMeta::Databases)
                .await,
        )
    }

    #[cfg(feature = "sql")]
    #[tool(
        description = "List schemas on a SQL connection. PostgreSQL returns schema names; MySQL returns database names; SQLite returns main.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn sql_list_schemas(
        &self,
        Parameters(args): Parameters<SqlSchemaArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .sql_meta(
                    &args.connection_id,
                    SqlMeta::Schemas {
                        database: args.database,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "sql")]
    #[tool(
        description = "List tables and optional views for a SQL connection. Use this to find a table before describing or querying it.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn sql_list_tables(
        &self,
        Parameters(args): Parameters<SqlTablesArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .sql_meta(
                    &args.connection_id,
                    SqlMeta::Tables {
                        schema: args.schema,
                        include_views: args.include_views,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "sql")]
    #[tool(
        description = "Describe columns, primary key, foreign keys, and indexes for one SQL table. Use this when the user asks for a table structure.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn sql_describe_table(
        &self,
        Parameters(args): Parameters<SqlTableArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .sql_meta(
                    &args.connection_id,
                    SqlMeta::Describe {
                        schema: args.schema,
                        table: args.table,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "sql")]
    #[tool(
        description = "Read rows from a PostgreSQL, MySQL, or SQLite connection with a single read-only statement. Use PostgreSQL $1 placeholders or MySQL/SQLite ? placeholders. Params are JSON values, or objects such as {\"t\":\"decimal\",\"v\":\"1.20\"} and {\"t\":\"bytes\",\"v\":\"BASE64\"}. Rejects DML, DDL, multiple statements, and locking reads.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn sql_query(
        &self,
        Parameters(args): Parameters<SqlQueryArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .sql_query(
                    &args.connection_id,
                    &args.sql,
                    args.params.as_deref(),
                    args.transaction_id.as_deref(),
                    args.limit,
                    args.timeout_ms,
                )
                .await,
        )
    }

    #[cfg(feature = "sql")]
    #[tool(
        description = "Execute one SQL write statement on a connection opened with read_only=false while the server was started with --allow-writes. Returns rows_affected. DROP, TRUNCATE, and UPDATE/DELETE without WHERE also require --allow-destructive and allow_destructive=true.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn sql_execute(
        &self,
        Parameters(args): Parameters<SqlExecuteArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .sql_execute(
                    &args.connection_id,
                    &args.sql,
                    args.params.as_deref(),
                    args.transaction_id.as_deref(),
                    args.allow_destructive,
                    args.timeout_ms,
                )
                .await,
        )
    }

    #[cfg(feature = "sql")]
    #[tool(
        description = "Show a SQL execution plan without running EXPLAIN ANALYZE. Use this to inspect how a read-only statement would run.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn sql_explain(
        &self,
        Parameters(args): Parameters<SqlExplainArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .sql_meta(
                    &args.connection_id,
                    SqlMeta::Explain {
                        sql: args.sql,
                        params: args.params,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "sql")]
    #[tool(
        description = "Begin a SQL transaction on a dedicated physical connection and return transaction_id. Pass that id to sql_query or sql_execute. Writable transactions require --allow-writes and a writable connection.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn sql_begin_transaction(
        &self,
        Parameters(args): Parameters<SqlBeginArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .sql_begin(&args.connection_id, args.read_only)
                .await,
        )
    }

    #[cfg(feature = "sql")]
    #[tool(
        description = "Commit a SQL transaction identified by transaction_id and release its connection.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn sql_commit_transaction(
        &self,
        Parameters(args): Parameters<TransactionArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(self.state.sql_finish(&args.transaction_id, true).await)
    }

    #[cfg(feature = "sql")]
    #[tool(
        description = "Roll back a SQL transaction identified by transaction_id and release its connection.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn sql_rollback_transaction(
        &self,
        Parameters(args): Parameters<TransactionArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(self.state.sql_finish(&args.transaction_id, false).await)
    }

    #[cfg(feature = "sql")]
    #[tool(
        description = "Read approximate or exact table statistics for a SQL table, including row estimate and size when the engine exposes them.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn sql_get_table_stats(
        &self,
        Parameters(args): Parameters<SqlTableArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .sql_meta(
                    &args.connection_id,
                    SqlMeta::Stats {
                        schema: args.schema,
                        table: args.table,
                    },
                )
                .await,
        )
    }
}

#[cfg(feature = "mongo")]
#[tool_router(router = mongo_tool_router)]
impl DbServer {
    #[tool(
        description = "List MongoDB databases visible to a connection_id.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn mongo_list_databases(
        &self,
        Parameters(args): Parameters<ConnectionIdArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .mongo(&args.connection_id, MongoOp::Databases)
                .await,
        )
    }

    #[cfg(feature = "mongo")]
    #[tool(
        description = "List collections and views in a MongoDB database. Pass database when the connection URL has no default database.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn mongo_list_collections(
        &self,
        Parameters(args): Parameters<SqlSchemaArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .mongo(
                    &args.connection_id,
                    MongoOp::Collections {
                        database: args.database,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "mongo")]
    #[tool(
        description = "Describe a MongoDB collection using indexes, collection options, and field types inferred from a bounded sample. Inferred fields are marked inferred=true.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn mongo_describe_collection(
        &self,
        Parameters(args): Parameters<MongoCollectionArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .mongo(
                    &args.connection_id,
                    MongoOp::Describe {
                        database: args.database,
                        collection: args.collection,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "mongo")]
    #[tool(
        description = "Find MongoDB documents. Filter, projection, and sort accept JSON or Extended JSON such as {\"$oid\":\"...\"}. Results keep ObjectId, Date, Decimal128, and Binary types.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn mongo_find(
        &self,
        Parameters(args): Parameters<MongoFindArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .mongo(
                    &args.connection_id,
                    MongoOp::Find {
                        database: args.database,
                        collection: args.collection,
                        filter: args.filter,
                        projection: args.projection,
                        sort: args.sort,
                        limit: args.limit,
                        skip: args.skip,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "mongo")]
    #[tool(
        description = "Run a MongoDB aggregation pipeline and return a bounded result. Read-only connections reject $out and $merge, including stages nested in $facet or $lookup.",
        annotations(read_only_hint = true, destructive_hint = true, open_world_hint = true)
    )]
    async fn mongo_aggregate(
        &self,
        Parameters(args): Parameters<MongoAggregateArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .mongo(
                    &args.connection_id,
                    MongoOp::Aggregate {
                        database: args.database,
                        collection: args.collection,
                        pipeline: args.pipeline,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "mongo")]
    #[tool(
        description = "Insert one or more MongoDB documents. Requires --allow-writes and a connection opened with read_only=false. Documents may use Extended JSON.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn mongo_insert(
        &self,
        Parameters(args): Parameters<MongoInsertArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .mongo(
                    &args.connection_id,
                    MongoOp::Insert {
                        database: args.database,
                        collection: args.collection,
                        documents: args.documents,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "mongo")]
    #[tool(
        description = "Update MongoDB documents. Set multi=true for update_many and upsert=true to insert when no document matches. An empty filter with multi=true also requires destructive authorization.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn mongo_update(
        &self,
        Parameters(args): Parameters<MongoUpdateArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .mongo(
                    &args.connection_id,
                    MongoOp::Update {
                        database: args.database,
                        collection: args.collection,
                        filter: args.filter,
                        update: args.update,
                        upsert: args.upsert,
                        multi: args.multi,
                        allow_destructive: args.allow_destructive,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "mongo")]
    #[tool(
        description = "Delete MongoDB documents matching a filter. multi=false deletes one document. An empty filter requires --allow-destructive and allow_destructive=true.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn mongo_delete(
        &self,
        Parameters(args): Parameters<MongoDeleteArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .mongo(
                    &args.connection_id,
                    MongoOp::Delete {
                        database: args.database,
                        collection: args.collection,
                        filter: args.filter,
                        multi: args.multi,
                        allow_destructive: args.allow_destructive,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "mongo")]
    #[tool(
        description = "List index definitions for a MongoDB collection.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn mongo_list_indexes(
        &self,
        Parameters(args): Parameters<MongoCollectionArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .mongo(
                    &args.connection_id,
                    MongoOp::Indexes {
                        database: args.database,
                        collection: args.collection,
                    },
                )
                .await,
        )
    }
}

#[cfg(feature = "redis")]
#[tool_router(router = redis_tool_router)]
impl DbServer {
    #[tool(
        description = "Incrementally discover Redis or Valkey keys with SCAN. Pass next_cursor back until it is 0. SCAN may repeat keys and can miss keys written during the scan. Do not use KEYS.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn redis_scan(
        &self,
        Parameters(args): Parameters<RedisScanArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .redis(
                    &args.connection_id,
                    RedisOp::Scan {
                        cursor: args.cursor,
                        pattern: args.pattern,
                        count: args.count,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "redis")]
    #[tool(
        description = "Read Redis or Valkey key type, TTL, PTTL, and optional memory usage. Use this to inspect expiration.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn redis_key_info(
        &self,
        Parameters(args): Parameters<RedisKeyArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .redis(&args.connection_id, RedisOp::KeyInfo { key: args.key })
                .await,
        )
    }

    #[cfg(feature = "redis")]
    #[tool(
        description = "Get a Redis or Valkey string value. UTF-8 values are marked encoding=utf8. Other bytes are returned as base64 without loss.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn redis_get(
        &self,
        Parameters(args): Parameters<RedisKeyArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .redis(&args.connection_id, RedisOp::Get { key: args.key })
                .await,
        )
    }

    #[cfg(feature = "redis")]
    #[tool(
        description = "Set a Redis or Valkey string. Provide text or base64, not both. Optional ex/px and nx/xx are supported. Requires write authorization.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn redis_set(
        &self,
        Parameters(args): Parameters<RedisSetArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .redis(
                    &args.connection_id,
                    RedisOp::Set {
                        key: args.key,
                        text: args.text,
                        base64: args.base64,
                        ex: args.ex,
                        px: args.px,
                        nx: args.nx,
                        xx: args.xx,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "redis")]
    #[tool(
        description = "Delete explicit Redis or Valkey keys with DEL, or UNLINK when unlink=true. Requires write authorization. Does not accept a key pattern.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn redis_delete(
        &self,
        Parameters(args): Parameters<RedisDeleteArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .redis(
                    &args.connection_id,
                    RedisOp::Delete {
                        keys: args.keys,
                        unlink: args.unlink,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "redis")]
    #[tool(
        description = "Run one Redis or Valkey hash action: hget, hset, hdel, hgetall, hlen, hexists, or hscan. Write actions require write authorization.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn redis_hash(
        &self,
        Parameters(args): Parameters<RedisHashArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .redis(
                    &args.connection_id,
                    RedisOp::Hash {
                        action: args.action,
                        key: args.key,
                        field: args.field,
                        text: args.text,
                        base64: args.base64,
                        cursor: args.cursor,
                        count: args.count,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "redis")]
    #[tool(
        description = "Run one Redis or Valkey list action: llen, lrange, lpush, rpush, lpop, or rpop. Write actions require write authorization.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn redis_list(
        &self,
        Parameters(args): Parameters<RedisListArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .redis(
                    &args.connection_id,
                    RedisOp::List {
                        action: args.action,
                        key: args.key,
                        text: args.text,
                        base64: args.base64,
                        start: args.start,
                        stop: args.stop,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "redis")]
    #[tool(
        description = "Run one Redis or Valkey set action: scard, sismember, sadd, srem, or sscan. Write actions require write authorization.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn redis_set_members(
        &self,
        Parameters(args): Parameters<RedisSetMembersArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .redis(
                    &args.connection_id,
                    RedisOp::SetMembers {
                        action: args.action,
                        key: args.key,
                        text: args.text,
                        base64: args.base64,
                        cursor: args.cursor,
                        count: args.count,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "redis")]
    #[tool(
        description = "Run one Redis or Valkey sorted set action: zcard, zrange, zadd, or zrem. zrange returns members with scores. Write actions require write authorization.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn redis_sorted_set(
        &self,
        Parameters(args): Parameters<RedisSortedSetArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(
            self.state
                .redis(
                    &args.connection_id,
                    RedisOp::SortedSet {
                        action: args.action,
                        key: args.key,
                        member_text: args.member_text,
                        member_base64: args.member_base64,
                        score: args.score,
                        start: args.start,
                        stop: args.stop,
                    },
                )
                .await,
        )
    }

    #[cfg(feature = "redis")]
    #[tool(
        description = "Read a filtered Redis or Valkey INFO subset and PING. Does not expose CONFIG or command stats that can leak sensitive server details.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn redis_info(
        &self,
        Parameters(args): Parameters<ConnectionIdArgs>,
    ) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
        timed(self.state.redis(&args.connection_id, RedisOp::Info).await)
    }
}

#[tool_handler(
    name = "dbconnector",
    version = "0.1.0",
    instructions = "Call open_connection with a database_url, then get_capabilities, then the engine-specific tools, and close_connection when finished. The server is read-only unless it was started with --allow-writes and the connection sets read_only to false. Credentials in database_url may be stored by the MCP client or model context.",
    router = self.tool_router
)]
impl ServerHandler for DbServer {}

fn timed(result: Result<Value, DbError>) -> Result<Json<StructuredJson>, rmcp::ErrorData> {
    match result {
        Ok(mut value) => {
            if let Some(object) = value.as_object_mut() {
                object.entry("duration_ms").or_insert(json!(0));
            }
            Ok(Json(value.into()))
        }
        Err(err) => Err(to_mcp(err)),
    }
}

fn to_mcp(err: DbError) -> rmcp::ErrorData {
    let data = serde_json::to_value(&err).unwrap_or(Value::Null);
    let message = err.to_string();
    match err.code {
        dbconnector_core::ErrorCode::InvalidArgument => {
            rmcp::ErrorData::invalid_params(message, Some(data))
        }
        dbconnector_core::ErrorCode::InternalError => {
            rmcp::ErrorData::internal_error(message, Some(data))
        }
        _ => rmcp::ErrorData::invalid_params(message, Some(data)),
    }
}

pub async fn serve(config: ServerConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let server = DbServer::new(config);
    let running = server.serve(rmcp::transport::stdio()).await?;
    running.waiting().await?;
    Ok(())
}

#[allow(dead_code)]
fn _instant(instant: Instant) -> Instant {
    instant
}

#[cfg(test)]
mod output_schema_tests {
    use dbconnector_core::ServerConfig;

    use super::DbServer;

    #[tokio::test]
    async fn every_tool_output_schema_type_is_object() {
        let server = DbServer::new(ServerConfig::default());
        let tools = server.tool_router.list_all();
        let expected = 5
            + if cfg!(feature = "sql") { 11 } else { 0 }
            + if cfg!(feature = "mongo") { 9 } else { 0 }
            + if cfg!(feature = "redis") { 10 } else { 0 };
        assert_eq!(tools.len(), expected, "registered tool count");
        for tool in tools {
            let schema = tool.output_schema.as_ref().unwrap_or_else(|| {
                panic!("{} is missing outputSchema", tool.name);
            });
            assert_eq!(
                schema.get("type").and_then(|value| value.as_str()),
                Some("object"),
                "{} outputSchema={}",
                tool.name,
                serde_json::Value::Object(schema.as_ref().clone())
            );
        }
    }
}
