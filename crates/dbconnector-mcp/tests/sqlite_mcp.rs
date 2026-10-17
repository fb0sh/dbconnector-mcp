//! In-process stdio-equivalent MCP session against a temporary SQLite file.
//! The transport is a Tokio duplex, which uses the same NDJSON framing as stdio.

use std::time::Duration;

use dbconnector_core::{NetworkPolicy, ServerConfig};
use dbconnector_mcp::DbServer;
use rmcp::model::CallToolRequestParams;
use rmcp::service::serve_client;
use rmcp::ServiceExt;
use serde_json::{json, Value};

const TOOLS: &[&str] = &[
    "open_connection",
    "close_connection",
    "list_connections",
    "get_connection_info",
    "get_capabilities",
    "sql_list_databases",
    "sql_list_schemas",
    "sql_list_tables",
    "sql_describe_table",
    "sql_query",
    "sql_execute",
    "sql_explain",
    "sql_begin_transaction",
    "sql_commit_transaction",
    "sql_rollback_transaction",
    "sql_get_table_stats",
    "mongo_list_databases",
    "mongo_list_collections",
    "mongo_describe_collection",
    "mongo_find",
    "mongo_aggregate",
    "mongo_insert",
    "mongo_update",
    "mongo_delete",
    "mongo_list_indexes",
    "redis_scan",
    "redis_key_info",
    "redis_get",
    "redis_set",
    "redis_delete",
    "redis_hash",
    "redis_list",
    "redis_set_members",
    "redis_sorted_set",
    "redis_info",
];

struct Session {
    client: rmcp::service::RunningService<rmcp::RoleClient, ()>,
}

impl Session {
    async fn start(config: ServerConfig) -> Self {
        let (server_io, client_io) = tokio::io::duplex(1024 * 1024);
        let server = DbServer::new(config);
        tokio::spawn(async move {
            let running = server.serve(server_io).await.expect("server transport");
            let _ = running.waiting().await;
        });
        let client = serve_client((), client_io)
            .await
            .expect("client initialize");
        Self { client }
    }

    async fn call(&self, name: &str, args: Value) -> Result<Value, String> {
        let arguments = args.as_object().cloned().unwrap_or_default();
        let params = CallToolRequestParams::new(name.to_string()).with_arguments(arguments);
        match self.client.call_tool(params).await {
            Ok(result) => {
                let content = result.structured_content.clone();
                content.ok_or_else(|| format!("{name} returned no structured content: {result:?}"))
            }
            Err(err) => Err(err.to_string()),
        }
    }
}

fn writable_config() -> ServerConfig {
    ServerConfig {
        allow_writes: true,
        allow_destructive: true,
        ..ServerConfig::default()
    }
}

#[tokio::test]
async fn default_build_lists_35_tools_and_sqlite_round_trip() {
    let dir = std::env::temp_dir().join(format!("dbconnector-mcp-{}", uuid_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("app.db");
    let url = format!("sqlite://{}", path.display());

    let session = Session::start(writable_config()).await;
    let listed = session
        .client
        .list_tools(None)
        .await
        .expect("tools/list")
        .tools;
    let names: Vec<String> = listed.iter().map(|tool| tool.name.to_string()).collect();
    assert_eq!(names.len(), 35, "tools: {names:?}");
    for expected in TOOLS {
        assert!(
            names.iter().any(|name| name == expected),
            "missing {expected}"
        );
    }

    let opened = session
        .call(
            "open_connection",
            json!({"database_url": url, "read_only": false}),
        )
        .await
        .expect("open");
    let connection_id = opened["connection_id"].as_str().unwrap().to_string();
    assert_eq!(opened["kind"], "sqlite");
    assert_eq!(opened["read_only"], false);

    let caps = session
        .call("get_capabilities", json!({"connection_id": connection_id}))
        .await
        .unwrap();
    assert!(caps["features"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "query"));

    session
        .call(
            "sql_execute",
            json!({
                "connection_id": connection_id,
                "sql": "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)"
            }),
        )
        .await
        .expect("create");
    session
        .call(
            "sql_execute",
            json!({
                "connection_id": connection_id,
                "sql": "INSERT INTO notes (body) VALUES (?)",
                "params": ["hello"]
            }),
        )
        .await
        .expect("insert");

    let queried = session
        .call(
            "sql_query",
            json!({
                "connection_id": connection_id,
                "sql": "SELECT body FROM notes WHERE id = ?",
                "params": [1]
            }),
        )
        .await
        .expect("select");
    assert_eq!(queried["returned_rows"], 1);
    assert_eq!(queried["rows"][0][0], "hello");

    let described = session
        .call(
            "sql_describe_table",
            json!({"connection_id": connection_id, "table": "notes"}),
        )
        .await
        .expect("describe");
    assert!(described["columns"]
        .as_array()
        .unwrap()
        .iter()
        .any(|col| col["name"] == "body" || col["name"] == "body"));

    let explained = session
        .call(
            "sql_explain",
            json!({
                "connection_id": connection_id,
                "sql": "SELECT body FROM notes WHERE id = 1"
            }),
        )
        .await
        .expect("explain");
    assert_eq!(explained["executed"], false);

    let begun = session
        .call(
            "sql_begin_transaction",
            json!({"connection_id": connection_id, "read_only": false}),
        )
        .await
        .expect("begin");
    let transaction_id = begun["transaction_id"].as_str().unwrap().to_string();
    session
        .call(
            "sql_execute",
            json!({
                "connection_id": connection_id,
                "transaction_id": transaction_id,
                "sql": "INSERT INTO notes (body) VALUES ('rolled')"
            }),
        )
        .await
        .expect("txn insert");
    session
        .call(
            "sql_rollback_transaction",
            json!({"transaction_id": transaction_id}),
        )
        .await
        .expect("rollback");
    let after_rollback = session
        .call(
            "sql_query",
            json!({
                "connection_id": connection_id,
                "sql": "SELECT COUNT(*) FROM notes"
            }),
        )
        .await
        .unwrap();
    assert_eq!(after_rollback["rows"][0][0], 1);

    let denied = session
        .call(
            "sql_execute",
            json!({
                "connection_id": connection_id,
                "sql": "DELETE FROM notes"
            }),
        )
        .await
        .expect_err("unconditional delete");
    assert!(
        denied.to_ascii_lowercase().contains("destructive") || denied.contains("PERMISSION"),
        "{denied}"
    );

    session
        .call(
            "sql_execute",
            json!({
                "connection_id": connection_id,
                "sql": "DELETE FROM notes",
                "allow_destructive": true
            }),
        )
        .await
        .expect("authorized delete");

    for _ in 0..3 {
        session
            .call(
                "sql_execute",
                json!({
                    "connection_id": connection_id,
                    "sql": "INSERT INTO notes (body) VALUES ('row')"
                }),
            )
            .await
            .unwrap();
    }
    let limited = session
        .call(
            "sql_query",
            json!({
                "connection_id": connection_id,
                "sql": "SELECT body FROM notes",
                "limit": 1
            }),
        )
        .await
        .unwrap();
    assert_eq!(limited["returned_rows"], 1);
    assert_eq!(limited["truncated"], true);

    let listed_connections = session.call("list_connections", json!({})).await.unwrap();
    let blob = listed_connections.to_string();
    assert!(!blob.contains("password"));
    assert!(blob.contains(&connection_id));

    session
        .call("close_connection", json!({"connection_id": connection_id}))
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn read_only_connection_rejects_writes_and_policy_blocks_before_connect() {
    let session = Session::start(ServerConfig::default()).await;
    let opened = session
        .call(
            "open_connection",
            json!({"database_url": "sqlite::memory:", "read_only": true}),
        )
        .await
        .expect("open memory");
    let connection_id = opened["connection_id"].as_str().unwrap();
    let err = session
        .call(
            "sql_execute",
            json!({
                "connection_id": connection_id,
                "sql": "CREATE TABLE t (id INTEGER)"
            }),
        )
        .await
        .expect_err("write");
    assert!(
        err.to_ascii_lowercase().contains("read-only") || err.contains("PERMISSION"),
        "{err}"
    );

    let secret = "SuperSecretValue";
    let restricted = ServerConfig {
        network: NetworkPolicy::Restricted {
            hosts: vec!["db.example".into()],
        },
        require_db_tls: true,
        ..ServerConfig::default()
    };
    let guarded = Session::start(restricted).await;
    let blocked = guarded
        .call(
            "open_connection",
            json!({
                "database_url": format!("postgres://user:{secret}@203.0.113.10:5432/app"),
                "read_only": true
            }),
        )
        .await
        .expect_err("policy");
    assert!(!blocked.contains(secret), "{blocked}");
    assert!(
        blocked.to_ascii_lowercase().contains("tls")
            || blocked.to_ascii_lowercase().contains("permission")
            || blocked.contains("PERMISSION"),
        "{blocked}"
    );
}

fn uuid_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(1))
        .as_nanos();
    format!("{nanos}")
}
