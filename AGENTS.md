# AGENTS.md — dbconnector 仓库级开发约束

> 项目：`dbconnector`  
> 状态：开发指令（与 `PLAN.md` V2 配套）  
> 目标：使用 Rust 实现一个支持 SQL、MongoDB、Redis/Valkey 的 **stdio-only** MCP Server。

## 0. 如何使用本文件

- **开始编码前必读**：先阅读本文件和仓库根目录 `PLAN.md`，核对现有代码与文档的差异，再制定当前阶段的最小执行计划。
- **规范优先级**：用户当次明确要求 > `PLAN.md` 中的产品功能/API 基线 > 本文件的工程执行细则 > 其他设计建议。存在冲突时记录冲突及处理依据。
- **持续交付**：按 `PLAN.md` 的 M0 → M1 → M2 → M3 → M4 → M5 开发；完成一个阶段后进行验证，再进入下一阶段。可以修复阶段外的阻塞性基础设施问题，但不要无故改变范围。
- **以代码为准验收**：每次修改均检查实际仓库现状；区分“已实现”“已编译”“已测试”“已通过真实数据库测试”“已通过真实 MCP Client 验证”。
- **工具使用**：允许使用环境中真正可用的 Agent skills、工具与子代理处理适合的独立子任务；若某环境没有这些能力，则直接完成工作，不虚构调用记录，也不为本项目额外开发 Skill。
- **工作范围**：只在用户提供的仓库中修改项目。修改部署环境、推送远端、发布版本或操作真实业务数据库，须遵守用户授权和仓库既有流程。

## 1. 固定产品决策：必须遵守

| 领域 | 决策 |
| --- | --- |
| 项目名称 | `dbconnector` |
| 语言与运行时 | Rust + Tokio |
| MCP SDK | 官方 `rmcp` |
| MCP Transport | **仅 stdio**；不实现 HTTP/SSE/远程 MCP Server |
| SQL 驱动 | `sqlx`，初期 PostgreSQL、MySQL/MariaDB、SQLite |
| MongoDB 驱动 | 官方 `mongodb` Rust Driver |
| Redis/Valkey 驱动 | `redis-rs`（`redis` crate） |
| 连接入口 | `open_connection(database_url, read_only?)` |
| 连接引用 | 返回随机、不透明 `connection_id`；同进程可同时保持多个不同数据库连接 |
| 凭据 | V1 允许在 URL 中提供凭据；仅保留于进程内运行时，**不得**打印或持久化原始 URL/密码 |
| 网络 | **默认 open**：允许所有操作系统可达的本地、内网、VPN、域名和公网主机 |
| 可选限制 | `--network-policy restricted`、`--allow-host`、`--require-db-tls`、`--sqlite-root` |
| TLS | 默认遵循 Database URL 与驱动配置；受限模式可要求严格 TLS 验证 |
| 写操作 | 默认只读；需 `--allow-writes` + `open_connection(read_only:false)` + DB 账户授权 |
| 破坏性操作 | 双重授权：`--allow-destructive` + 工具 `allow_destructive:true`；须额外验证策略 |
| 发布 | 一个 `dbconnector` 可执行文件，Linux/macOS/Windows |
| 工具 | 完整 V1 目标为 **35 个 MCP Tools**；仅向 `tools/list` 注册实际已经实现的 Tool |

**重点**：默认开放的数据库网络访问策略与数据库写入权限互相独立。不得把旧版“默认 localhost 白名单”“远程数据库必须 TLS”作为新的默认行为。也不得为省事移除超时、资源限制、密码脱敏和写入授权。

## 2. 工程架构与模块边界

目标结构遵循 `PLAN.md`：

```text
dbconnector/
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml
├── PLAN.md
├── AGENTS.md
├── PROMPT.md
├── README.md
├── SECURITY.md
├── CHANGELOG.md
├── src/main.rs
├── crates/
│   ├── dbconnector-core/     # 连接/策略/事务/限制/结果/错误
│   ├── dbconnector-mcp/      # rmcp stdio Server、Tool Schema 与路由
│   ├── dbconnector-sql/      # SQLx 与 PostgreSQL/MySQL/SQLite 适配
│   ├── dbconnector-mongo/    # MongoDB BSON、CRUD、聚合、索引
│   └── dbconnector-redis/    # Redis/Valkey Key 和数据类型操作
├── tests/contract/
├── tests/integration/
└── docker-compose.test.yml
```

- MCP Layer 仅负责协议、参数模型、Tool 注册、错误/响应映射；数据库具体行为放到 Driver/Adapter。
- Core 负责 `ConnectionManager`、能力发现、权限检查、并发/资源控制、结果类型与生命周期。
- SQL 使用 SQLx；**MongoDB 和 Redis 使用各自驱动**。避免把所有数据库强行抽象成 SQL 查询。
- 不引入 Web 框架、持久配置数据库、HTTP 服务器、GUI 或 Skill 作为 V1 依赖。
- 依赖尽量小，运行时使用稳定版本；检查 crate Features、Rust MSRV 与 `rmcp` 实际支持的协议版本，锁定 `Cargo.lock`。

## 3. MCP stdio 正确性

- `stdin`/`stdout` 只收发 MCP JSON-RPC；**stdout 不得出现日志、进度、Banner、调试输出或普通 `println!`**。
- `tracing` 日志输出到 `stderr`，从源头删除数据库密码、完整 DSN、私密字段、原始 SQL 参数内容。
- 使用官方 `rmcp` 支持的初始化/协商流程；遵循当前 SDK 和客户端兼容能力，不自行捏造 MCP 协议消息。
- Tool 必须有明确 `name`、`description`、`inputSchema`、合理的 `outputSchema` 和准确的只读/破坏性 annotations。
- 正常结果使用 MCP `structuredContent` 的对象表示，并为需要的客户端提供兼容内容；Tool 执行错误应使用可解析的业务错误结构。
- Agent 通过客户端的 `tools/list`/`tools/call` 发现并调用工具。无需额外的 Skill 才能使用此 MCP。工具的自动选择受客户端设置、工具授权和模型行为影响，必须实测。
- Tool Description 以动词开头，说明使用场景、适配数据库、关键参数、返回结果及写权限；`open_connection` 须提示包含密码的 URL 可能经过模型上下文/客户端日志。

## 4. 固定 35 个 Tool 名称

### 通用连接管理（5，M0）

`open_connection`、`close_connection`、`list_connections`、`get_connection_info`、`get_capabilities`

### SQL（11，M1–M2）

`sql_list_databases`、`sql_list_schemas`、`sql_list_tables`、`sql_describe_table`、`sql_query`、`sql_execute`、`sql_explain`、`sql_begin_transaction`、`sql_commit_transaction`、`sql_rollback_transaction`、`sql_get_table_stats`

### MongoDB（9，M3）

`mongo_list_databases`、`mongo_list_collections`、`mongo_describe_collection`、`mongo_find`、`mongo_aggregate`、`mongo_insert`、`mongo_update`、`mongo_delete`、`mongo_list_indexes`

### Redis / Valkey（10，M4）

`redis_scan`、`redis_key_info`、`redis_get`、`redis_set`、`redis_delete`、`redis_hash`、`redis_list`、`redis_set_members`、`redis_sorted_set`、`redis_info`

**接口契约以 `PLAN.md` 第 2–4 节为准。** 禁止擅自重命名或通过通用 `execute_any_command` / `run_command` 代替上述受限 Tools。新增候选 Tool 应先列入 Roadmap，再明确 Schema、能力边界与验收范围。

## 5. 连接与驱动实现规则

- 支持 URL Scheme：`postgres://`、`postgresql://`、`mysql://`、`sqlite://`、`mongodb://`、`mongodb+srv://`、`redis://`、`rediss://`；使用驱动实际支持的合法 URL 解析，不自造宽松语法。
- 每次 `open_connection` 真实尝试连接并 ping，成功后返回唯一 `connection_id`；不同连接/数据库能够并存。连接失败不登记成功状态。
- `connection_id`、`transaction_id` 只在当前 MCP Server 进程有效；不把 ID 当作跨用户鉴权令牌。
- SQLx `AnyPool` 在启动时注册默认驱动；针对参数绑定、类型解码、Schema 差异提供数据库专用实现和集成测试。
- MongoDB BSON 必须支持 Extended JSON，并保持 ObjectId、Date、Decimal128、Binary 等类型信息。
- Redis Key/Value 必须二进制无损；文本 UTF-8 与 Base64 输出明确标识。遍历 Key 使用有游标、有限批次的 `SCAN`。
- ConnectionManager 应控制条目数、连接池上限、空闲 TTL、并发数及关闭清理。严禁持有全局 map 锁跨数据库 I/O 的 `.await`。
- SQL 事务固定一个物理连接，使用独立 `transaction_id`，正确处理串行执行、提交、回滚、超时和断连清理。
- 不将 MongoDB/Redis 自身特性（事务、Cluster、Streams）未经支持检查就宣称完整兼容；由 `get_capabilities` 描述已实现功能与限制。

## 6. 数据访问与权限约束

- 默认网络策略：`open`，允许连接任何操作系统可达的数据库；**可选** restricted 白名单与强制 TLS 要有独立测试。
- `open_connection` 默认 `read_only:true`；服务端授权和连接权限都要检查。Agent 无法通过任意 Tool 自行将只读连接升级为写连接。
- SQL 的只读入口 `sql_query`：限制单语句、非只读语法、带副作用的查询；AST 判断属于辅助保护，结合实际数据库账户权限测试。运行 `sql_explain` 不应默认实际执行用户语句。
- MongoDB 聚合读取路径拒绝 `$out`、`$merge`，并覆盖嵌套 pipeline；仅在授权写模式允许受控写动作。
- Redis 工具中的 write action 必须重新检查写入权限；默认不开放 `FLUSHALL`、`FLUSHDB`、`CONFIG`、`DEBUG`、`EVAL`、`MODULE`、`ACL` 等泛化管理能力。
- 对 DROP/TRUNCATE、无条件大范围更新/删除等操作采用双重破坏性授权。权限判断必须由 MCP Server 强制实施。
- 默认 SQL 查询 100 行、最大 1000 行、结果 256 KiB、超时 10 秒；默认最多 16 个连接条目、SQL Pool 最多 3 个物理连接、全局查询并发 8、每连接 4；事务闲置 60 秒、最多 5 分钟，连接 idle TTL 30 分钟。上述数值为 `PLAN.md` 默认基线，可暴露 CLI 配置覆盖。
- 控制数据库 I/O 返回量、网络超时、会话状态；遇到驱动无法安全中断的操作时明确说明执行状态未知。
- **凭据风险不可隐藏**：`database_url` 中的密码可能被 Agent/MCP Client 保存到历史；服务器本身不能保证上游记录不包含该参数。在 README 和 Tool 描述中明确告知。

## 7. 代码质量要求

- 优先符合现有仓库约定：`rustfmt`、Clippy、清晰命名、简单可测试的模块边界。
- 所有公开 Tool 的输入/输出在 Rust 类型层建模，利用 `serde` / `schemars` 实现校验；严禁散布缺乏验证的自由 JSON 映射。
- 不使用 `unwrap()` / `expect()` 处理数据库地址、用户输入或 I/O 失败；仅在真实不变式与测试中合理使用，并注明原因。
- 返回语义稳定的错误码，例如 `INVALID_ARGUMENT`、`CONNECTION_FAILED`、`CONNECTION_NOT_FOUND`、`PERMISSION_DENIED`、`QUERY_TIMEOUT`、`QUERY_FAILED`、`RESOURCE_LIMIT_EXCEEDED`、`TRANSACTION_CONFLICT`、`UNSUPPORTED_OPERATION`、`INTERNAL_ERROR`。
- 类型结果应覆盖 NULL、精确数值、时间/时区、JSON、UUID、二进制及 MongoDB BSON 扩展类型。不能隐式损失精度。
- 默认 Feature 包含 `sql,mongo,redis`。禁用 Feature 后，相应工具与驱动不应出现在该构建的注册表中。
- 禁止把凭据、真实业务数据、远端地址私有信息或测试密钥加入代码、快照、日志和文档示例。

## 8. 测试与验收

每个阶段完成前至少运行：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
```

并按里程碑添加以下验证：

| 阶段 | 必须验证 |
| --- | --- |
| M0 | stdio 工具发现、5 个连接 Tool、多数据库同进程、脱敏、默认开放网络 |
| M1 | PostgreSQL/MySQL/SQLite 的 Schema、SELECT、参数绑定、权限/结果限制 |
| M2 | SQL 写入授权、DDL 策略、EXPLAIN、事务提交/回滚/取消 |
| M3 | MongoDB 9 个 Tool，CRUD/聚合/索引、BSON 类型与读写限制 |
| M4 | Redis/Valkey 10 个 Tool，SCAN、TTL、各数据结构、二进制与写入权限 |
| M5 | 测试矩阵、CI、跨平台 Release、MCP Inspector、至少两个真实 MCP Client |

- 使用本地、临时、可销毁的测试数据库（推荐 `docker-compose.test.yml`），分别准备只读与读写用户。**禁止对真实生产数据库执行破坏性测试**。
- 单元测试可使用 Mock；关键验收必须有真实 PostgreSQL/MySQL/SQLite/MongoDB/Redis/Valkey 的集成测试证据。
- 不在缺少 Docker/外部网络时伪称集成测试通过；记录环境阻塞、跳过项、复现命令，并推进其他可实现任务。
- MCP 工具自动选择测试至少覆盖 SQL 表结构、MongoDB 聚合和 Redis TTL，在两种真实 Client 上记录发现结果与调用路径。
- 新功能必须配套测试和文档，现有 Tool 语义不得悄然变化。

## 9. 里程碑报告与文档同步

每完成一个里程碑，更新 `docs/implementation-status.md`（若不存在则创建），至少包含：

1. 当前阶段、提交/版本标识、变更文件；
2. 已实现 Tools、接口变化与 capabilities；
3. 执行过的测试命令、通过/失败/跳过及真实环境信息；
4. 已知风险、兼容性限制、设计偏差与处理理由；
5. 下一阶段任务、剩余阻塞项与复现方式。

同时保持 `README.md`（安装、CLI、客户端 stdio 注册）、`SECURITY.md`（凭据与权限风险）、`CHANGELOG.md`（可见变更）、`docs/tool-api.md`（35 个 Tool Schema）与代码一致。

## 10. 完成定义（DoD）

- `dbconnector serve` 在 stdio 客户端真实启动，`tools/list` 准确列出完整构建的 35 个已实现工具。
- 三类数据库真实连接和操作路径通过测试，多个 `connection_id` 并存，连接/事务/资源可靠释放。
- 默认网络开放 + 只读；可选受限网络、强制 TLS、受控写入和破坏性操作边界按文档生效。
- 输入/输出 JSON Schema、工具描述、结构化结果、错误语义、凭据脱敏、查询限制与退出行为经过验证。
- CI 和至少两个真实 MCP 客户端验收完成，提供可复制的安装、注册和使用说明。
- 最终报告清楚标明通过、未通过、未执行项。**只有测试证据支持时，才宣告 M5 完成。**
