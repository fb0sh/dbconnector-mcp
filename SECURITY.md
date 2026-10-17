# Security

## Credentials

`open_connection` takes `database_url`, which may contain a password. The MCP client and the model can retain that argument in logs and transcripts. This server does not persist the URL. It redacts userinfo passwords and `password`, `pwd`, `secret`, and `token` query parameters from errors and tracing. Connection listings store host, database name, engine, and read-only state.

Do not point the server at a production account that can do more than the task needs.

## Network

The default network policy is `open`: any host the operating system can route to is allowed, including public addresses. TLS follows the database URL. It is not required by default.

`--network-policy restricted` plus one or more `--allow-host` values limits hosts. If any allow-list entry is an IP address, every resolved address of the target must be listed. Host names match case-insensitively.

`--require-db-tls` accepts only verified TLS: PostgreSQL `sslmode=verify-full`, MySQL `ssl-mode=VERIFY_IDENTITY`, MongoDB `tls=true` without insecure flags, `mongodb+srv`, or `rediss` without `insecure=true`.

`--sqlite-root` confines file databases to that directory. In-memory SQLite is rejected when a root is set.

## Writes

Writes require both `--allow-writes` and `open_connection` `read_only: false`. A read-only connection cannot be upgraded.

`DROP`, `TRUNCATE`, `ALTER`, and `UPDATE`/`DELETE` without `WHERE` also require `--allow-destructive` and `allow_destructive: true`. MongoDB delete or multi-update with an empty filter uses the same gate. Aggregation `$out` and `$merge` are rejected on read-only connections and when the server was not started with `--allow-writes`, including nested pipelines.

Redis write actions (`SET`, delete, and mutating hash, list, set, and sorted-set actions) check the write gate again. `FLUSHALL`, `FLUSHDB`, `CONFIG`, `DEBUG`, `EVAL`, `MODULE`, and `ACL` are not tools.

## Results and time

Reads stop at the row limit and the byte limit. Timeouts return `QUERY_TIMEOUT`. If the driver cannot cancel the running statement, the error says the execution state may be unknown. A SQL transaction that times out is marked broken and a later commit rolls it back.

## stdout

stdin and stdout carry MCP JSON-RPC only. Tracing uses stderr and does not include the raw URL.
