# Acceptance report

Date: 2026-10-17. Scope: `dbconnector` 0.1.0 in this workspace. M5 is not claimed.

## Passed

- Default stdio server answers MCP `initialize` (`2025-11-25`) and `tools/list` with 35 tools. Server name `dbconnector`, version `0.1.0`. Stderr stayed empty for that handshake.
- `dbconnector serve` writes nothing to stdout during 500ms with no client input (`tests/stdout_quiet.rs`).
- SQLite file round trip through an in-process NDJSON session: create, insert, select, describe, explain without execute, transaction rollback, destructive delete gate, row limit, close. Read-only connection rejects `CREATE`. `--require-db-tls` plus a restricted host rejects a Postgres URL before connect, and the error does not contain the URL password.
- Live Docker engines, one run: Postgres, MySQL, MongoDB, Redis, Valkey. Read-only Postgres and Redis accounts reject writes. MongoDB `$out` is rejected on a read-only connection.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and a release build succeeded on Linux. `cargo test --workspace` skips the live engines unless `DBCONNECTOR_INTEGRATION=1`. With that variable unset, `tests/engines.rs` returns immediately and does not open a database.
- Feature-off build compiles. Its tool list is the five connection tools. SQL-only build keeps SQL tools off MongoDB and Redis.

### Write and destructive round over MCP

A second acceptance round ran with `serve --allow-writes --allow-destructive` and exercised
the write path of all five engines through the MCP tools, plus one full read-only round with
`serve` and no write flags.

- SQLite: create, parameterized insert, read back, describe, table stats, `EXPLAIN` with
  `executed: false`, `EXPLAIN ANALYZE` rejected, multi-statement query rejected, read-only
  statement enforcement on `sql_query`, transaction rollback and commit, `DELETE` rejected
  without `allow_destructive` and accepted with it, read-only connection rejecting writes.
- PostgreSQL: create, parameterized insert, read back, rollback and commit, destructive gate,
  `DROP TABLE` with the flag, read-only account rejected by the server and, on a writable
  connection, by the database account. `TRANSACTION_NOT_FOUND` for a finished transaction id.
- MySQL: create, parameterized insert, read back, rollback and commit after the transaction
  fix, destructive gate, read-only account rejected by the server and by the database account
  (`1142 INSERT command denied`).
- MongoDB (writable connection): insert, find, update with `multi: false`, aggregate, empty
  filter update and delete rejected without `allow_destructive` and accepted with it,
  filtered delete accepted, `$out` allowed and its output collection cleaned up with a
  filtered `mongo_delete`, read-only connection rejecting insert and `$out`/`$merge`.
- Redis: TTL key, hash, list, set, sorted set, `SCAN` loop, `UNLINK`, read-only account
  rejected by the server and by the ACL user (`NOPERM`).
- Valkey: same read and write path, `redis_info` reporting `product: valkey`.

### Regression round after the catalog and transaction fixes

- `DBCONNECTOR_INTEGRATION=1 cargo test -p dbconnector-mcp --test engines -- --test-threads=1`
  passes 7 tests, including `postgres_metadata_tools_decode_catalog_types`,
  `mysql_metadata_tools_decode_catalog_types`, `postgres_transaction_commit_and_rollback`,
  and `mysql_transaction_commit_and_rollback`.
- A stdio probe of the release binary ran 18 checks over real MCP JSON-RPC. All passed:
  `initialize`, 35 tools with object `outputSchema`, all five PostgreSQL catalog tools plus
  `sql_explain`, MySQL catalog tools, MySQL rollback and commit round trips,
  `TRANSACTION_NOT_FOUND`, all connections closed, and no credential on stderr.

## Not passed

Nothing in the commands above failed on the final run. Earlier failures (strict-client
`outputSchema`, MySQL text decoding, PostgreSQL catalog decoding, MySQL table statistics,
MySQL transactions) were fixed and now have regression tests.

## Not executed

- GitHub Actions jobs in `.github/workflows/ci.yml` and `release.yml`.
- macOS and Windows builds.
- MCP Inspector. The `mcp-inspector` command is not installed. `npx` is present and was not used to download it.
- Claude Code. `claude` is not on `PATH`.
- A Codex or OpenCode conversation that discovers and calls a tool. The CLIs are installed (`codex` 0.162.1, `opencode` 2.0.29). No MCP entry was written into user configuration.
- Release checksum artifacts. The workflow is present and was not dispatched.
- MariaDB as a separate server. MySQL 8 was used. The MySQL driver path is the one MariaDB would use.

## Risks that remain

- `duration_ms` is always `0`.
- Tool failures are JSON-RPC errors, not `CallToolResult` structured errors.
- MySQL `TEXT` and `BLOB` are not distinct in sqlx `Any`. Valid UTF-8 is labeled text, so a
  binary value whose bytes are valid UTF-8 is labelled `utf8`. Round trips stay lossless.
- MySQL `information_schema.columns.data_type` is reported as a binary column by MySQL 8.4,
  so it keeps the `{encoding, text}` wrapper even after `CAST`/`CONVERT`.
- PostgreSQL `sql_explain` returns the text plan because the `Any` driver cannot decode the
  `json` column of `EXPLAIN (FORMAT JSON)`.
- SQLite reports `NULL` as the column type for computed columns such as `SELECT 1`.
- MongoDB `describe_collection` field types come from a sample. `DbPointer` is emitted as a placeholder because the driver fields are private.
- The idle-connection reaper starts inside `DbServer::new`.

## How to repeat the live engines

```bash
docker compose -f docker-compose.test.yml up -d --wait
DBCONNECTOR_INTEGRATION=1 cargo test -p dbconnector-mcp --test engines -- --test-threads=1
docker compose -f docker-compose.test.yml down -v
```
