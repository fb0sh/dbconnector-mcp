//! Live engine checks. Skipped unless `DBCONNECTOR_INTEGRATION=1`.
//! Start the stack with `docker compose -f docker-compose.test.yml up -d --wait`.
//! Fixture password is `dbconnector_test` and is not a production secret.

use dbconnector_core::ServerConfig;
use dbconnector_mcp::DbServer;
use rmcp::model::CallToolRequestParams;
use rmcp::service::serve_client;
use rmcp::ServiceExt;
use serde_json::{json, Value};

struct Session {
    client: rmcp::service::RunningService<rmcp::RoleClient, ()>,
}

impl Session {
    async fn start(config: ServerConfig) -> Self {
        let (server_io, client_io) = tokio::io::duplex(1024 * 1024);
        let server = DbServer::new(config);
        tokio::spawn(async move {
            let running = server.serve(server_io).await.expect("server");
            let _ = running.waiting().await;
        });
        let client = serve_client((), client_io).await.expect("client");
        Self { client }
    }

    async fn call(&self, name: &str, args: Value) -> Result<Value, String> {
        let arguments = args.as_object().cloned().unwrap_or_default();
        let params = CallToolRequestParams::new(name.to_string()).with_arguments(arguments);
        match self.client.call_tool(params).await {
            Ok(result) => result
                .structured_content
                .clone()
                .ok_or_else(|| format!("{name} missing structured content")),
            Err(err) => Err(err.to_string()),
        }
    }
}

fn enabled() -> bool {
    std::env::var("DBCONNECTOR_INTEGRATION").ok().as_deref() == Some("1")
}

const POSTGRES_URL: &str =
    "postgres://dbconnector:dbconnector_test@127.0.0.1:54329/app?sslmode=disable";
const MYSQL_URL: &str =
    "mysql://dbconnector:dbconnector_test@127.0.0.1:33069/app?ssl-mode=DISABLED";

/// `MySQL` returns `TEXT` as bytes through sqlx `Any`; valid UTF-8 is wrapped.
fn cell_text(cell: &Value) -> &str {
    cell.as_str()
        .or_else(|| cell.get("text").and_then(Value::as_str))
        .unwrap_or("")
}

/// A table name that is unique per test run so repeated runs do not collide.
fn unique_table(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("{prefix}_{}", nanos % 1_000_000_000)
}

fn skip(name: &str) -> bool {
    if enabled() {
        return false;
    }
    eprintln!("skip {name}: set DBCONNECTOR_INTEGRATION=1");
    true
}

fn writable() -> ServerConfig {
    ServerConfig {
        allow_writes: true,
        allow_destructive: true,
        ..ServerConfig::default()
    }
}

async fn open(session: &Session, url: &str, read_only: bool) -> String {
    let opened = session
        .call(
            "open_connection",
            json!({"database_url": url, "read_only": read_only}),
        )
        .await
        .unwrap_or_else(|err| panic!("open {url}: {err}"));
    opened["connection_id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn postgres_mysql_round_trip() {
    if !enabled() {
        eprintln!("skip postgres_mysql_round_trip: set DBCONNECTOR_INTEGRATION=1");
        return;
    }
    let session = Session::start(writable()).await;
    let pg = open(
        &session,
        "postgres://dbconnector:dbconnector_test@127.0.0.1:54329/app?sslmode=disable",
        false,
    )
    .await;
    let rows = session
        .call(
            "sql_query",
            json!({"connection_id": pg, "sql": "SELECT body FROM notes"}),
        )
        .await
        .expect("pg select");
    assert_eq!(rows["rows"][0][0], "fixture");
    let ro = open(
        &session,
        "postgres://dbconnector_ro:dbconnector_test@127.0.0.1:54329/app?sslmode=disable",
        true,
    )
    .await;
    let denied = session
        .call(
            "sql_execute",
            json!({"connection_id": ro, "sql": "INSERT INTO notes (id, body) VALUES (2, 'no')"}),
        )
        .await
        .expect_err("pg ro");
    assert!(denied.to_ascii_lowercase().contains("read-only") || denied.contains("PERMISSION"));

    let my = open(
        &session,
        "mysql://dbconnector:dbconnector_test@127.0.0.1:33069/app?ssl-mode=DISABLED",
        false,
    )
    .await;
    let my_rows = session
        .call(
            "sql_query",
            json!({"connection_id": my, "sql": "SELECT body FROM notes"}),
        )
        .await
        .expect("mysql select");
    let cell = &my_rows["rows"][0][0];
    assert_eq!(cell_text(cell), "fixture");
    session
        .call(
            "sql_explain",
            json!({"connection_id": my, "sql": "SELECT body FROM notes"}),
        )
        .await
        .expect("mysql explain");
}

#[tokio::test]
async fn mongo_crud_and_pipeline_guard() {
    if !enabled() {
        eprintln!("skip mongo_crud_and_pipeline_guard: set DBCONNECTOR_INTEGRATION=1");
        return;
    }
    let session = Session::start(writable()).await;
    let id = open(
        &session,
        "mongodb://dbconnector:dbconnector_test@127.0.0.1:27018/app?authSource=app",
        false,
    )
    .await;
    let listed = session
        .call("mongo_list_databases", json!({"connection_id": id}))
        .await
        .expect("dbs");
    assert!(listed["databases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "app"));
    session
        .call(
            "mongo_insert",
            json!({
                "connection_id": id,
                "database": "app",
                "collection": "notes",
                "documents": [{"body": "from-mcp"}]
            }),
        )
        .await
        .expect("insert");
    let found = session
        .call(
            "mongo_find",
            json!({
                "connection_id": id,
                "database": "app",
                "collection": "notes",
                "filter": {"body": "from-mcp"}
            }),
        )
        .await
        .expect("find");
    assert!(found["returned"].as_u64().unwrap() >= 1);
    let ro = open(
        &session,
        "mongodb://dbconnector_ro:dbconnector_test@127.0.0.1:27018/app?authSource=app",
        true,
    )
    .await;
    let blocked = session
        .call(
            "mongo_aggregate",
            json!({
                "connection_id": ro,
                "database": "app",
                "collection": "notes",
                "pipeline": [{"$match": {}}, {"$out": "stolen"}]
            }),
        )
        .await
        .expect_err("out");
    assert!(blocked.to_ascii_lowercase().contains("write") || blocked.contains("PERMISSION"));
}

#[tokio::test]
async fn redis_and_valkey_scan_ttl_and_read_only() {
    if !enabled() {
        eprintln!("skip redis_and_valkey_scan_ttl_and_read_only: set DBCONNECTOR_INTEGRATION=1");
        return;
    }
    let session = Session::start(writable()).await;
    for (label, url) in [
        ("redis", "redis://127.0.0.1:63799"),
        ("valkey", "redis://127.0.0.1:63809"),
    ] {
        let id = open(&session, url, false).await;
        session
            .call(
                "redis_set",
                json!({"connection_id": id, "key": format!("dbconnector:{label}"), "text": "1", "ex": 30}),
            )
            .await
            .unwrap_or_else(|err| panic!("{label} set: {err}"));
        let got = session
            .call(
                "redis_get",
                json!({"connection_id": id, "key": format!("dbconnector:{label}")}),
            )
            .await
            .unwrap_or_else(|err| panic!("{label} get: {err}"));
        assert_eq!(got["found"], true, "{label}");
        let info = session
            .call(
                "redis_key_info",
                json!({"connection_id": id, "key": format!("dbconnector:{label}")}),
            )
            .await
            .unwrap_or_else(|err| panic!("{label} info: {err}"));
        assert!(info["ttl_seconds"].as_i64().unwrap() > 0, "{info}");
        let scan = session
            .call(
                "redis_scan",
                json!({"connection_id": id, "cursor": "0", "pattern": "dbconnector:*", "count": 20}),
            )
            .await
            .unwrap_or_else(|err| panic!("{label} scan: {err}"));
        assert!(scan.to_string().contains(&format!("dbconnector:{label}")));
        session
            .call("redis_info", json!({"connection_id": id}))
            .await
            .unwrap_or_else(|err| panic!("{label} server info: {err}"));
    }

    let ro = open(
        &session,
        "redis://dbconnector_ro:dbconnector_test@127.0.0.1:63799",
        true,
    )
    .await;
    let denied = session
        .call(
            "redis_set",
            json!({"connection_id": ro, "key": "dbconnector:denied", "text": "no"}),
        )
        .await
        .expect_err("redis ro set");
    assert!(denied.to_ascii_lowercase().contains("read-only") || denied.contains("PERMISSION"));
}

/// Rollback hides a row, commit keeps it, and a finished transaction id is rejected.
async fn transaction_round_trip(session: &Session, connection_id: &str, table: &str) {
    session
        .call(
            "sql_execute",
            json!({
                "connection_id": connection_id,
                "sql": format!("CREATE TABLE {table} (id integer PRIMARY KEY, body text)")
            }),
        )
        .await
        .unwrap_or_else(|err| panic!("create {table}: {err}"));

    let rolled = session
        .call(
            "sql_begin_transaction",
            json!({"connection_id": connection_id, "read_only": false}),
        )
        .await
        .unwrap_or_else(|err| panic!("begin: {err}"));
    let rolled_id = rolled["transaction_id"].as_str().expect("transaction id");
    session
        .call(
            "sql_execute",
            json!({
                "connection_id": connection_id,
                "sql": format!("INSERT INTO {table} (id, body) VALUES (1, 'rolled')"),
                "transaction_id": rolled_id
            }),
        )
        .await
        .unwrap_or_else(|err| panic!("insert rolled: {err}"));
    session
        .call(
            "sql_rollback_transaction",
            json!({"connection_id": connection_id, "transaction_id": rolled_id}),
        )
        .await
        .unwrap_or_else(|err| panic!("rollback: {err}"));

    let after_rollback = session
        .call(
            "sql_query",
            json!({
                "connection_id": connection_id,
                "sql": format!("SELECT COUNT(*) AS n FROM {table}")
            }),
        )
        .await
        .unwrap_or_else(|err| panic!("count after rollback: {err}"));
    assert_eq!(
        after_rollback["rows"][0][0].as_i64(),
        Some(0),
        "{after_rollback}"
    );

    let kept = session
        .call(
            "sql_begin_transaction",
            json!({"connection_id": connection_id, "read_only": false}),
        )
        .await
        .unwrap_or_else(|err| panic!("begin 2: {err}"));
    let kept_id = kept["transaction_id"].as_str().expect("transaction id");
    session
        .call(
            "sql_execute",
            json!({
                "connection_id": connection_id,
                "sql": format!("INSERT INTO {table} (id, body) VALUES (2, 'kept')"),
                "transaction_id": kept_id
            }),
        )
        .await
        .unwrap_or_else(|err| panic!("insert kept: {err}"));
    session
        .call(
            "sql_commit_transaction",
            json!({"connection_id": connection_id, "transaction_id": kept_id}),
        )
        .await
        .unwrap_or_else(|err| panic!("commit: {err}"));

    let rows = session
        .call(
            "sql_query",
            json!({
                "connection_id": connection_id,
                "sql": format!("SELECT id, body FROM {table} ORDER BY id")
            }),
        )
        .await
        .unwrap_or_else(|err| panic!("select after commit: {err}"));
    assert_eq!(rows["returned_rows"].as_u64(), Some(1), "{rows}");
    assert_eq!(rows["rows"][0][0].as_i64(), Some(2), "{rows}");
    assert_eq!(cell_text(&rows["rows"][0][1]), "kept", "{rows}");

    let stale = session
        .call(
            "sql_commit_transaction",
            json!({"connection_id": connection_id, "transaction_id": kept_id}),
        )
        .await
        .expect_err("a finished transaction id must be rejected");
    assert!(stale.contains("TRANSACTION_NOT_FOUND"), "{stale}");

    session
        .call(
            "sql_execute",
            json!({
                "connection_id": connection_id,
                "sql": format!("DROP TABLE {table}"),
                "allow_destructive": true
            }),
        )
        .await
        .unwrap_or_else(|err| panic!("drop {table}: {err}"));
}

#[tokio::test]
async fn postgres_transaction_commit_and_rollback() {
    if skip("postgres_transaction_commit_and_rollback") {
        return;
    }
    let session = Session::start(writable()).await;
    let id = open(&session, POSTGRES_URL, false).await;
    transaction_round_trip(&session, &id, &unique_table("mcp_pg_txn")).await;
}

#[tokio::test]
async fn mysql_transaction_commit_and_rollback() {
    if skip("mysql_transaction_commit_and_rollback") {
        return;
    }
    let session = Session::start(writable()).await;
    let id = open(&session, MYSQL_URL, false).await;
    transaction_round_trip(&session, &id, &unique_table("mcp_mysql_txn")).await;
}

/// The catalog queries must stay decodable through the sqlx `Any` driver.
#[tokio::test]
async fn postgres_metadata_tools_decode_catalog_types() {
    if skip("postgres_metadata_tools_decode_catalog_types") {
        return;
    }
    let session = Session::start(writable()).await;
    let id = open(&session, POSTGRES_URL, false).await;

    let databases = session
        .call("sql_list_databases", json!({"connection_id": id}))
        .await
        .expect("pg list_databases");
    assert!(
        databases["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row[0].as_str() == Some("app"))),
        "{databases}"
    );

    let schemas = session
        .call("sql_list_schemas", json!({"connection_id": id}))
        .await
        .expect("pg list_schemas");
    assert!(schemas.to_string().contains("public"), "{schemas}");

    let tables = session
        .call(
            "sql_list_tables",
            json!({"connection_id": id, "schema": "public"}),
        )
        .await
        .expect("pg list_tables");
    assert_eq!(
        tables["columns"][0]["name"].as_str(),
        Some("table_schema"),
        "{tables}"
    );
    assert!(tables.to_string().contains("notes"), "{tables}");

    let described = session
        .call(
            "sql_describe_table",
            json!({"connection_id": id, "schema": "public", "table": "notes"}),
        )
        .await
        .expect("pg describe_table");
    let columns = described["columns"].as_array().expect("columns");
    assert!(
        columns
            .iter()
            .any(|column| column["column_name"].as_str() == Some("body")),
        "{described}"
    );
    assert!(
        described["primary_key"]
            .as_array()
            .is_some_and(|keys| keys.iter().any(|key| key.as_str() == Some("id"))),
        "{described}"
    );

    let stats = session
        .call(
            "sql_get_table_stats",
            json!({"connection_id": id, "schema": "public", "table": "notes"}),
        )
        .await
        .expect("pg get_table_stats");
    assert!(
        stats["stats"]
            .as_array()
            .is_some_and(|stats| !stats.is_empty()),
        "{stats}"
    );

    let explained = session
        .call(
            "sql_explain",
            json!({"connection_id": id, "sql": "SELECT body FROM notes WHERE id = 1"}),
        )
        .await
        .expect("pg explain");
    assert_eq!(explained["executed"], false, "{explained}");
    assert!(
        explained["plan"]["rows"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "{explained}"
    );
}

#[tokio::test]
async fn mysql_metadata_tools_decode_catalog_types() {
    if skip("mysql_metadata_tools_decode_catalog_types") {
        return;
    }
    let session = Session::start(writable()).await;
    let id = open(&session, MYSQL_URL, false).await;

    let databases = session
        .call("sql_list_databases", json!({"connection_id": id}))
        .await
        .expect("mysql list_databases");
    assert_eq!(
        databases["columns"][0]["name"].as_str(),
        Some("name"),
        "{databases}"
    );
    assert!(
        databases["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| cell_text(&row[0]) == "app")),
        "{databases}"
    );

    let schemas = session
        .call("sql_list_schemas", json!({"connection_id": id}))
        .await
        .expect("mysql list_schemas");
    assert!(
        schemas["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| cell_text(&row[0]) == "app")),
        "{schemas}"
    );

    let tables = session
        .call(
            "sql_list_tables",
            json!({"connection_id": id, "schema": "app"}),
        )
        .await
        .expect("mysql list_tables");
    assert_eq!(
        tables["columns"][1]["name"].as_str(),
        Some("table_name"),
        "{tables}"
    );
    assert!(tables.to_string().contains("notes"), "{tables}");

    let described = session
        .call(
            "sql_describe_table",
            json!({"connection_id": id, "schema": "app", "table": "notes"}),
        )
        .await
        .expect("mysql describe_table");
    let columns = described["columns"].as_array().expect("columns");
    let body = columns
        .iter()
        .find(|column| column["column_name"].as_str() == Some("body"))
        .unwrap_or_else(|| panic!("no body column in {described}"));
    // MySQL reports `information_schema.columns.data_type` as a binary column, so the
    // value keeps the documented `{encoding, text}` wrapper. `CAST(... AS CHAR)`
    // normalizes the columns that sqlx would otherwise wrap, such as `column_name`.
    assert_eq!(cell_text(&body["data_type"]), "text", "{described}");
    assert!(body["is_nullable"].is_string(), "{described}");
    assert!(body.get("COLUMN_NAME").is_none(), "{described}");

    let stats = session
        .call(
            "sql_get_table_stats",
            json!({"connection_id": id, "schema": "app", "table": "notes"}),
        )
        .await
        .expect("mysql get_table_stats");
    assert!(stats["stats"][0].get("row_estimate").is_some(), "{stats}");
    assert!(stats["stats"][0].get("total_bytes").is_some(), "{stats}");

    let explained = session
        .call(
            "sql_explain",
            json!({"connection_id": id, "sql": "SELECT body FROM notes"}),
        )
        .await
        .expect("mysql explain");
    assert_eq!(explained["executed"], false, "{explained}");
}
