# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.1.0

First public release.

### Added

- stdio-only MCP server `dbconnector` built on the official `rmcp` SDK. No HTTP, SSE, or
  remote transport.
- Connection tools: `open_connection`, `close_connection`, `list_connections`,
  `get_connection_info`, `get_capabilities`. Each connection returns an opaque
  `connection_id`, and several engines can be open at once.
- SQL tools for PostgreSQL, MySQL/MariaDB, and SQLite: schema discovery, `sql_query`,
  `sql_execute`, `sql_explain`, transactions, and table statistics.
- MongoDB tools with Extended JSON round trips: databases, collections, find, aggregate,
  insert, update, delete, and indexes.
- Redis and Valkey tools for `SCAN`, key metadata and TTL, strings, hashes, lists, sets,
  and sorted sets. Binary payloads use exactly one of `text` or `base64`.
- Default network policy `open`. Optional `--network-policy restricted`, `--allow-host`,
  `--require-db-tls`, and `--sqlite-root`.
- Writes stay off unless the server runs with `--allow-writes` and the connection is
  opened with `read_only: false`. Destructive statements and empty MongoDB filters also
  need `--allow-destructive` plus `allow_destructive: true` on the call.
- Feature flags `sql`, `mongo`, and `redis`. The default build registers 35 tools; a
  reduced build registers only the tools of the enabled features.
- Every tool `outputSchema` has root `type: "object"`, which strict MCP clients require
  on `tools/list`.
- Disposable engine stack in `docker-compose.test.yml` with read-write and read-only
  fixture accounts for all five engines.
- CI workflows for Linux, macOS, and Windows plus a tag-triggered release workflow.

### Fixed

- SQL catalog queries failed on PostgreSQL through the sqlx `Any` driver:
  `sql_list_databases`, `sql_list_schemas`, `sql_list_tables`, and `sql_describe_table`
  raised `Any driver does not support the Postgres type PgTypeInfo(Name)`, and
  `sql_explain` raised the same error for `PgTypeInfo(Json)`. Catalog columns are now cast
  to `text`, and the PostgreSQL plan is requested in text format, which the `Any` driver
  can decode.
- MySQL `sql_get_table_stats` failed with
  `Any driver does not support MySql type MySqlTypeInfo { type: NewDecimal, ... }` because
  MySQL 8.4 returns `table_rows` and the length columns as `DECIMAL`. They are now cast to
  `SIGNED`.
- MySQL transactions were unusable: `sql_begin_transaction` returned
  `1295 (HY000): This command is not supported in the prepared statement protocol yet`,
  which also made `sql_commit_transaction` and `sql_rollback_transaction` unreachable.
  Transaction control now uses the driver's simple query path, which MySQL accepts.
- MySQL catalog output was inconsistent with the other engines. Metadata columns are now
  lower case in every engine, and MySQL catalog text columns are cast to `CHAR` instead of
  being reported as `BLOB`, except `information_schema.columns.data_type`, which MySQL
  still reports as binary and therefore keeps the documented `{encoding, text}` wrapper.
- `sql_list_databases` on MySQL used `SHOW DATABASES`, whose column cannot be named. It now
  reads `information_schema.schemata`, matching `sql_list_schemas`.

### Known limitations

- `duration_ms` on tool results is always `0`. Timeouts are still enforced around driver
  calls.
- MySQL `TEXT` and `BLOB` are not distinguishable through sqlx `Any`. Valid UTF-8 bytes are
  returned as `{encoding: "utf8", text}` and other bytes as `{encoding: "base64", base64}`.
  A binary value whose bytes happen to be valid UTF-8 is therefore labelled `utf8`; the
  round trip is still lossless.
- SQLite reports `NULL` as the column type for expressions such as `SELECT 1`, because
  SQLite does not expose a type for computed columns.
- Catalog values are sampled where the engine offers no metadata: MongoDB
  `mongo_describe_collection` infers field types from a sample, and PostgreSQL
  `sql_get_table_stats` reads `pg_class` estimates that are accurate only after `ANALYZE`.
- MongoDB multi-document transactions and change streams are not exposed.
