# dbconnector

stdio-only MCP server for PostgreSQL, MySQL/MariaDB, SQLite, MongoDB, and Redis/Valkey.

The process speaks MCP JSON-RPC on stdin and stdout. Logs go to stderr. There is no HTTP or SSE transport.

## Install

```bash
cargo build --release
# binary: target/release/dbconnector
```

Rust 1.88 or newer. The default build includes SQL, MongoDB, and Redis. A smaller build:

```bash
cargo build --release --no-default-features --features sql
```

## Run

```bash
dbconnector serve
dbconnector serve --allow-writes
dbconnector serve --allow-writes --allow-destructive
dbconnector serve --network-policy restricted --allow-host db.internal --require-db-tls
dbconnector serve --sqlite-root /var/lib/dbconnector
```

Defaults: network policy `open`, connections read-only, 16 connection entries, SQL pool size 3, query timeout 10s, 100 rows (maximum 1000), 256 KiB results, idle connection TTL 30 minutes.

`--allow-host` is rejected when the network policy is `open`.

## Client registration

The server is selected with `open_connection`, then `get_capabilities`, then the engine tools. Credentials in `database_url` can be stored by the MCP client and by the model transcript. Prefer a short-lived URL and a least-privilege database account.

### Codex

```toml
[mcp_servers.dbconnector]
command = "/absolute/path/dbconnector"
args = ["serve"]
```

Writable sessions add `"--allow-writes"` to `args`.

### OpenCode

```json
{
  "mcp": {
    "dbconnector": {
      "type": "local",
      "command": ["/absolute/path/dbconnector", "serve"]
    }
  }
}
```

### Claude Code

`claude` is not installed in the development environment used for this repository. The same stdio command applies:

```bash
claude mcp add dbconnector -- /absolute/path/dbconnector serve
```

## Example calls

SQLite file (created when the connection is writable):

1. `open_connection` with `database_url` `sqlite:///tmp/app.db` and `read_only` `false` (server started with `--allow-writes`).
2. `sql_execute` `CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)`.
3. `sql_query` `SELECT body FROM notes`.
4. `close_connection`.

MongoDB: `mongo_aggregate` with a pipeline. Stages `$out` and `$merge` are rejected unless the server and the connection allow writes.

Redis: `redis_set` with `text` or `base64` and `ex` for TTL, then `redis_key_info`. Binary values come back as `{encoding, text}` or `{encoding, base64}`. Key iteration uses `redis_scan`.

## Tests

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
```

Live engines:

```bash
docker compose -f docker-compose.test.yml up -d --wait
DBCONNECTOR_INTEGRATION=1 cargo test -p dbconnector-mcp --test engines -- --test-threads=1
docker compose -f docker-compose.test.yml down -v
```

The compose passwords are fixtures (`dbconnector_test`) for disposable containers.

## Status

`0.1.0`. The default build registers 35 tools. Verified on Linux: `fmt`, clippy with
`-D warnings`, the full test suite, a release build, the live engine tests against
disposable PostgreSQL 16, MySQL 8, MongoDB 7, Redis 7, and Valkey 8 containers, and a
stdio JSON-RPC probe of the release binary that runs every catalog tool and a MySQL
transaction round trip. macOS and Windows builds are wired into CI but have not run yet,
and the tools have not been driven from a real agent client in this environment.
Details and the remaining risks: [docs/implementation-status.md](docs/implementation-status.md)
and [docs/acceptance-report.md](docs/acceptance-report.md).

## License

MIT OR Apache-2.0, see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).

Tool schemas: [docs/tool-api.md](docs/tool-api.md). Threat model: [SECURITY.md](SECURITY.md).
