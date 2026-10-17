# PROMPT.md — dbconnector Coding Agent 执行任务

> 将本文中的 **「执行指令」**整体发送给具有仓库文件、终端和（可选）Docker 访问能力的 Coding Agent。  
> 放置路径：`dbconnector/PROMPT.md`，与 `PLAN.md`、`AGENTS.md` 位于同一仓库根目录。  
> 类型：**一次性开发任务**，持续开发直至 M0–M5 已完成或受到真实环境条件阻塞。

---

## 执行指令

你是 `dbconnector` 项目的主开发 Agent，负责从当前仓库状态开始，**独立完成实施、测试、修复、文档和验收**。目标是交付可编译、可安装、可被 MCP Client 发现和使用的 Rust **stdio-only** 多数据库 MCP Server。

### 一、先读取约束，审计现有仓库

1. 读取仓库根目录的 `PLAN.md`（V2 基线）与 `AGENTS.md`（长期开发约束），随后检查 `Cargo.toml`、现有 Rust 代码、测试、CI、README 与 Git 工作区状态。
2. 识别已经实现的模块和功能，保留已有正确实现及用户未提交的修改。优先修复缺陷，不无故推倒重写。
3. 对照 `PLAN.md` 35 个 Tool 清单，列出“已实现 / 部分实现 / 未实现 / 未验证”的差距。对不确定事项先检查代码和真实 SDK API。
4. 简述执行计划，随后**直接开始实施**。按照 M0 → M1 → M2 → M3 → M4 → M5 连续推进，不要每个阶段结束都要求用户再次确认。
5. 若已有可用 Skills、专用开发工具或子代理，按任务需要合理使用；如果没有，直接用现有编辑器、终端和测试工具完成，禁止虚构使用记录。本项目**只构建 MCP**，不额外实现数据库 Skill。

### 二、不可更改的核心架构

必须满足：

```text
Rust + Tokio
    │
    └── rmcp  (stdio only)
          │
          ├── Tool Registry / descriptions / JSON Schemas
          ├── ConnectionManager + PolicyEngine + ResultNormalizer
          │     │
          │     ├── SQLx: PostgreSQL / MySQL / MariaDB / SQLite
          │     ├── mongodb Rust Driver: MongoDB
          │     └── redis-rs: Redis / Valkey
          │
          └── stdout = MCP messages only; stderr = redacted logs
```

- 仅实现 `dbconnector serve` 的 MCP stdio 传输；不添加 HTTP、Web UI、持久凭据库或运行前强制配置。
- `open_connection({"database_url":"...","read_only":true})` 接收数据库 URL，真实连接成功后返回随机 `connection_id`。每个 MCP Server 进程可并存多个 SQL、MongoDB、Redis/Valkey 连接。
- 支持 `postgres://`、`postgresql://`、`mysql://`、`sqlite://`、`mongodb://`、`mongodb+srv://`、`redis://`、`rediss://`，按 URL Scheme 自动分派驱动。
- 工具操作通过 `connection_id` 选择目标连接。`close_connection` 关闭连接并清理其事务资源；进程退出或空闲 TTL 到期时回收资源。
- **默认网络策略 `open`**：允许连接操作系统可达的 localhost、内网、VPN/WireGuard、主机名、公网 IP；无需 `--allow-host`，不默认强制 TLS。`--network-policy restricted`、`--allow-host` 与 `--require-db-tls` 是按需启用的额外限制。
- **默认只读**：写入需显式 `dbconnector serve --allow-writes` + `open_connection(read_only:false)` + 目标数据库账号授权；破坏性操作另需 `--allow-destructive` 与 `allow_destructive:true`。
- 数据库密码可以作为 URL 的一部分输入；绝不在 Tool 结果、程序日志、错误、文档示例或保存文件中输出原始 URL/密码。README 明确告知：MCP Client/模型可能留存敏感工具参数；stdio 本身不消除这个上游风险。
- 非敏感元数据、统一错误码、结构化 JSON、查询超时、行数/响应字节上限和并发资源限制是基础能力。

### 三、对外 Tool API：最终恰好 35 个

以下为 **V1 完整构建的目标工具集合**。准确遵循名称，逐个实现输入/输出 Schema、描述、读写权限、能力检查及集成测试。

**通用（5）：**

```text
open_connection
close_connection
list_connections
get_connection_info
get_capabilities
```

**SQL（11）：**

```text
sql_list_databases
sql_list_schemas
sql_list_tables
sql_describe_table
sql_query
sql_execute
sql_explain
sql_begin_transaction
sql_commit_transaction
sql_rollback_transaction
sql_get_table_stats
```

**MongoDB（9）：**

```text
mongo_list_databases
mongo_list_collections
mongo_describe_collection
mongo_find
mongo_aggregate
mongo_insert
mongo_update
mongo_delete
mongo_list_indexes
```

**Redis / Valkey（10）：**

```text
redis_scan
redis_key_info
redis_get
redis_set
redis_delete
redis_hash
redis_list
redis_set_members
redis_sorted_set
redis_info
```

实现规则：

- 当前阶段 `tools/list` 仅列出**真实已实现**的工具，M5 完整版包含上述全部 35 个；不得用占位返回假装已实现。
- 参数遵循 `PLAN.md` 第 2 节约定，必要时细化定义，但保持工具命名和主要语义稳定。
- `open_connection` 描述必须清楚写明支持数据库类别、何时调用、敏感 URL 风险以及返回 `connection_id`；其他工具描述必须帮助 Agent 判断查询、Schema 探索、MongoDB 文档操作和 Redis Key 操作的适用场景。
- 可在 MCP Server 的 instructions 中说明 `open_connection → get_capabilities → 专属工具 → close_connection`；工具是否自动调用最终由 MCP Client 和 Agent 决定，须实测。
- 对不适用当前数据库的工具返回 `UNSUPPORTED_OPERATION` 等明确错误，不编造成功结果。
- 不提供绕过服务端授权的通用 SQL 以外数据库管理命令入口；Redis 只提供受控 action 枚举，MongoDB 聚合只读时禁止写入阶段。

### 四、实施里程碑：逐阶段完成

#### M0 — Rust 项目骨架、stdio 与统一连接

1. 建立 Cargo Workspace、`dbconnector-core` / `dbconnector-mcp` / `dbconnector-sql` / `dbconnector-mongo` / `dbconnector-redis` crates 及 CLI 入口。
2. 以官方 `rmcp` 实现 stdio Server、工具路由、工具发现、Schema 与 JSON-RPC 生命周期；正确处理 SDK 支持的协议版本及客户端协商。
3. 实现 URL Scheme 路由、DatabaseKind、ConnectionBackend、ConnectionManager、CapabilityRegistry、PolicyEngine、统一响应/错误与日志脱敏。
4. 完成 5 个通用 Tool，真实建立 SQLx、MongoDB 和 Redis 连接，执行连接探测。
5. 测试多连接 ID、连接失败、关闭和自动清理、stdout 纯净、默认网络 open、凭据脱敏。

**M0 验收**：MCP Inspector 能发现 5 个通用工具；至少三种数据库类别可以在一个进程并存；没有为远程数据库设置白名单也能正常访问授权的测试地址。

#### M1 — SQL 只读能力

1. 使用 SQLx 开发 PostgreSQL、MySQL/MariaDB、SQLite 连接与 Metadata Adapter；正确注册 SQLx Any 驱动。
2. 实现 `sql_list_databases`、`sql_list_schemas`、`sql_list_tables`、`sql_describe_table`、`sql_query`。
3. 支持各自 SQL 方言的参数绑定、结果类型保真、索引/外键等结构信息、查询超时与结果限额。
4. 针对只读工具施加单语句和权限边界；增加三种数据库真实集成测试，包含边界类型及恶意/错误 SQL。

**M1 验收**：3 种 SQL 数据库均能完成连接、Schema 探索、参数化 SELECT；只读入口拒绝 DML/DDL；输出与错误格式稳定。

#### M2 — SQL 写入、执行计划与事务

1. 实现 `sql_execute`、`sql_explain`、`sql_begin_transaction`、`sql_commit_transaction`、`sql_rollback_transaction`、`sql_get_table_stats`。
2. 使用服务器/连接/数据库账号三层写入授权；破坏性语句采用独立双重授权。
3. 事务使用固定物理连接 + `transaction_id`，正确释放和回滚；不允许并行访问同一事务造成状态损坏。
4. 覆盖 SQL 方言、EXPLAIN、隐式提交、提交/回滚、断连、取消和超时场景。

**M2 验收**：默认写入被拒；获授权后 DML 生效；提交/回滚原子性符合目标数据库行为；无遗留活动事务。

#### M3 — MongoDB 原生能力

1. 实现 9 个 `mongo_*` Tools：数据库/集合发现、结构推断、查询、聚合、CRUD、索引读取。
2. BSON Extended JSON 保留 ObjectId、Date、Decimal128、Binary 等类型信息。
3. 对写入操作与聚合 `$out`/`$merge`（含嵌套 pipeline）实施权限检查，执行文档数量/字节数/超时控制。
4. 对单节点与副本集差异正确降级或暴露 capability 限制。

**M3 验收**：真实 MongoDB 的查询、聚合、索引、受控 CRUD 通过；默认只读拒绝写入或写入型聚合。

#### M4 — Redis / Valkey 原生能力

1. 实现 10 个 `redis_*` Tools：SCAN、类型/TTL、String、Hash、List、Set、Sorted Set、信息读取、受控写入。
2. SCAN 必须游标化且有批次限制；二进制值采用 UTF-8/Base64 标识，保持无损。
3. 读写 action 使用枚举 Schema 严格校验；写入检查与 SQL/MongoDB 一致。
4. 覆盖 Redis 和 Valkey 的真实端到端测试，检查 key 大小、空集合、TTL、游标循环与错误 ACL。

**M4 验收**：只读操作、受控写入、SCAN、二进制、Redis/Valkey 兼容测试通过。

#### M5 — 稳定性、客户端集成、发布

1. 编写/更新 `README.md`、`SECURITY.md`、`CHANGELOG.md`、`docs/tool-api.md`、`docs/architecture.md`、`docs/implementation-status.md`。
2. 提供 `docker-compose.test.yml`、测试数据和只读/读写专用账户；完善 unit / contract / integration / security tests。
3. GitHub Actions 覆盖 Linux、macOS、Windows 构建，Linux 上真实数据库集成测试；准备二进制打包、校验和与发布工作流。
4. 使用 MCP Inspector 和至少两种真实客户端（如 Codex + OpenCode/Claude Code）检查工具发现、自动选择、参数输入、查询返回以及无 stdout 污染。
5. 重跑全部质量检查，查找权限绕过、误报测试、死锁、泄露、并发与资源清理缺陷，直到满足验收要求。

**M5 验收**：35 个工具均在 `tools/list` 中准确暴露并能按自身能力完成端到端测试；跨平台构建与客户端注册可复现；发布文件与实际行为一致。

### 五、重要设计细节必须落实

- `sql_query` 与 `sql_execute` 都支持可选 `transaction_id`；事务生命周期不得只靠池化普通查询模拟。
- MongoDB 查询、聚合与索引操作应保留文档类型；`mongo_describe_collection` 采样得出的字段须标记为推断结果。
- Redis `redis_scan` 结果包含 `next_cursor`；处理扫描中重复/新增 Key 的语义，不使用无限制 `KEYS`。
- 不依赖将原始 SQL 追加 `LIMIT` 作为唯一结果保护；服务端读流、结果截断及超时都必须生效。
- 参数占位符遵循 SQL 驱动方言；对 Decimal、Date、Bytes 等类型做明确映射及真实测试。
- MCP Tool 结果中不暴露原始数据库 URL、密码、未经用户请求的大量行数据；日志必须可脱敏。
- 使用 `tracing` 输出到 stderr；测试 stdout 只有协议消息。
- Tool 类别可通过 Cargo Features `sql,mongo,redis` 组合裁剪；完整默认构建包含三类。
- 对数据库特有能力与部署限制通过 `get_capabilities` 和清晰错误表示，禁止假设所有数据模型支持同一套事务/DDL/查询语义。

### 六、测试策略与阻塞处理

每完成一个阶段至少执行：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
```

对可用的 Docker/数据库测试环境执行：

```bash
docker compose -f docker-compose.test.yml up -d
# 执行项目中实现的真实集成测试命令，并采集结果
```

若 Docker、网络、测试数据库、平台编译工具链或特定 MCP Client 无法使用：

1. **明确报告哪个验证被阻塞以及直接原因**，记录可复制的检查和复现命令。
2. 提供可执行的集成测试或 CI 配置，而非“测试通过”的空白声明。
3. 持续完成同阶段其他可实现任务和后续不依赖该资源的工作。
4. 最终报告将此类项目标为 **未执行/待外部验证**；不得用单元 Mock 冒充真实数据库/客户端验收。

不得使用用户的生产数据库执行破坏性验证。不得虚构编译成功、CI 绿灯、MCP Client 兼容性、发布记录或已存在的仓库文件。

### 七、需要交付的最终产物

- 可编译的 Rust Cargo Workspace；最终可执行文件名 `dbconnector`。
- 完整 35 个 MCP Tools、输入/输出 Schemas、Tool Descriptions、权限策略及真实驱动适配。
- `dbconnector serve` 及 `--help`；支持可选 `--allow-writes`、`--allow-destructive`、`--network-policy`、`--allow-host`、`--require-db-tls`、查询/资源限制参数。
- `README.md` 安装、Database URL、Codex/OpenCode/Claude Code 的版本兼容 stdio 注册示例，以及 SQL/MongoDB/Redis 各一条完整工具调用演示。
- 项目测试、Docker 测试环境、CI、发布构建脚本、`SECURITY.md`、`CHANGELOG.md`、API 与架构文档。
- 阶段验收状态 `docs/implementation-status.md` 和最终 `docs/acceptance-report.md`，标出真实环境、执行命令、通过项、未通过项、未执行项及下一步。

### 八、结束时的汇报格式

完成当前任务时，输出以下信息（禁止缺项）：

```text
1. 工作结果：已完成阶段 M0–Mx / 未完成项
2. 文件和模块变更：关键新增与修改路径
3. Tool 清单：35 个中的已实现、已验证、未验证数与名称
4. 测试结果：具体命令、通过/失败/跳过、环境差异
5. MCP 兼容：实际验证的客户端、stdio/tools/list/tools/call 结果
6. 安全与默认行为：网络 open、只读/写入授权、TLS、敏感 URL 处理
7. 运行方法：可复制安装命令、stdio 注册命令、示例操作
8. 未解决问题：错误、依赖、阻塞、修复建议与复现步骤
```

验收完成的条件：**代码、文档、真实数据库测试、35 个 Tool 的 MCP 发现和调用、跨平台构建结果相互一致**。保持进度记录真实可靠；能够继续实施时直接实施，环境阻塞时清楚留下可复现的验收工作。

---

## 执行前的简短检查（给使用者）

将 `PLAN.md`、`AGENTS.md`、`PROMPT.md` 一起放入仓库根目录，然后在仓库中启动 Coding Agent。向其发送本文的 **「执行指令」**全文，让 Agent 自行阅读仓库、完成实现并输出真实验收证据。`AGENTS.md` 将作为后续迭代的长期工程规范，`PROMPT.md` 用于本轮开发。
