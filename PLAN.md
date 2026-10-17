# dbconnector — Rust 多数据库 MCP Server 实施计划

> 状态：V2 实施基线（2026-10-17；替代上一版计划）  
> 目标读者：Coding Agent / 项目维护者  
> 交付目标：可独立编译、测试、发布的 stdio-only MCP Server；支持运行时动态连接 SQL、MongoDB、Redis/Valkey，使用 `connection_id` 复用连接。  
> 执行原则：按阶段连续交付；以开箱即用的 stdio MCP + 动态 Database URL 为优先级，默认网络开放，安全限制由用户按需启用。

## 0. 产品定位与已确认决策

项目名称：**dbconnector**。

- 开发语言：Rust，异步运行时 Tokio。
- MCP：官方 `rmcp` SDK，仅启用 **stdio Server**；`stdin/stdout` 专用于 MCP JSON-RPC，日志写 `stderr`。
- SQL：`sqlx`，首批支持 PostgreSQL、MySQL/MariaDB、SQLite；运行时使用 `AnyPool`，Schema 与数据库差异由专用适配器处理。
- 文档数据库：MongoDB 官方 Rust Driver (`mongodb`)。
- 键值数据库：`redis-rs` (`redis`)，兼容 Redis/Valkey 的基本操作。
- 连接入口：`open_connection(database_url, read_only?)`；建立真实连接并返回不透明、随机的 `connection_id`。
- 会话模型：一个 MCP Server 进程持有多个独立连接；所有后续工具通过 `connection_id` 关联连接。进程重启后原 ID 失效。
- 凭据模型：V1 接受 URL 中的用户名和密码，**无需 TOML/环境变量预设数据库**；不持久化凭据，且明确告知 URL 可能出现在模型上下文、MCP Client 日志与会话记录中。未来预留 `credential_ref`，不影响 V1 入参。
- 默认网络：**open**（本机、局域网、VPN、域名和公网数据库均允许），不要求 `--allow-host`；限制目标网络与强制 TLS 是可选的 hardened 模式。
- 默认写权限：**只读**。写入需要运行 MCP 进程的用户通过 `--allow-writes` 授权，连接声明 `read_only:false`，并受数据库账户权限约束。
- 发布：优先单一 `dbconnector` 可执行文件（Linux/macOS/Windows）；只做 stdio，不提供 HTTP、Web UI、守护服务。
- Agent 发现：MCP Client 启动进程并读取 `tools/list`；所有 Tool 配备准确描述、输入 JSON Schema 和典型使用场景，无需 Skill；额外 `AGENTS.md` 为可选增强。

### 0.1 范围

**必须交付**：多连接、自动驱动识别、连接健康检查、数据库结构发现、SQL 查询与受控写入、SQL 事务、MongoDB 基础 CRUD/聚合、Redis 常用数据类型、统一结构化输出、限制与审计、自动化测试、发布文档。

**后续扩展**：SQL Server、Oracle、Elasticsearch、OpenSearch、Neo4j、Cassandra；MongoDB Change Streams、Redis Streams / PubSub、批量数据导入导出、持久化凭据、远程 MCP。扩展通过新 Adapter 和按 Feature 注册的工具组完成。

### 0.2 非功能目标

1. 本地构建成功、跨平台可运行，使用 MCP Inspector 与至少两种客户端测试。
2. 连接池初始化、查询执行、资源清理均受超时与并发限制。
3. stdout 零污染；无凭据、明文 DSN、敏感行数据的非必要日志。
4. 所有工具有稳定 JSON Schema、统一错误代码及可机器解析的结果。
5. 默认只读，但**默认允许任何可达数据库主机与 URL 显式指定的 TLS 设置**；分别测试开放网络与可选限制模式。
6. 维护明确的 Rust MSRV、依赖锁定与 CI 测试矩阵；开始实现时选用当时相互兼容的稳定依赖版本。

---

## 1. 架构与模块边界

```text
MCP Client (Codex / Claude Code / OpenCode / 自研 Agent)
             │ JSON-RPC over stdio
             ▼
    dbconnector-mcp (rmcp Tool Router)
             │ 参数验证 / 结构化错误
             ▼
    dbconnector-core
    ├── ConnectionManager  ── connection_id -> ConnectionHandle
    ├── CapabilityRegistry ── 数据库能力 / 启用工具组
    ├── PolicyEngine       ── 只读/写入/主机/路径/结果策略
    ├── QueryLimiter       ── 超时、并发、行数、字节数
    ├── TransactionManager ── 独占物理连接和事务生命周期
    └── ResultNormalizer   ── 类型保真、结构化结果、脱敏
             │
      DatabaseAdapter（能力分层）
       ├── dbconnector-sql   -> SQLx (Postgres / MySQL / SQLite)
       ├── dbconnector-mongo -> mongodb Rust Driver
       └── dbconnector-redis -> redis-rs (Redis / Valkey)
```

### 1.1 Cargo Workspace

```text
dbconnector/
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml
├── README.md
├── PLAN.md
├── SECURITY.md
├── CHANGELOG.md
├── .github/workflows/ci.yml
├── .github/workflows/release.yml
├── crates/
│   ├── dbconnector-core/
│   │   └── src/{connection,capabilities,policy,result,error,limits,transaction}.rs
│   ├── dbconnector-mcp/
│   │   └── src/{server,tools/{common,sql,mongo,redis}}.rs
│   ├── dbconnector-sql/
│   │   └── src/{lib,postgres,mysql,sqlite,metadata,query,types}.rs
│   ├── dbconnector-mongo/
│   │   └── src/{lib,documents,bson,indexes}.rs
│   └── dbconnector-redis/
│       └── src/{lib,keys,types,scan}.rs
├── src/main.rs
├── examples/mcp-client-config.json
├── tests/contract/
├── tests/integration/
└── docker-compose.test.yml
```

可先建立完整 Workspace 骨架，模块随着阶段推进落地。避免过早引入数据库驱动之外的服务框架。

### 1.2 核心类型（设计草案）

```rust
pub enum DatabaseKind { Postgres, MySql, Sqlite, MongoDb, Redis }

pub enum ConnectionBackend {
    Sql(sqlx::AnyPool),
    Mongo(mongodb::Client),
    Redis(redis::aio::ConnectionManager),
}

pub struct ConnectionHandle {
    pub id: ConnectionId,
    pub kind: DatabaseKind,
    pub backend: ConnectionBackend,
    pub policy: EffectivePolicy,
    pub created_at: std::time::Instant,
    pub last_used_at: std::time::Instant,
    // 仅保留脱敏连接元数据；严禁将原始 DSN 写入 Debug/日志。
}

pub struct ConnectionManager {
    // HashMap<ConnectionId, Arc<ConnectionHandle>>
    // 真实实现建议 tokio::sync::RwLock + 单连接操作细粒度锁。
}
```

- `ConnectionId` 采用 UUID v4 或同等级不可预测值，`conn_` 前缀仅用于识别格式。
- 不透明 ID 只是会话内引用，**不是鉴权令牌**。授权由本地 MCP Client 进程权限、连接策略与数据库服务端权限共同保证。
- 使用 `Arc` 共享连接句柄，短暂获取 map 锁后立即释放；整个网络操作期间不得持有全局读写锁。
- 并发打开同一 URL 不自动合并账户会话；每次打开返回不同 ID，或在明确设计幂等 API 后再增加复用策略。
- SQLx `AnyPool` 需在启动阶段调用 `sqlx::any::install_default_drivers()`。
- MongoDB `Client` 共享并发使用；创建后执行 ping 验证连通性。
- Redis 使用异步 ConnectionManager 并执行 PING；对 blocking 命令未来考虑专用连接。
- SQLx `Any` 的动态绑定和类型解码存在驱动差异：在 `sql` 层显式封装绑定与结果类型转换，覆盖各驱动的集成测试。

### 1.3 Adapter 能力模型

统一实现 `ConnectionAdapter`：`kind()`、`capabilities()`、`ping()`、`close()`；按数据模型实现 `SqlAdapter`、`DocumentAdapter`、`KeyValueAdapter`。MCP 层使用带类型校验的后端分派，不强制用一个 SQL 风格的 Query Trait 表达 Mongo/Redis。

---

## 1.4 Agent 自动发现与使用策略（不可遗漏）

1. MCP Client 注册 `dbconnector serve`，通过 stdio 拉起进程；服务仅从 stdin 读取 MCP 消息，并仅在 stdout 写 MCP 消息。
2. 服务应同时兼容 SDK 已支持的 MCP `2026-07-28` 与主流客户端仍可能使用的 `2025-11-25`。**不得把旧版 `initialize` 握手写成所有客户端必须执行的步骤**；以协商的协议版本和 `rmcp` 官方实现为准。
3. Client 通过 `tools/list` 获取可用工具的 `name`、`description`、`inputSchema`、`outputSchema` 与行为 annotations；具体哪一步由 Agent 触发，受模型和客户端工具审批配置影响。
4. Tool descriptions 必须以动词开头，写清适用数据库、何时调用、必填参数、只读/写入属性；`open_connection` 描述须覆盖 SQL、MongoDB、Redis 及 `connection_id` 的后续复用方式。
5. MCP Server 提供 server instructions，简明建议工作流：`open_connection` → `get_capabilities` → 模型相关工具 → `close_connection`（后者按任务生命周期可省略，由空闲超时清理）。
6. **35 个 Tools 是完整 V1 验收目标**。阶段开发期间 `tools/list` 只列真实已实现的工具；不得声明未实现的功能。支持按编译 Feature/启动选项禁用整个工具组，不依据每个 connection_id 动态改变完整工具表。
7. 增加工具发现与 Tool Description 测试：输入自然语言“看看 PG 表结构”“MongoDB 聚合”“Redis 查看 TTL”，至少在两个真实 Client 上观察是否选择了正确工具。工具选择是模型行为，测试用命中率而非绝对保证。

推荐的关键工具描述（英文或双语，最终保持简短）：

```text
open_connection: Connect to a PostgreSQL, MySQL, SQLite, MongoDB,
Redis or Valkey database using a database_url. Call this first
when the user asks to access a database. Return a connection_id
for subsequent database tools. Credentials in the URL may enter
client logs or model context.
sql_query: Read rows from a SQL database using an existing
connection_id. Use for SELECT queries and schema-informed analysis.
mongo_find: Read MongoDB documents using an existing connection_id.
redis_scan: Discover Redis keys incrementally using SCAN and cursors.
```

---

## 2. MCP Tool API（稳定命名）

V1 的 **35 个工具**分模块逐步落地。默认完整构建启用 `sql,mongo,redis`；也允许按构建 Feature 或启动配置关闭某工具组。客户端 `tools/list` 只暴露当前实际可用的工具。每个工具都须定义：描述、JSON Schema、输入示例、输出示例、错误码、只读/破坏性 annotation、限制策略。

### 2.1 通用连接工具：5 个（M0）

| Tool | 输入 | 输出 / 行为 |
|---|---|---|
| `open_connection` | `database_url`, `read_only?=true` | 连接并主动验证，返回 `connection_id`, `kind`, `server_version?`, `status`, `capabilities` |
| `close_connection` | `connection_id` | 释放连接及关联事务/游标资源，返回 `closed` |
| `list_connections` | 无 | 仅显示当前进程连接：ID、类型、脱敏主机/库、状态、创建时间 |
| `get_connection_info` | `connection_id` | 引擎、版本、健康状态、有效权限、限制，不回显 URL 或密码 |
| `get_capabilities` | `connection_id` | 数据库功能与启用工具组，说明版本/部署限制 |

**统一 URL**：`postgres://`、`postgresql://`、`mysql://`、`sqlite://`、`mongodb://`、`mongodb+srv://`、`redis://`、`rediss://`。校验 Scheme、地址、数据库类型与参数。SQLite 使用驱动接受的 URL 格式并测试绝对/相对路径。URL 密码需遵循百分号编码规则。

**`open_connection` 请求**：

```json
{"database_url":"postgresql://app_user:***@localhost:5432/app?sslmode=verify-full","read_only":true}
```

**`open_connection` 响应**：

```json
{"connection_id":"conn_827c5a...","kind":"postgres","status":"connected","read_only":true,"capabilities":["schema","query","explain"]}
```

`***` 在文档中仅为占位符，真实客户端调用会传入原始 URL；该入参具有敏感性，参见第 5 节。

### 2.2 SQL 工具：11 个（M1、M2）

| Tool | 核心输入 | 阶段 |
|---|---|---|
| `sql_list_databases` | `connection_id` | M1 |
| `sql_list_schemas` | `connection_id`, `database?` | M1 |
| `sql_list_tables` | `connection_id`, `schema?`, `include_views?` | M1 |
| `sql_describe_table` | `connection_id`, `schema?`, `table` | M1 |
| `sql_query` | `connection_id`, `sql`, `params?`, `transaction_id?`, `limit?`, `timeout_ms?` | M1 |
| `sql_execute` | `connection_id`, `sql`, `params?`, `transaction_id?` | M2 |
| `sql_explain` | `connection_id`, `sql`, `params?` | M2 |
| `sql_begin_transaction` | `connection_id`, `read_only?` | M2 |
| `sql_commit_transaction` | `transaction_id` | M2 |
| `sql_rollback_transaction` | `transaction_id` | M2 |
| `sql_get_table_stats` | `connection_id`, `schema?`, `table` | M2 |

约束：

- PostgreSQL、MySQL、SQLite 的系统表和元数据能力分别实现；不假设三者有相同的 Schema 层级。
- `sql_query` 只接受单条只读语句；对多语句、会话设置、DML/DDL、存储过程、`SELECT ... FOR UPDATE` 等做限制。AST 解析用于分类；数据库只读账号、只读事务等作为强制边界。
- 参数化查询逐类型绑定；参数占位符遵循原生方言（PostgreSQL `$1`、MySQL `?` 等）。以类型标签解决整数、布尔、decimal、timestamp、bytes、JSON 的跨驱动歧义。
- 默认限制：100 行，最大 1000 行，结果最多 256 KiB，单次查询默认 10s；使用服务端限制、客户端读取截断、超时取消组合执行，不能仅依赖 SQL 字符串自动追加 LIMIT。
- `sql_execute` 受服务端写入授权限制；返回 `rows_affected` 与状态；DDL 和数据库管理员级操作进一步分级。
- `sql_explain` 默认不执行 SQL，仅计划分析；`EXPLAIN ANALYZE` 保留为未来显式授权的扩展。
- SQL 事务需要固定同一物理连接；`sql_query` 与 `sql_execute` 均接受可选 `transaction_id`，不传时从普通连接池执行。TransactionManager 管理独占连接、串行事务语句、超时回滚与异常中止。SQLx 的事务生命周期须以安全所有权模型实现，避免持有全局锁跨 `await`。数据库 DDL 的隐式提交差异需在文档声明。
- SQL Server / Oracle 扩展新驱动，不改变上述 SQL Tool 契约，但通过 capabilities 暴露差异。

**`sql_query` 请求**：

```json
{"connection_id":"conn_827c5a...","sql":"SELECT id, name FROM users WHERE id > $1 ORDER BY id LIMIT 10","params":[100],"limit":100}
```

**`sql_query` 响应**：

```json
{
  "columns":[{"name":"id","type":"int8"},{"name":"name","type":"text"}],
  "rows":[[101,"alice"],[102,"bob"]],
  "returned_rows":2,
  "truncated":false,
  "duration_ms":14
}
```

### 2.3 MongoDB 工具：9 个（M3）

| Tool | 功能 |
|---|---|
| `mongo_list_databases` | 列出当前账号可访问的数据库 |
| `mongo_list_collections` | 列出集合与视图 |
| `mongo_describe_collection` | 索引/验证器/采样推断字段；结果明确标记“推断” |
| `mongo_find` | filter、projection、sort、limit、skip |
| `mongo_aggregate` | 聚合 pipeline 与结果限制 |
| `mongo_insert` | 单条或批量插入 |
| `mongo_update` | 更新、upsert、多文档模式（显式参数） |
| `mongo_delete` | 按条件删除（显式单条/多条限制） |
| `mongo_list_indexes` | 获取索引定义 |

- 内部用 BSON / Extended JSON 处理 `ObjectId`、`Date`、`Decimal128`、`Binary`，保证类型保真。
- `mongo_aggregate` 读取模式禁止 `$out`、`$merge`，需要递归检查可能嵌套的子管道；写入模式也走强制策略。
- MongoDB 命令接口 `run_command` 暂不向 Agent 暴露，避免通过泛化命令绕过限制。
- `mongo_find`、聚合、文档采样有超时、文档数及响应字节限制；禁止无边界返回整个集合。
- MongoDB Transaction、Bulk Write、Change Streams 放在扩展阶段，依据 MongoDB 部署类型检查 capabilities。

### 2.4 Redis / Valkey 工具：10 个（M4）

| Tool | 功能 |
|---|---|
| `redis_scan` | SCAN 游标、MATCH、COUNT 参数；避免全量 KEYS 阻塞 |
| `redis_key_info` | TYPE、TTL/PTTL、可选大小信息 |
| `redis_get` | GET String / Binary |
| `redis_set` | SET String，支持 EX/PX、NX/XX 等互斥参数 |
| `redis_delete` | DEL/UNLINK，支持单/多 key，写入授权 |
| `redis_hash` | HGET/HSET/HDEL/HSCAN 等受控 action 枚举 |
| `redis_list` | LRANGE/LLEN/LPUSH/RPUSH/LPOP 等受控 action |
| `redis_set_members` | SSCAN/SADD/SREM/SCARD 等受控 action |
| `redis_sorted_set` | ZRANGE/ZADD/ZREM/ZCARD 等受控 action |
| `redis_info` | PING、INFO 的经过过滤的安全子集 |

- SCAN 用 cursor 返回 `next_cursor`、有限批次；遍历过程可能重复 key，文档说明迭代语义。
- Redis 值属于二进制安全数据：返回 UTF-8 文本或 Base64 + `encoding`，禁止有损替换。
- 数据修改 action 需要写权限；细粒度的 `action` 参数使用枚举和条件校验。
- 默认不暴露任意 Redis 命令；`FLUSHALL`、`FLUSHDB`、`CONFIG`、`DEBUG`、`EVAL`、`MODULE`、`ACL` 等高风险指令必须由未来独立且受限的管理模式处理。
- Redis Streams、PubSub、Cluster 管理、Pipeline/MULTI 放在扩展阶段。

### 2.5 后续增量工具候选

`sql_fetch_page`、`sql_cancel_query`、`mongo_create_index`、`mongo_drop_index`、`mongo_bulk_write`、`mongo_begin_transaction`、`mongo_commit_transaction`、`mongo_abort_transaction`、`redis_stream_read`、`redis_stream_write`、`redis_pipeline`、`export_data`、`import_data`。这些候选先保持在 Roadmap，按真实需求追加；Tool 清单变更遵循语义化版本与兼容规则。

---

## 3. 工具输出与错误契约

### 3.1 通用响应标准

正常结果用 MCP `structuredContent` 提供 JSON 对象，同时提供适度的文本内容以兼容较旧客户端。基础元字段：`connection_id?`、`duration_ms`、`truncated?`、`warnings?`。每个 Tool 发布独立 outputSchema，禁止把一切响应塞进无约束 `any`。

错误采用稳定的业务错误码：

| error.code | 场景 |
|---|---|
| `INVALID_ARGUMENT` | 参数、URL、Schema 格式无效 |
| `UNSUPPORTED_ENGINE` | URL 指向未实现数据库 |
| `UNSUPPORTED_OPERATION` | 当前驱动/服务器能力不足 |
| `CONNECTION_FAILED` | 建连失败、认证失败或 TLS 校验失败（脱敏） |
| `CONNECTION_NOT_FOUND` | ID 不存在或已过期 |
| `PERMISSION_DENIED` | 只读策略/主机策略/数据库权限拒绝 |
| `QUERY_TIMEOUT` | 超时与取消 |
| `QUERY_FAILED` | 数据库语句错误；保留安全的 SQLSTATE/驱动错误码 |
| `RESOURCE_LIMIT_EXCEEDED` | 行数、结果大小、连接数、并发限额 |
| `TRANSACTION_NOT_FOUND` | 事务无效或已结束 |
| `TRANSACTION_CONFLICT` | 同一事务发生并行操作冲突 |
| `INTERNAL_ERROR` | 内部错误（携带 request_id，隐藏内部细节） |

MCP 层的协议错误与执行期间的 Tool 错误分离，遵循 SDK 行为；数据库错误内容先脱敏再返回。

### 3.2 数据类型

统一的数据表示要保留原始类型信息：

- SQL：NULL、Boolean、Int、UInt、Decimal（字符串保精度）、Float、Timestamp（带时区信息）、UUID、JSON、Bytes（Base64）。
- MongoDB：使用 Extended JSON 序列化 ObjectId、Date、Decimal128、BinData 等 BSON 类型。
- Redis：String/Binary、Hash、List、Set、Sorted Set，明确 key 类型和 bytes 编码。
- 避免隐式丢失数字精度；对未知类型返回类型名、编码形式及警告。

---

## 4. 连接、并发与生命周期

1. 每次 `open_connection` 解析 URL、识别驱动、应用**默认开放的网络策略**与 `read_only`，创建驱动连接，执行真实 ping，再分配 ID；仅在启用 restricted 网络或 TLS 强制策略时执行额外拒绝判断。
2. 默认最大同时连接条目 16、每个 SQL Pool 最大 3 个物理连接；通过 CLI 配置上限。
3. 默认 idle TTL 30 分钟；后台定期清理，并在 `close_connection` 与进程退出时释放资源。
4. 使用并发 Semaphore，默认全局并发查询 8，每个连接 4（事务专用连接一次 1 条），防止 Agent 无限并行查询。
5. 查询与连接调用均设置超时；发生取消或连接中断时清理流与游标；对于服务器取消语义不可保证的操作，要显式说明执行状态可能未知。
6. 事务独立 `transaction_id`，绑定 `connection_id` 与物理连接；默认事务空闲 TTL 60 秒、总时限 5 分钟。结束、关闭、过期均回滚并释放资源。
7. 连接 ID/事务 ID/游标 ID 均限定当前服务进程；重启后不会恢复。
8. SQLite 写入并发与文件锁、MySQL DDL 隐式提交、MongoDB 集群模式、Redis Cluster 命令路由等差异由适配层负责，并在能力发现中声明。

---

## 5. 默认易用的安全与网络策略（V2 重要变更）

本节明确覆盖旧版 PLAN 中“默认回环白名单”和“远程必须 TLS”的限制。**默认允许连接任何操作系统可达的地址，默认遵循 database_url/驱动自身的 TLS 选项**；用户可按需开启隔离与加密要求。

### 5.1 默认行为（零配置启动）

| 维度 | `dbconnector serve` 默认行为 | 可选加强措施 |
|---|---|---|
| MCP 传输 | 仅 stdio | 不实现 HTTP |
| 主机网络 | **open：任意可达 IP、域名、localhost、内网、VPN、公网** | `--network-policy restricted` + `--allow-host` |
| DNS / IPv4 / IPv6 | 按驱动和系统解析；支持正常访问 | 受限模式中校验解析结果/地址变更 |
| DB 连接 TLS | **按 database_url 和驱动配置**；支持不加密的开发连接 | `--require-db-tls`，并验证证书和主机名 |
| SQLite 文件 | 可使用当前进程有访问权限的路径 | `--sqlite-root` 限定路径根目录 |
| 写入权限 | **只读**，Agent 不能自行升级 | 启动 `--allow-writes` 且连接设 `read_only:false` |
| SQL 返回 | 默认 100 行、最多 1000 行、最多 256 KiB | `--max-rows`、`--max-result-bytes` |
| 超时 | 默认查询 10 秒 | `--query-timeout-ms` |
| 凭据 | URL 传递，进程内存持有连接，不落盘 | 未来引入安全引用或本地秘密注入 |

关键原则：网络连接范围与数据库写权限**分别管理**。允许连接公网 IP 不等于允许执行数据库写入；`--allow-writes` 也不改变网络策略。

### 5.2 Database URL 与凭据边界

- `open_connection(database_url)` 允许用户名、密码在 URL 中；支持特殊字符 URL 编码。示例中的 `***` 或 `<password>` 为占位符。
- **事实边界**：密码可能经过模型上下文、MCP Client 会话历史/工具调用日志；stdio 保证本地管道通信，但不对工具参数进行加密或从上游记录中删除。此风险要写入 README 与 `open_connection` Tool 描述。
- 服务端不得回显 URL 原文、密码、敏感查询结果到日志。为连接保存脱敏元信息和驱动对象，避免持久化原始 DSN；不要对 Secret 类型自动派生可打印 Debug/Serialize。
- 数据库通信的加密独立于 MCP stdio：对敏感/公网生产库**建议**启用 PostgreSQL `sslmode=verify-full`、MongoDB TLS 或 `rediss://`；这些属于部署建议，V1 不默认拒绝明文开发连接。

### 5.3 默认只读与写入模式

- `open_connection` 的 `read_only` 默认 `true`。
- `--allow-writes` 是进程启动时用户授予的写权限上限；连接的 `read_only:false` 再声明此连接可执行写入，最终受数据库用户实际权限约束。
- `sql_execute`、`mongo_insert/update/delete`、Redis 写操作均执行服务端写权限判定；同一套策略适用于 `redis_hash/list/set_members/sorted_set` 的 write actions。
- SQL 的解析/单语句限制用于辅助限制只读调用；只读访问对有副作用函数、权限提升和 DDL 差异要加真实数据库测试。优先建议使用专用只读数据库账户作为最终防线。
- 对 DROP、TRUNCATE、无 WHERE 的 UPDATE/DELETE、MongoDB 全集合写入等破坏性操作，要求显式 `allow_destructive` 工具参数和服务端 `--allow-destructive` 两个条件，并记录审计。**不得依赖 Agent 确认弹窗充当服务端权限校验**。
- Redis 不暴露通用任意命令执行器；禁用 FLUSHALL、CONFIG、DEBUG、MODULE、EVAL 等高风险管理指令的默认工具入口。

### 5.4 可选网络限制

默认启动：

```bash
dbconnector serve
```

允许 SQL、MongoDB、Redis 访问本机、192.168.x.x、10.x.x.x、WireGuard、域名及公网，只要当前主机与账号可连接即可。

需要强制只连接某些地址时，由本机用户显式启用：

```bash
dbconnector serve \
  --network-policy restricted \
  --allow-host 127.0.0.1 \
  --allow-host 192.168.1.100 \
  --allow-host db.example.com
```

实现约束：restricted 模式校验目标 IP、域名解析、重定向/重新解析和 IPv6；SQLite 不属于 TCP 主机白名单。策略失败返回 `PERMISSION_DENIED`，并提供用户可理解的错误说明。

可选强制数据库加密：

```bash
dbconnector serve --require-db-tls
```

仅对 TCP 数据库连接执行要求；SQLite 本地文件不适用。要求 TLS 时必须验证证书链/服务器名，不允许静默降级。

### 5.5 保留的低干扰防护

超时、最大连接数、并发数、结果行数和响应字节数默认开启；日志默认写 stderr、凭据脱敏。错误码区分认证失败、网络不可达、TLS、超时和权限拒绝。默认网络开放时仍有这些边界，避免 Agent 无意间耗尽资源。

---

## 6. 具体实施阶段与执行顺序

每个里程碑必须：① 完成全部代码；② 添加自动测试；③ 运行 fmt/clippy/test；④ 更新 README / API 文档；⑤ 提交阶段验收报告；⑥ 进入下一阶段。

### M0 — 基础工程与通用连接（优先级 P0）

**任务**

1. 初始化 Cargo Workspace、工具链与依赖、Rust 入口、CI 基线。
2. 使用 `rmcp` 创建 stdio Server；确保 Tool Registry、Schema 输出与生命周期符合当前稳定 MCP 协议。
3. 实现 URL Parser、Engine Detector、ConnectionManager、统一 Error、Policy、Result 基础类型。
4. 集成 SQLx、MongoDB、Redis 三类客户端并支持 `open_connection` / `close_connection` / `list_connections` / `get_connection_info` / `get_capabilities`。
5. SQLite 内存数据库、临时文件和本地 PostgreSQL/Redis/MongoDB 测试连接；至少覆盖同时打开多个不同实例。
6. 全局超时、日志脱敏、stdout 测试、正常关闭资源；默认网络开放，可直接连接预置的内网/外网测试主机（受测试环境网络控制）。

**验收**：MCP Inspector 可发现 5 个通用工具；至少 3 种类别同时打开、返回不同 ID；断开单一连接后其余连接可用；**不传 `--allow-host` 也可连接允许访问的远程测试数据库**；错误 DSN 和认证失败有稳定错误码；stdout 无日志；凭据不在响应、stderr 中出现。

### M1 — SQL 只读能力（优先级 P0）

**任务**

1. 完成 `sql_list_databases`、`sql_list_schemas`、`sql_list_tables`、`sql_describe_table`、`sql_query`。
2. 实现 PostgreSQL/MySQL/SQLite 差异化 Metadata Adapter；支持表、视图、列、主键、外键、索引。
3. SQL 参数化绑定、强类型结果标准化、行数/字节/超时限制和只读判定。
4. 增加 PostgreSQL、MySQL、SQLite 容器与文件数据库的集成测试，测试空表、NULL、大整数、Decimal、JSON、时间、Binary、特殊标识符。

**验收**：同一 MCP API 可对 3 种 SQL 数据库完成结构探索与参数化 SELECT；能够拒绝 INSERT/UPDATE/DELETE/DDL、注释混淆与多语句；查询限制真实生效；无密码泄漏。

### M2 — SQL 写入、执行计划和事务（优先级 P1）

**任务**

1. 完成 `sql_execute`、`sql_explain`、`sql_begin_transaction`、`sql_commit_transaction`、`sql_rollback_transaction`、`sql_get_table_stats`。
2. 在服务端 `--allow-writes` + 连接 `read_only:false` + DB 账号权限三重约束下实现受控写入。
3. 事务固定物理连接并串行操作；事务结束、超时、Client 异常退出自动回滚。
4. 对 DDL 与大范围 DELETE/UPDATE 采用双重破坏性策略（`--allow-destructive` + `allow_destructive:true`）；默认禁止任意扩展执行、服务器文件操作与高危管理语句。
5. `sql_explain` 适配不同方言，不自动使用执行型 ANALYZE。

**验收**：写操作默认拒绝；获得服务端授权后可写入；事务提交后可见、回滚后不可见；并行操作/超时/断连不会留下开放事务；不同 SQL 数据库均有执行计划与表统计的合理降级结果。

### M3 — MongoDB（优先级 P1）

**任务**

1. 完成 9 个 `mongo_*` Tools；复用公共超时、权限、结果处理。
2. 以 BSON 原生文档运算，输出 Extended JSON，覆盖 ObjectId/Date/Decimal128/Bytes。
3. 支持过滤、投影、排序、限制、采样结构推断和索引读取。
4. 对 aggregate 的写入阶段与嵌套子管道施行安全校验；执行限额与数据库账号权限复核。
5. 覆盖单节点与副本集相关能力差异，明确跨集合事务作为后续计划。

**验收**：MongoDB 可查询、聚合、插入、更新、删除（写入授权下）；默认只读模式阻止各类写入聚合；BSON 往返序列化无关键类型丢失；分页/limit 与超时受控。

### M4 — Redis/Valkey（优先级 P1）

**任务**

1. 完成 10 个 `redis_*` Tools，Redis `ConnectionManager` + 可选 TLS + PING。
2. 支持 String/Hash/List/Set/Sorted Set；SCAN 的 cursor 分页；TTL 和连接/服务信息的安全子集。
3. 所有二进制值采用无损的编码标记；Key 扫描实现批次限制。
4. Redis 写入 action 绑定 `--allow-writes` 和只读 ACL；禁用任意命令入口，禁止危险服务器管理指令。
5. 对 Redis 与 Valkey 分别运行核心兼容测试。

**验收**：只读模式可枚举/读取各数据结构；可写模式完成各结构基本 CRUD；SCAN 游标正常；**默认可连接远程 redis://，按需 rediss:// 或 `--require-db-tls`**；大键及二进制值处理受限且无损。

### M5 — 稳定性、发布和扩展接口（优先级 P1）

**任务**

1. 完成完整 API Reference、部署文档、风险说明、MCP Client 配置示例、Troubleshooting、CHANGELOG。
2. 集成测试覆盖断连重试、数据库中断、连接空闲淘汰、并行请求、取消/超时、错误重连、SQLite 锁。
3. GitHub Actions 运行 Linux/macOS/Windows build/test；依赖审计、Clippy、格式检查；容器集成测试运行于 Linux。
4. Release 自动生成可执行文件与校验和，发布明确版本；增加基本性能回归门槛（并发查询、内存、连接数）。
5. 通过 MCP Inspector + 至少两个实际客户端完成验收；输出可复现测试记录。

**验收**：从全新机器安装并按 README 启动；完成 SQL/Mongo/Redis 三类完整路径；CI 全绿；无关键已知高危依赖问题；安全用例全部通过；Release 可下载安装。

### M6 — 按需演进（后续）

实施 MongoDB 事务/批量操作、Redis Streams/Pipeline、SQL 游标/流式结果、导入导出、凭据引用、数据库新适配器。每新增数据库必须提交能力矩阵、驱动边界、权限规则和集成测试；避免为了统一接口削弱各数据库原生功能。

---

## 7. 自动化测试矩阵

### 7.1 基础测试

| 领域 | 最低测试 |
|---|---|
| MCP 协议 | 2026-07-28 的请求元信息/发现流程及旧版 initialize 兼容，tools/list、tools/call、结构化输出、错误消息、stdio 断开 |
| 连接生命周期 | 多库共存、ID 唯一、连接失败、连接关闭、空闲回收、服务退出 |
| SQL | 三种引擎结构发现、参数绑定、只读/写入、事务、解释计划、类型精度 |
| MongoDB | find、aggregate、CRUD、索引、Extended JSON、写入阶段阻断 |
| Redis/Valkey | SCAN、TTL、String、Hash、List、Set、ZSet、ACL、二进制数据 |
| 网络与安全 | 默认 open 远程连接、restricted 白名单拒绝、可选 TLS 强制、URL 脱敏、日志泄漏、SQL 绕过、多语句、资源限制 |
| 稳定性 | 查询超时、取消、并发 20+、DB 重启、断线、清理事务、无锁死 |

### 7.2 测试基础设施

`docker-compose.test.yml` 提供 PostgreSQL、MySQL、MongoDB、Redis、Valkey；SQLite 使用临时路径。所有账号采用专用测试账号；分别创建只读账户和可写账户。使用固定种子数据，通过环境变量提供测试 URL；CI 以 ephemeral 测试数据启动容器，清理过程可重复执行。

### 7.3 质量门禁

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
```

可增加 `cargo audit` / `cargo deny` 与 MSRV CI；安全扫描对真实安全公告进行人工确认。

---

## 8. 最终使用方式、CLI 与 MCP Client 配置

### 8.1 可执行文件及 CLI（目标接口，尚待开发）

```text
dbconnector serve [--allow-writes] [--allow-destructive]
                   [--network-policy open|restricted]
                   [--allow-host HOST]...
                   [--require-db-tls] [--sqlite-root PATH]
                   [--max-connections N] [--query-timeout-ms N]
                   [--max-rows N] [--max-result-bytes N]
dbconnector --version
dbconnector --help
```

完整默认启动仅需 `dbconnector serve`。应在 `--help` 中标明 `--network-policy open` 是默认值；`--allow-host` 仅与 restricted 模式配合使用。平台需单独发布 Linux/macOS/Windows 二进制并测试执行权限。

### 8.2 Codex（已验证 CLI 语法）

```bash
codex mcp add dbconnector -- dbconnector serve
codex mcp list
```

若可执行文件不在 PATH，用绝对路径替代 `dbconnector`。CLI 注册后 Codex 会自行启动 stdio 子进程与发现 Tools；仍受客户端工具可用性/审批策略影响。

### 8.3 OpenCode

当前 OpenCode 文档有不同版本的配置结构；**先执行客户端版本检查，再遵从该版本文档**。常见本地 MCP 配置示例：

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "dbconnector": {
      "type": "local",
      "command": ["dbconnector", "serve"],
      "enabled": true
    }
  }
}
```

如使用采用 `mcp.servers` 层级的 OpenCode v2 配置，则在 `mcp.servers.dbconnector` 下配置相同的 `type` 与 `command`；不得盲目把旧配置覆盖到新版。文档补充 `opencode mcp` 或等效检查命令，按已安装的版本确认。

### 8.4 Claude Code / 其他客户端

发布文档必须提供对应客户端**已经验证**的本地 stdio 注册示例，不猜测具体配置路径与版本差异。通用 JSON（**结构仅供参考**）：

```json
{
  "mcpServers": {
    "dbconnector": {
      "command": "dbconnector",
      "args": ["serve"]
    }
  }
}
```

### 8.5 完整对话演示和正确工具选择

用户输入：**“连接我自己的 PostgreSQL，显示 users 表结构并查询前 10 行，然后连接 Redis 看一下 session:* 的数量”**。

```text
MCP Client 启动 dbconnector serve → tools/list 自动发现
open_connection({database_url:"postgresql://.../app"}) → conn_A
sql_list_tables({connection_id:"conn_A"})
sql_describe_table({connection_id:"conn_A",table:"users"})
sql_query({connection_id:"conn_A",sql:"SELECT ... LIMIT 10"})
open_connection({database_url:"redis://.../0"}) → conn_B
redis_scan({connection_id:"conn_B",cursor:"0",match:"session:*"})
redis_scan(...next_cursor...) → 直到 cursor 为 0；聚合结果
close_connection({connection_id:"conn_A"})
close_connection({connection_id:"conn_B"})
```

MongoDB 示例：`open_connection({database_url:"mongodb://..."})` → `mongo_list_collections` → `mongo_find` 或 `mongo_aggregate`。连接 ID 都是例子，每次应使用 Tool 实际返回的随机 ID。

写入示例：本机用户启动 `dbconnector serve --allow-writes`，Agent 调用 `open_connection({database_url:"...",read_only:false})` 后才可执行允许的写入操作。运行于同一个 stdio 进程的工具均需验证当前 ID 权限。

---

## 9. Coding Agent 执行指令（直接使用）

> 你是 `dbconnector` 项目的主开发 Agent。严格依据本 V2 PLAN.md 按 M0→M5 顺序实现，最终交付可运行的 Rust **stdio-only** MCP Server。**默认网络开放，允许任何可达数据库主机，无需 allow-host 或强制 TLS；restricted 白名单和强制 TLS 均为可选配置**。优先端到端验证和易用性，保持已确定接口与技术选型。每个阶段完成前需运行对应测试并修复问题，随后更新 README、接口文档和阶段验收记录；在当前环境具备所需服务时持续推进到 M5。若受限于 Docker、网络或系统依赖，保留可执行测试代码，明确记录未验证项和复现命令，并继续完成其他可独立实施的任务。不要把未实现的功能写成已支持，不要用 Mock 替代端到端验收，不要提交真实数据库密码或密钥。
>
> 技术要求：Rust + Tokio + 官方 rmcp（stdio only）+ SQLx + 官方 MongoDB Rust Driver + redis-rs；动态 URL 建连并返回随机 connection_id；多连接共存；默认只读、可选 `--allow-writes`；网络默认 open、TLS 依据 URL；结构化 JSON 输出；超时、结果限制、凭据脱敏与测试覆盖。所有 stdout 内容均须为 MCP JSON-RPC 帧。
>
> 输出阶段报告必须包含：变更文件、已实现 Tools、测试命令及结果、设计偏差与原因、已知限制、下一阶段任务。完成 M5 后给出最终架构图、35 个 Tools/Schema 清单、测试矩阵、可直接复制的 Codex/OpenCode stdio 配置、版本信息与正式验收记录；**逐一测试 Tool descriptions 能否让 Agent 自动选用**。

## 10. 最终验收清单

- [ ] `dbconnector serve` 可由 MCP Client 通过 stdio 正常启动/关闭。
- [ ] 35 个规划内 Tools 全部实现并具有 inputSchema/outputSchema；`list_tools` 与文档一致。
- [ ] 多个 `connection_id` 同时操作不同 SQL、MongoDB、Redis 连接。
- [ ] PostgreSQL、MySQL、SQLite 元数据与只读查询、受控写入、事务全部通过集成测试。
- [ ] MongoDB 查询、聚合、CRUD、索引、BSON 类型保真通过集成测试。
- [ ] Redis/Valkey 常用数据类型、游标遍历、权限控制、二进制支持通过集成测试。
- [ ] **默认网络开放**：无需额外启动参数即可连接获授权的内网、VPN 与公网测试库；可选 restricted 网络白名单、TLS 强制、默认只读、超时、结果限制、凭据脱敏全部通过测试。
- [ ] stdout 无日志；兼容当前 MCP 版本与常见旧客户端；至少两个真实客户端 `tools/list` 和 `tools/call` 测试通过，Agent 对 SQL/Mongo/Redis 的工具选择有记录。
- [ ] Linux/macOS/Windows 发布构建、CI、README、SECURITY.md、CHANGELOG 完成。
- [ ] 输出最终验收报告，所有未满足项列出证据与后续处理建议。

## 11. 官方参考（实施时再次核对稳定版本）

- 官方 Rust MCP SDK（rmcp）: <https://github.com/modelcontextprotocol/rust-sdk>
- MCP 2026-07-28 规范更新：<https://blog.modelcontextprotocol.io/posts/2026-07-28/>
- MCP Tools 与工具发现：<https://modelcontextprotocol.io/specification/2026-07-28/server/tools>
- SQLx AnyPool / install_default_drivers：<https://docs.rs/sqlx/latest/sqlx/any/fn.install_default_drivers.html>
- MongoDB 官方 Rust Driver：<https://www.mongodb.com/docs/drivers/rust/current/>
- redis-rs：<https://github.com/redis-rs/redis-rs>
- Codex MCP CLI 源码：<https://github.com/openai/codex/blob/main/codex-rs/cli/src/mcp_cmd.rs>
- OpenCode 本地 MCP：<https://docs.opencode.ai/docs/mcp-servers/>

> 文档中的 CLI 选项和 35 个 Tools 属于 dbconnector 的**目标开发规范**，不表示已经实现。实现过程中如遇到特定数据库协议/权限限制，应保留名称与契约，并通过 capabilities、错误码和验收报告如实表达差异。
