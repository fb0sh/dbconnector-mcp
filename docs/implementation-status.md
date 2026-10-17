# Implementation status

Version: 0.1.0 workspace. Date checked: 2026-10-17. Host: Linux, rustc 1.97.1, Docker 29.8.0.

## Stage

M0 through M4 are implemented in this workspace: connection manager, SQL, MongoDB, and Redis/Valkey tools. M5 documents, CI workflows, and the disposable engine stack are in the tree. M5 is not accepted as complete: macOS, Windows, MCP Inspector, and a model-driven Codex or OpenCode session were not executed, and the GitHub workflows have not run on GitHub.

## Tools

The default build registers these 35 tools, confirmed by `tools/list` against both the
in-process server (`DBCONNECTOR_INTEGRATION=1` engine tests) and `target/release/dbconnector serve`:

`open_connection`, `close_connection`, `list_connections`, `get_connection_info`, `get_capabilities`, `sql_list_databases`, `sql_list_schemas`, `sql_list_tables`, `sql_describe_table`, `sql_query`, `sql_execute`, `sql_explain`, `sql_begin_transaction`, `sql_commit_transaction`, `sql_rollback_transaction`, `sql_get_table_stats`, `mongo_list_databases`, `mongo_list_collections`, `mongo_describe_collection`, `mongo_find`, `mongo_aggregate`, `mongo_insert`, `mongo_update`, `mongo_delete`, `mongo_list_indexes`, `redis_scan`, `redis_key_info`, `redis_get`, `redis_set`, `redis_delete`, `redis_hash`, `redis_list`, `redis_set_members`, `redis_sorted_set`, `redis_info`.

`--no-default-features` registers the five connection tools. `--no-default-features --features sql` registers connection tools plus SQL tools and not MongoDB or Redis. Both were checked with `registered_tools_follow_crate_features`.

## Fixed defects

- Strict MCP clients (OpenCode and any client built on the TypeScript MCP SDK) rejected `tools/list` with `Invalid result for tools/list: ... outputSchema.type expected "object"`. Root cause: every `#[tool]` returned `Json<Value>`; `schemars` maps `serde_json::Value` to a permissive schema with no root `type`, so `outputSchema` was not an object schema. Fix: tools return `Json<StructuredJson>`, a wrapper with a hand-written `JsonSchema` that always emits `{"type":"object","properties":{},"additionalProperties":true}`. Serialization is unchanged. Regression guard: `dbconnector-mcp` unit test `every_tool_output_schema_type_is_object` asserts all 35 registered tools advertise a root object schema.
- PostgreSQL catalog tools failed through the sqlx `Any` driver. `sql_list_databases`, `sql_list_schemas`, `sql_list_tables`, and `sql_describe_table` returned `error in Any driver mapping: Any driver does not support the Postgres type PgTypeInfo(Name)`; `sql_explain` returned the same mapping error for `PgTypeInfo(Json)`. Root cause: `information_schema` identifier columns have the PostgreSQL `name` type and `EXPLAIN (FORMAT JSON)` returns `json`, neither of which the `Any` driver can decode. Fix: cast every catalog identifier to `text` and ask PostgreSQL for the plan in text form. Regression guard: `postgres_metadata_tools_decode_catalog_types`.
- MySQL `sql_get_table_stats` failed with `Any driver does not support MySql type MySqlTypeInfo { type: NewDecimal, flags: ColumnFlags(NOT_NULL | BINARY), max_size: Some(23) }`. Root cause: MySQL 8.4 reports `table_rows`, `data_length`, and `index_length` as `DECIMAL`. Fix: `CAST(... AS SIGNED)`. Regression guard: `mysql_metadata_tools_decode_catalog_types`.
- MySQL transactions were unusable. `sql_begin_transaction` returned `QUERY_FAILED: error returned from database: 1295 (HY000): This command is not supported in the prepared statement protocol yet` for both `read_only` values, so `sql_commit_transaction` and `sql_rollback_transaction` could not be reached. Root cause: transaction control was issued as a prepared `sqlx::Query`, and MySQL rejects `START TRANSACTION` in the prepared-statement protocol. Fix: run transaction control through the driver's simple query path by executing a `&str`, which is the same path sqlx uses for its own transactions. A first attempt with `sqlx::raw_sql` compiled but made the server future fail the `Send` check (`implementation of Executor is not general enough`); executing `&str` avoids that. Regression guard: `mysql_transaction_commit_and_rollback` and `postgres_transaction_commit_and_rollback`.
- MySQL catalog output was inconsistent with PostgreSQL and SQLite: upper-case column labels (`TABLE_SCHEMA`, `COLUMN_NAME`) and `BLOB`-wrapped strings. Fix: explicit lower-case aliases plus `CAST(... AS CHAR)` for the catalog text columns that the `Any` driver reports as `BLOB`. `information_schema.columns.data_type` is still reported as binary by MySQL 8.4 even through `CONCAT`/`CONVERT`, so it keeps the documented `{encoding, text}` shape; the regression test asserts that explicitly instead of pretending it is normalized.
- `sql_list_databases` on MySQL used `SHOW DATABASES`, whose column label is `Database` and cannot be aliased. It now reads `information_schema.schemata`, so the column is lower case and matches `sql_list_schemas`.

Verified after the fixes:

- `DBCONNECTOR_INTEGRATION=1 cargo test -p dbconnector-mcp --test engines -- --test-threads=1`: 7 passed, including the four new regression tests.
- A stdio probe of `target/release/dbconnector serve --allow-writes --allow-destructive` ran 18 checks over real MCP JSON-RPC: `initialize`, 35 tools with object `outputSchema`, every PostgreSQL catalog tool, `sql_explain` with `executed: false`, MySQL catalog tools, MySQL rollback and commit round trips, `TRANSACTION_NOT_FOUND` for a finished transaction id, all connections closed, and no credential on stderr.

## Behavior notes

- Network policy defaults to `open`. `--allow-host` is rejected with that policy.
- Writes need `--allow-writes` and `read_only: false`. Destructive SQL and empty MongoDB filters also need `--allow-destructive` and `allow_destructive: true`. Three distinct gates were observed live: the server flag, the connection `read_only` flag, and the database account itself.
- A writable SQLite file URL is opened with `mode=rwc` so the file is created. A read-only file URL uses `mode=ro`.
- MySQL `TEXT` arrives as bytes through sqlx `Any`. Valid UTF-8 is returned as `{encoding: "utf8", text}`, other bytes as `{encoding: "base64", base64}`.
- PostgreSQL `sql_explain` returns the text plan, one row per plan line, in a `QUERY PLAN` column. JSON format cannot be decoded by the `Any` driver. MySQL returns the `EXPLAIN` table and SQLite returns `EXPLAIN QUERY PLAN` rows.
- `duration_ms` on tool results is `0`. Timeouts are still enforced around driver calls.
- Business errors use JSON-RPC tool errors. `data.code` carries the stable code. Most codes use the invalid-params error class. `TRANSACTION_NOT_FOUND` is returned for an unknown or already finished `transaction_id`.

## Commands run on this machine

Passed:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace` (32 tests; the live engine tests skip without `DBCONNECTOR_INTEGRATION=1`)
- `cargo build --workspace --release`
- `cargo check -p dbconnector --no-default-features`
- `DBCONNECTOR_INTEGRATION=1 cargo test -p dbconnector-mcp --test engines -- --test-threads=1` (7 tests)
- stdio probe of `target/release/dbconnector serve --allow-writes --allow-destructive`: initialize `2025-11-25`, 35 tools, 18 checks, no credential on stderr
- `tests/stdout_quiet.rs`: no stdout before the client writes

Live engines (127.0.0.1, fixture password `dbconnector_test`): PostgreSQL 16 :54329, MySQL 8 :33069, MongoDB 7 :27018, Redis 7 :63799, Valkey 8 :63809. Read and write paths were exercised through MCP for all five engines, including destructive gates, read-only accounts, MongoDB `$out`, Redis TTL and `SCAN`, and Valkey product detection.

Not run here: GitHub Actions, macOS build, Windows build, MCP Inspector, Claude, a Codex or OpenCode agent turn. `codex` 0.162.1 and `opencode` 2.0.29 are installed. Their MCP commands only manage saved server entries. No server was added to user config.

## Next

Run the GitHub workflows on macOS and Windows. Drive `tools/list` and one SQLite call from Codex and OpenCode without writing the password into a shared config. Measure `duration_ms` from the tool clock. Replace `EXPLAIN (FORMAT TEXT)` with a PostgreSQL-typed decode path if structured plans become a requirement. Consider a MariaDB container, which shares the MySQL driver path but not the MySQL 8.4 catalog types.
