# Tool API

All tools are registered on the default build. Arguments are JSON objects. Successful calls return a JSON object in MCP `structuredContent`. The server adds `duration_ms` when the object does not already have it. That field is currently `0`; driver timeouts are still enforced.

`open_connection.database_url` may contain a password. Clients can retain it. See SECURITY.md.

## Connection

| Tool | Arguments | Result |
| --- | --- | --- |
| `open_connection` | `database_url`, `read_only` (default true) | `connection_id`, `kind`, `status`, `read_only`, `server_version`, `capabilities` |
| `close_connection` | `connection_id` | `closed` |
| `list_connections` | none | `connections[]` without URL or password |
| `get_connection_info` | `connection_id` | version, write flags, row and byte limits |
| `get_capabilities` | `connection_id` | `features`, `limitations` |

Schemes: `postgres`, `postgresql`, `mysql`, `sqlite`, `mongodb`, `mongodb+srv`, `redis`, `rediss`.

## SQL

Shared arguments: `connection_id`. Reads take optional `limit` and `timeout_ms`. Writes take optional `transaction_id`, `timeout_ms`, and `allow_destructive`.

| Tool | Extra arguments |
| --- | --- |
| `sql_list_databases` | |
| `sql_list_schemas` | `database` |
| `sql_list_tables` | `schema`, `include_views` (default true) |
| `sql_describe_table` | `schema`, `table` |
| `sql_query` | `sql`, `params`, `transaction_id`, `limit`, `timeout_ms` |
| `sql_execute` | `sql`, `params`, `transaction_id`, `allow_destructive`, `timeout_ms` |
| `sql_explain` | `sql`, `params`. Does not run `EXPLAIN ANALYZE`. |
| `sql_begin_transaction` | `read_only` |
| `sql_commit_transaction` | `transaction_id` |
| `sql_rollback_transaction` | `transaction_id` |
| `sql_get_table_stats` | `schema`, `table` |

`sql_query` accepts one read-only statement. `params` is an array of JSON scalars or `{t, v}` objects (`null`, `bool`, `int`, `float`, `decimal`, `text`, `bytes`, `json`). Decimal values stay strings. Bytes are returned as `{encoding, base64}`.

`sql_query` and `sql_execute` rows use `columns`, `rows`, `returned_rows`, `truncated`, `warnings`. MySQL `TEXT` is exposed by the driver as bytes: valid UTF-8 is `{encoding: "utf8", text}` and other bytes are `{encoding: "base64", base64}`. Because MySQL cannot distinguish `TEXT` from `BLOB` through the driver, a binary value whose bytes happen to be valid UTF-8 is labelled `utf8`; decoding either form gives the original bytes back.

Catalog results:

- Catalog column labels are lower case in every engine.
- PostgreSQL catalog identifiers are cast to `text` because the sqlx `Any` driver cannot decode the `name` type.
- MySQL catalog text columns are cast to `CHAR` so they are returned as strings instead of `BLOB`. The exception is `information_schema.columns.data_type`, which MySQL 8.4 reports as a binary column, so `sql_describe_table` values keep the `{encoding, text}` wrap on MySQL.
- `sql_explain` never executes the statement and always returns `executed: false`. `plan` is the engine's own shape: PostgreSQL returns one text line per row in a `QUERY PLAN` column (JSON format is not decodable through the `Any` driver), MySQL returns the `EXPLAIN` table, and SQLite returns `EXPLAIN QUERY PLAN` rows.
- `sql_list_databases` reads the catalog on MySQL, so it returns the same lower-case `name` column as the other engines.

## MongoDB

`database` is optional when the URL has a default database. Documents and filters are Extended JSON objects.

| Tool | Arguments |
| --- | --- |
| `mongo_list_databases` | `connection_id` |
| `mongo_list_collections` | `connection_id`, `database` |
| `mongo_describe_collection` | `connection_id`, `database`, `collection` |
| `mongo_find` | `filter`, `projection`, `sort`, `limit`, `skip` |
| `mongo_aggregate` | `pipeline` |
| `mongo_insert` | `documents` |
| `mongo_update` | `filter`, `update`, `upsert`, `multi`, `allow_destructive` |
| `mongo_delete` | `filter`, `multi`, `allow_destructive` |
| `mongo_list_indexes` | `connection_id`, `database`, `collection` |

Find and aggregate return `documents.items`, `documents.truncated`, and `returned`.

## Redis / Valkey

String payloads use exactly one of `text` or `base64`. Key iteration is `SCAN`.

| Tool | Arguments |
| --- | --- |
| `redis_scan` | `cursor` (default `0`), `pattern`, `count` |
| `redis_key_info` | `key`. Returns type, `ttl_seconds`, `pttl_millis` |
| `redis_get` | `key` |
| `redis_set` | `key`, `text` or `base64`, `ex`, `px`, `nx`, `xx` |
| `redis_delete` | `keys`, `unlink` |
| `redis_hash` | `action` `hget` `hset` `hdel` `hgetall` `hlen` `hexists` `hscan`, `key`, `field`, `text` or `base64`, `cursor`, `count` |
| `redis_list` | `action` `lpush` `rpush` `lpop` `rpop` `lrange` `llen`, `key`, `text` or `base64`, `start`, `stop` |
| `redis_set_members` | `action` `sadd` `srem` `sismember` `scard` `sscan`, `key`, `text` or `base64`, `cursor`, `count` |
| `redis_sorted_set` | `action` `zadd` `zrem` `zrange` `zcard`, `key`, `member_text` or `member_base64`, `score`, `start`, `stop` |
| `redis_info` | `connection_id` |

Mutating actions require a writable connection. Payloads use exactly one of `text` or `base64`; passing both or neither is `INVALID_ARGUMENT`. `redis_set_members` names its member payload `text`/`base64`, while `redis_sorted_set` names it `member_text`/`member_base64` and takes a numeric `score`. `redis_get` returns `found`, `value`, and the echoed `key`; a value is `{encoding: "utf8", text}` when the bytes are valid UTF-8 and `{encoding: "base64", base64}` otherwise. `hscan` and `sscan` return the raw two-element `SCAN` reply, `[cursor, members]`, with every string wrapped in the same `{encoding, ...}` form.

## Errors

Tool failures are JSON-RPC errors whose `data.code` is one of:

`INVALID_ARGUMENT`, `UNSUPPORTED_ENGINE`, `UNSUPPORTED_OPERATION`, `CONNECTION_FAILED`, `CONNECTION_NOT_FOUND`, `PERMISSION_DENIED`, `QUERY_TIMEOUT`, `QUERY_FAILED`, `RESOURCE_LIMIT_EXCEEDED`, `TRANSACTION_NOT_FOUND`, `TRANSACTION_CONFLICT`, `INTERNAL_ERROR`.

`TRANSACTION_NOT_FOUND` is returned for an unknown or already finished `transaction_id`. `TRANSACTION_CONFLICT` is returned when a transaction is already running a statement, or when it must be rolled back because an earlier statement failed or timed out.
