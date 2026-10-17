# Architecture

```text
dbconnector (CLI, stderr tracing)
  dbconnector-mcp     rmcp stdio, tool schemas, connection map
    dbconnector-core  URL parse, network policy, redaction, errors, limits
    dbconnector-sql   sqlx Any pool, statement classifier
    dbconnector-mongo official mongodb driver, Extended JSON
    dbconnector-redis redis crate ConnectionManager
```

The MCP crate owns `AppState`. Each entry holds one backend. The map lock is not held across database I/O. A global semaphore and a per-connection semaphore bound concurrency. A background task drops idle connections and expired SQL transactions.

SQL transactions pin one `PoolConnection`. A second statement on the same `transaction_id` returns `TRANSACTION_CONFLICT`. Placeholders are `$1` for PostgreSQL and `?` for MySQL and SQLite. Results are streamed until the row or byte limit. The server does not append `LIMIT` to user SQL.

MongoDB values cross the tool boundary as Extended JSON. Redis values are bytes. UTF-8 is tagged `utf8`; other bytes are standard Base64.

`tools/list` is generated from the `#[tool]` methods compiled into the binary. Disabling a feature removes that engine's tools.

Business failures are JSON-RPC tool errors whose `data` object has `code` and `message`. Codes include `INVALID_ARGUMENT`, `CONNECTION_FAILED`, `CONNECTION_NOT_FOUND`, `PERMISSION_DENIED`, `QUERY_TIMEOUT`, `QUERY_FAILED`, `RESOURCE_LIMIT_EXCEEDED`, `TRANSACTION_NOT_FOUND`, `TRANSACTION_CONFLICT`, `UNSUPPORTED_OPERATION`, `UNSUPPORTED_ENGINE`, and `INTERNAL_ERROR`.
