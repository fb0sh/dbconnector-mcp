# TEST-PROMPT — 用已接入的 dbconnector MCP 打临时库

你是验收代理。用户已经把 **dbconnector** MCP 接到你的会话。你的工作是拉起本仓库的一次性测试库，然后只用 MCP 工具把 SQLite、PostgreSQL、MySQL、MongoDB、Redis、Valkey 各走一遍。不要改产品代码，不要注册或改写用户的 MCP 配置，不要连接清单以外的地址。

口令 `dbconnector_test` 是仓库里的夹具，只存在于本机临时容器。报告里写主机和库名即可，不要复述完整 URL。

## 0. 先确认 MCP

1. 列出当前可用工具。默认构建应有 35 个，名称以 `docs/tool-api.md` 为准。
2. 若没有 `open_connection`，停下来，说明 MCP 未接入。不要自己改客户端配置。
3. 写路径需要服务进程带 `--allow-writes`。破坏性语句还需要 `--allow-destructive`。先用下面的 SQLite 写操作探测。若返回权限错误，后面的写步骤标成「服务未授权」，只继续做读。不要为了让写成功去改启动参数，除非用户在本轮明确允许你重启 MCP。

调用顺序固定为：`open_connection` → `get_capabilities` → `get_connection_info` → 该引擎的工具 → `close_connection`。每个连接用完就关。`list_connections` 的结果里不得出现口令或完整 URL。

任一步失败就记下工具名、错误码和原文，然后继续下一类数据库。不要用猜测补成成功。

## 1. 拉起环境

在仓库根目录执行：

```bash
docker compose -f docker-compose.test.yml up -d --wait
docker compose -f docker-compose.test.yml ps
```

五个服务都应是 healthy：`postgres`、`mysql`、`mongo`、`redis`、`valkey`。某服务起不来就跳过该类，并附上 `ps` 与该服务日志的最后 40 行。SQLite 不依赖 Docker。

夹具已经写入，不要 `DROP` 这些对象：

| 引擎 | 地址 | 读写账号 | 只读账号 | 已有数据 |
| --- | --- | --- | --- | --- |
| PostgreSQL 16 | `127.0.0.1:54329/app` | `dbconnector` | `dbconnector_ro` | 表 `notes`，`id=1`，`body=fixture` |
| MySQL 8 | `127.0.0.1:33069/app` | `dbconnector` | `dbconnector_ro` | 同上 |
| MongoDB 7 | `127.0.0.1:27018`，库 `app`，`authSource=app` | `dbconnector` | `dbconnector_ro` | 集合 `notes`，文档 `body=fixture` |
| Redis 7 | `127.0.0.1:63799` | URL 不带账号（default） | `dbconnector_ro` | 无业务键 |
| Valkey 8 | `127.0.0.1:63809` | URL 不带账号 | 不必再测只读账号 | 无业务键 |

URL 形态：

- `postgres://dbconnector:dbconnector_test@127.0.0.1:54329/app?sslmode=disable`
- `postgres://dbconnector_ro:dbconnector_test@127.0.0.1:54329/app?sslmode=disable`
- `mysql://dbconnector:dbconnector_test@127.0.0.1:33069/app?ssl-mode=DISABLED`
- `mysql://dbconnector_ro:dbconnector_test@127.0.0.1:33069/app?ssl-mode=DISABLED`
- `mongodb://dbconnector:dbconnector_test@127.0.0.1:27018/app?authSource=app`
- `mongodb://dbconnector_ro:dbconnector_test@127.0.0.1:27018/app?authSource=app`
- `redis://127.0.0.1:63799`
- `redis://dbconnector_ro:dbconnector_test@127.0.0.1:63799`
- `redis://127.0.0.1:63809`
- SQLite 文件：`sqlite:///tmp/dbconnector-mcp-probe.db`

选一个短后缀 `<S>`（例如当前 unix 秒）。你创建的表、集合、键都带这个后缀，避免撞上夹具和别人的探测。

MySQL 文本列可能返回 `{encoding:"utf8", text:"..."}`，不要因此判失败。`duration_ms` 当前可以是 `0`。

## 2. SQLite

文件库，`read_only: false`。

1. `open_connection`。确认 `kind=sqlite`。
2. `get_capabilities`、`get_connection_info`。
3. `sql_execute`：`CREATE TABLE probe_<S> (id INTEGER PRIMARY KEY, body TEXT)`。
4. `sql_execute`：`INSERT INTO probe_<S> (body) VALUES (?)`，`params: ["alpha"]`。
5. `sql_query`：`SELECT body FROM probe_<S> WHERE id = ?`，`params: [1]`。结果应能读出 `alpha`。
6. `sql_describe_table`，`table` 为 `probe_<S>`。列里应有 `body`。
7. `sql_list_tables`。结果应含该表。
8. `sql_get_table_stats`。
9. `sql_explain`：`SELECT body FROM probe_<S> WHERE id = 1`。`executed` 应为 false。不要发 `EXPLAIN ANALYZE`。
10. `sql_begin_transaction`，`read_only: false`。在该 `transaction_id` 上 `INSERT` 一行 `rolled`，再 `sql_rollback_transaction`。之后 `SELECT COUNT(*)` 仍为 1。
11. 再 `sql_begin_transaction`，插入 `kept`，`sql_commit_transaction`。计数变为 2。
12. `sql_execute`：`DELETE FROM probe_<S>`，不带 `allow_destructive`。应拒绝。
13. 同一条 `DELETE` 加上 `allow_destructive: true`。仅当服务带了 `--allow-destructive` 时期望成功。
14. `sql_query`：`SELECT 1; SELECT 2`。应拒绝多语句。
15. 再 `open_connection` 同一文件，`read_only: true`。`CREATE TABLE` 应拒绝。`SELECT` 夹具行或 `SELECT 1` 应成功。
16. `list_connections`，然后关掉两个连接。

## 3. PostgreSQL

读写连接用 `dbconnector`，`read_only: false`。

1. `sql_query`：`SELECT body FROM notes WHERE id = 1`。应为 `fixture`。
2. `sql_list_databases`、`sql_list_schemas`、`sql_list_tables`（`schema: "public"`）、`sql_describe_table`（`schema: "public"`，`table: "notes"`）、`sql_get_table_stats`、`sql_explain`（`SELECT body FROM notes WHERE id = 1`）。
3. `sql_execute`：`CREATE TABLE probe_<S> (id integer PRIMARY KEY, body text)`，插入一行，`sql_query` 读回。
4. 事务：开始、插入、回滚，确认回滚后的行不在。再开始、插入、提交，确认提交后的行在。PostgreSQL 的 DDL 若放进事务，提交前别的连接可以看不见它；本项只要求同一 `connection_id` 上的回滚与提交。
5. `DELETE FROM probe_<S>` 不带 `allow_destructive` 应拒绝。不要对 `notes` 做无条件删除。
6. 另开 `dbconnector_ro`，`read_only: true`。`SELECT body FROM notes` 成功。`INSERT INTO notes` 应被服务器拒绝。不要期待只读账号能通过服务器写检查。
7. 关掉全部 Postgres 连接。

## 4. MySQL

步骤与 PostgreSQL 相同，库名仍是 `app`，表是 `notes` 和 `probe_<S>`。占位符用 `?`。

额外确认：`SELECT body FROM notes` 的单元格若是对象，则 `encoding` 为 `utf8` 且 `text` 为 `fixture`。`sql_explain` 的 `executed` 为 false。

只读账号同样只做 `SELECT`，写操作应被拒绝。不要 `DROP notes`。

## 5. MongoDB

读写连接，`database: "app"`。集合用 `probe_<S>`，不要改 `notes` 里的夹具文档。

1. `mongo_list_databases`。结果应含 `app`。
2. `mongo_list_collections`。应能看到 `notes`。
3. `mongo_find`，集合 `notes`，过滤 `{body: "fixture"}`。`returned` 至少为 1，文档在 `documents.items`。
4. `mongo_describe_collection`，集合 `notes`。字段类型来自抽样，记录实际返回，不要因为 `inferred: true` 判失败。
5. `mongo_list_indexes`，集合 `notes`。
6. `mongo_insert`，集合 `probe_<S>`，文档 `[{marker: "<S>", n: 1}]`。
7. `mongo_find` 按 `marker` 找回来。
8. `mongo_update`，过滤 `{marker: "<S>"}`，更新 `{$set: {n: 2}}`，`multi: false`。再 find，`n` 为 2。
9. `mongo_aggregate`，pipeline 只有 `{$match: {marker: "<S>"}}`。应返回该文档。
10. 同一条 pipeline 再加 `{$out: "stolen_<S>"}`。只读语义下应拒绝。若当前连接可写且服务有 `--allow-writes`，服务器可能允许；若允许了，记下实际行为，并用 `mongo_delete` 按过滤条件清掉 `stolen_<S>`，不要空过滤。
11. `mongo_delete`，集合 `probe_<S>`，过滤 `{}`，`multi: true`，不带 `allow_destructive`。应拒绝。
12. 再删 `{marker: "<S>"}`，`multi: true`。这是有条件删除，应成功。
13. 另开 `dbconnector_ro`，`read_only: true`。find `notes` 成功。`mongo_insert` 应拒绝。aggregate 含 `$out` 或 `$merge` 应拒绝。
14. 关掉两个连接。

## 6. Redis

连接 `redis://127.0.0.1:63799`，`read_only: false`。键前缀 `mcp:<S>:`。

1. `redis_info`。应有服务器信息，且不是 `CONFIG`。
2. `redis_set`，键 `mcp:<S>:s`，`text: "v1"`，`ex: 60`。
3. `redis_get`。`found` 为 true，值的文本是 `v1`。
4. `redis_key_info`。`ttl_seconds` 大于 0，类型为 string。
5. `redis_set`，键 `mcp:<S>:bin`，`base64` 为 `AAEC`（字节 `00 01 02`），不要同时传 `text`。`redis_get` 应为 base64，而不是被当成 UTF-8 文本。
6. `redis_hash`：`hset` 键 `mcp:<S>:h` 字段 `f` 文本 `hv`；`hget` 读回；`hgetall`；`hlen`。
7. `redis_list`：`rpush` 两次，`llen`，`lrange` 的 `start: 0`、`stop: 10`。
8. `redis_set_members`：`sadd` 成员文本 `m`，`sismember`，`scard`。
9. `redis_sorted_set`：`zadd` 成员文本 `z`、`score: 1.5`，`zrange`，`zcard`。
10. `redis_scan`，`cursor: "0"`，`pattern: "mcp:<S>:*"`，`count: 50`。把返回的 `next_cursor` 继续传回去，直到为 `"0"` 或已经扫到上述键。SCAN 可以重复键。
11. `redis_delete`，`keys` 为上面创建的键，`unlink: true`。
12. 另开 `redis://dbconnector_ro:...`，`read_only: true`。`redis_get` 一个不存在的键可以返回 `found: false`。`redis_set` 应被拒绝。
13. 关掉连接。

## 7. Valkey

对 `redis://127.0.0.1:63809` 重复 Redis 的 1–4 和 10–11，键前缀改成 `mcp:<S>:valkey:`。`redis_info` 的产品信息应能看出 valkey。不要对 Valkey 再跑只读账号。

## 8. 收尾

1. `list_connections`。若还有连接，逐个 `close_connection`。
2. 不要 `docker compose down`，除非用户要求拆除。容器是用户接下来可能继续看的环境。
3. 按下面格式作答。每一类一行结论：通过、失败、跳过。失败写工具名和错误原文。跳过写原因（服务没起来，或 MCP 没有写权限）。

```text
环境：compose ps 摘要
工具数：tools 列表数量
SQLite：
PostgreSQL：
MySQL：
MongoDB：
Redis：
Valkey：
只读拒绝：哪些连接的写操作被拒绝，哪些没有
破坏性门闩：无 allow_destructive 的删除是否被拒绝
未调用的工具：
与文档不一致的返回：
```
