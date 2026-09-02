# MCP 层统一重构方案 —— `McpManager` 单门面

> 状态：**已实施（M1–M6）；M7 收尾验证**
> 覆盖 crates：`mcp-rmcp` / `core` / `tool-manager` / `agent-gui`
> 说明：本实施不涉及 `planned-agent`（其不含 MCP 装配，且其 `src/agent.rs` 等在工作区为并发未提交 WIP、已被删除）。本文档是蓝图；完成后 `docs/mcp-rmcp.md` 已更新现行架构。

## 1. 动机与问题

当前 MCP 相关代码存在几处重复/心智负担：

1. **重复注册**：冷启动缓存工具被注册两遍——GUI `register_cached_tools` 手动注册一次，随后 `ToolRegistry::set_mcp_manager` 又 `get_all_tools()` 全量注册一次（见 logs 中 `browser_*` 出现两次）。
2. **双门面**：`McpManager`（运行时连接）与 `McpBundle`（持久化 config+status）是两个正交对象，`agent-gui` 的 `McpContext` 被迫同时协调两者。
3. **GUI / CLI 各自为政**：`agent-gui` 冷启动预载与 `planned-agent::agent::connect_mcp_servers` 几乎逐行重复（读配置 → `set_server_tools_with_config` → `set_mcp_manager`）。
4. **分类映射两处重复**：GUI `parse_categories`（字符串→`ToolCategory`）与 `tool-manager/core/registry.rs` 内部同款 match 各一份。
5. **协调逻辑堆在 GUI**：`refresh_tools` / 预载等业务装配写在 `agent-gui`（McpContext），GUI 层不该承载。

## 2. 目标架构（三层各管各的）

```
mcp-rmcp     : McpManager = 唯一门面（持久化 config/status + 运行时连接 + 拉取刷新）
               不依赖 tool-manager（保持依赖反转：McpManagerTrait 仍在 core）
tool-manager : ToolRegistry = 唯一"注册/反注册"入口
               新增"按 server 同步注册"，分类映射收归一处
planned-agent: 提供薄协调装配函数（GUI 与 CLI bin 共用），消灭各自重复
agent-gui    : 只剩 DI（选 KV/File）+ 调用门面；McpContext 不再写业务编排
```

### 依赖方向（保持不变）

```
core ←─ tool-manager（注册，消费 core::McpManagerTrait）
core ←─ mcp-rmcp    （实现 core::McpClient / core::McpManagerTrait）
core ←─ tool-manager ←─(无)  mcp-rmcp      // 二者互不依赖
planned-agent 依赖 mcp-rmcp + tool-manager（提供装配）
agent-gui 依赖 planned-agent + mcp-rmcp + tool-manager
```

## 3. 已确认决策

| # | 决策 | 内容 |
|---|---|---|
| D1 | 公开面 | **彻底单门面**：删除公开类型 `McpConfigManager` 与 `McpBundle`，方法全部并入 `McpManager`。`lib.rs` 只导出 `McpManager` + 数据类型 + `storage`。 |
| D2 | core trait | **接受给 `McpManagerTrait` 加方法**：新增 `get_server_tools(&self, server: &str) -> Vec<Tool>`，以支撑 tool-manager 按单个 server 同步注册。同步实现：`McpManager`、`McpManagerAdapter`。 |
| D3 | 方案落盘 | 先落盘本文档再开始实施。 |

---

## 4. A. `crates/mcp-rmcp` —— 合并为 `McpManager` 单门面（impl 分文件）

### 现状 → 目标文件

现状：`config.rs`(数据+McpConfigManager)、`bundle.rs`(门面)、`manager.rs`(连接)、`tools.rs`(内部路由表)、`storage/`、`client.rs`、`command_resolver.rs`。

目标：

```
src/
  lib.rs               # 只导出 McpManager + 数据类型 + storage；不再导出 McpConfigManager / McpBundle
  model.rs             # 数据模型：McpConfigFile / McpServerEntry / ToolEntry / McpServerView + 转换
  client.rs            # McpClientImpl（不动）
  command_resolver.rs  #（不动）
  tools.rs             # ToolManager(server→tools 路由表)，manager 内部组件（保留）
  storage/             # trait + File/InMemory（不动；清理指向旧 manager 的 stale doc）
  manager/
    mod.rs             # struct McpManager{ inner(运行时), config:Arc<dyn McpConfigStorage>, status:Arc<dyn McpStatusStorage> }
                       #   + 构造：new()=File 默认 / with_backends(config,status) 异构后端 / init(加载+预载)
                       #   + impl McpManagerTrait（含新增 get_server_tools）
    config.rs          # impl：load/save/add/update/delete_server/cache_tools/…（原 McpConfigManager）
    status.rs          # impl：record_status/get/load_all/delete_status/has_status（原 status 转发）
    views.rs           # impl：load_servers / get_server → McpServerView（原 bundle join）
    routing.rs         # impl：connect/disconnect/call_tool/call_tool_auto/set_server_tools*_with_config（原 manager.rs）
    refresh.rs         # impl：fetch_and_cache_tools / preload_cached（供 init）
```

技术要点：
- `manager.rs`（单文件）改为 `manager/` 目录：struct 定义与 impl 块分离到 `mod.rs` + 各主题文件。同目录不允许 `manager.rs` 与 `manager/` 并存，因此**删除 `manager.rs`，整体迁入 `manager/`**。
- struct 私有字段放 `manager/mod.rs`；`manager/config.rs` 等子模块用 `impl crate::manager::McpManager` 分主题实现，子模块可访问父模块私有字段（Rust 可见性规则）。
- 运行时 `clients / tool_manager / server_configs` 归入 `inner: Arc<RwLock<McpManagerInner>>`；config/status 两个 storage 是同步 `Send + Sync`，放 struct 外层字段（`Arc<dyn ...>`），无需 RwLock。
- 构造保持后端可插拔：`McpManager::new()`（File 默认，CLI 兼容）、`McpManager::with_backends(config_storage, status_storage)`（GUI KV / 异构）。
- `init()`（或构造后一次调用）负责：加载 config → 打印 → 把有缓存的 server 预载进自身路由表（`set_server_tools_with_config`）→ 记录 Ready status。**不连接任何 server。**

### A.0 对外门面 API 分组（讨论确认版）

一个 `McpManager` struct、方法按主题分组、impl 拆 `manager/` 下各文件。全部方法挂在同一对象上，对外只此一门面。

```
构造（可插拔后端 ⑤）
  new()                                   # File 默认（CLI 兼容）
  with_backends(config_storage, status_storage)   # GUI KV / 异构

── ① 服务/持久化管理（对应第 1 类）──
  list_servers() -> Vec<McpServerView>     # 原 bundle.load_servers，join status
  get_server(name) -> Option<McpServerView>
  add_server(entry) / update_server(old, entry) / delete_server(name)  # delete 联动清 cache+status
  save_server(entry)                       # 编辑器"保存"，add/update 统一入口（可选）
  probe_connection(entry)                  # 仅测连，不写库（"保存并测试"用）

── ② 工具刷新/测连+缓存（对应第 1 类"插入/刷新时测连+缓存"）──
  refresh_server_tools(name)               # 连→list_tools→缓存→更新内部路由表；
                                           # 成功自动记 Ready(n)，失败记 Failed(kind,msg)；
                                           # 失败返回 ③ 结构化 ConnectionError，不阻塞保存流程

── ③ 连接与状态（对应第 3 类 + 断开/状态）──
  connect_server(name) / connect_all()     # 显式 eager 连接（连+list+登记）
  disconnect_server(name) / disconnect_all()
  is_connected(name)
  get_server_status(name) / list_status()  # 状态读写（status 内部自动维护）
  get_server_categories(name)              # 供 ToolRegistry 分类

── ④ 工具列表（对应第 2 类，甲语义）──
  server_tools(name) -> Vec<Tool>          # 读运行时登记表，不触发连接；
                                           # 无数据返回空（数据由预载/刷新/连接写路径填好）
  get_all_tools()                          # 全量（供 LLM / McpManagerTrait）
  # 持久化缓存 get_cached_tools 降为内部实现，不对外

── ⑤ 工具调用（对应第 4 类）──
  call_tool(server, name, args)            # 显式指定 server
  call_tool_auto(name, args)               # 懒路由：未连→懒连→调（McpManagerTrait 用）
```

要点：
- 服务 CRUD / 刷新 / 连接 / 状态 / 调用都收敛到这一个对象，无第二个门面；方法按主题分布在 `manager/config.rs` / `status.rs` / `views.rs` / `routing.rs` / `refresh.rs`。
- 读路径（`list_servers` / `server_tools` / 状态）一律**无副作用、不触发连接**；连接/拉取只发生在写路径（刷新 / 显式 connect / 懒连触发）。
- `server_tools` 采用**甲语义**：读登记表，未登记返回空；不查询时懒拉。
- 连接失败返回**结构化** `ConnectionError`（Spawn/Handshake/Timeout + stderr），不被吞成字符串。
- 该面与 core `McpManagerTrait` 对齐（含 D2 新增 `get_server_tools`），供 tool-manager 以 `Arc<dyn McpManagerTrait>` 注入。

### 公开 API 变化（mcp-rmcp）
- 删除：`McpConfigManager`、`McpBundle` 两个公开类型。
- 新增 `McpManager` 方法：原 `McpConfigManager`/`McpBundle`/`manager.rs` 全部方法合并；新增 `get_server_tools`（D2，trait 实现）。
- 保留导出：`McpServerEntry`/`McpServerConfig`/`Tool`/`ToolEntry`/`McpServerView`/`ServerStatus`/`McpConfigFile`/storage traits + File/InMemory 实现/`McpClientImpl`。

## 5. B. `crates/core` —— `McpManagerTrait` 增方法（D2）

`crates/core/src/tool_registry/traits.rs` 的 `McpManagerTrait` 新增：

```rust
fn get_server_tools(&self, server_name: &str) -> Vec<Tool>;
```

影响实现者（需全部同步补）：
- `mcp-rmcp::manager::McpManager`
- `tool-manager::adapter::mcp::McpManagerAdapter`（透传 inner）
- 其它 mock / 测试实现者（实施时 grep `impl McpManagerTrait` 全仓确认）

## 6. C. `crates/tool-manager` —— 注册收敛到 ToolRegistry（3B）

`crates/tool-manager/src/core/registry.rs`：
- 把"字符串→`ToolCategory`"映射从 `set_mcp_manager` 内抽成私有 helper（`map_server_categories(&[String]) -> Vec<ToolCategory>`）。
- 新增 `pub fn sync_mcp_server(&self, server_name: &str) -> Result<usize>`：
  1. `unregister_mcp_server_tools(server_name)` 卸旧；
  2. 从已注入的 `McpManagerTrait`（`get_server_tools` + `get_server_categories`）重新注册该 server 的工具；返回注册数。
- `set_mcp_manager` 复用同一注册逻辑做全量注册（遍历 `get_server_names`），保持"覆盖式、幂等"语义。
- 注：`sync_mcp_server` 为同步方法（unregister/register 均同步）；异步的"连接拉取"在 mcp-rmcp `manager` 侧。

## 7. D. `crates/planned-agent` —— 薄协调装配（lib 层）

在 `planned_agent`（lib）暴露装配函数，供 GUI 与 CLI bin 复用：
- `setup_mcp(registry: &ToolRegistry, manager: Arc<McpManager>)`：等价于"预载已在 init 完成 + 调用 `registry.set_mcp_manager`"的冷启动装配（GUI/CLI 各一行）。
- `refresh_mcp_server(registry, manager, name)`：`manager.fetch_and_cache_tools`（更新自身路由+状态）→ `registry.sync_mcp_server(name)`。
- CLI `agent.rs::connect_mcp_servers` 改为调用这些装配，删除逐行重复逻辑；同时修正其中的硬编码路径 `/home/code/...`。

## 8. E. `crates/agent-gui` —— McpContext 变薄

- `context/mcp.rs`：
  - `McpContext` 只持 `manager: Arc<McpManager>`，**删除 `bundle` 字段**。
  - `init()`：仍负责按场景选 KV/File 后端并构造 `manager`（DI 保留在 GUI），加载/预载交给 manager 内部完成。
  - `load_servers`/`delete_server`/`refresh_tools` 改为薄转发（refresh 走 `planned_agent` 装配或 tool-manager 的 `sync_mcp_server`）。
  - **删除** `parse_categories` / `register_tools_to_registry`（分类映射已归 tool-manager）、重复的 config 加载。
- `main.rs`：`McpContext::init` 完成后 `use_effect` 只剩一行装配（`registry.set_mcp_manager(manager)`）。
- `pages/mcp/editor_page.rs`：改用 manager 的 config CRUD + 状态；不再依赖 `bundle.config_manager()`。
- `pages/mcp/list_page.rs`：改调 manager / 装配方法。
- `context/init_status.rs`：适配 `McpContext` 结构变化。

---

## 9. 实施顺序（每步 `cargo check`）

1. **A → B 依赖基础**：先 `core` 加 trait 方法（B）会让 mcp-rmcp/tool-manager 编译失败，故先 B 的空实现过渡不可行。改为：
   1. **A-基础**：mcp-rmcp 内部合并 McpManager（先不动公开删除，类型先保留），编译绿。
   2. **B**：core trait 加 `get_server_tools`；同步实现 McpManager + McpManagerAdapter，编译绿。
   3. **A-收口**：删除 `McpConfigManager`/`McpBundle` 公开类型，迁移调用方。
   4. **C**：tool-manager 注册 helper + `sync_mcp_server`。
   5. **D**：planned-agent 装配函数 + CLI 收敛。
   6. **E**：GUI McpContext/页面瘦身。
2. 每步 `cargo check`（`-p planned-agent-gui` / `-p planned-agent`），最后全仓 `cargo check --workspace`。
3. 完成后 `cargo run -p planned-agent-gui` 实测冷启动日志（确认 `browser_*` 只注册一次）并核对刷新/删除流程。

## 10. 风险与注意

- **实现者/mock 同步**：加 trait 方法会破坏 mcp-rmcp 之外所有 `impl McpManagerTrait`；实施前 `grep 'impl McpManagerTrait'` 全仓盘点。
- **editor_page / list_page** 直接依赖旧对象，迁移要仔细，属 GUI 主流程。
- **文档漂移**：`docs/mcp-rmcp.md`/`tool-manager.md`/`core.md` 及源码内多处 doc 注释（如 `storage/trait_def.rs`、`status_trait.rs` 引用 `McpConfigManager`/`McpStatusManager`）会过时，需一并清理。
- **重构面大**：跨 5 个 crate，建议小步、每步编译绿、最后统一 review。

## 11. 后续维护清单（实施完成后）

- [ ] 更新 `docs/mcp-rmcp.md`（改为 McpManager 单门面描述）
- [ ] 更新 `docs/tool-manager.md`（注册/同步方法）
- [ ] 更新 `docs/core.md`（McpManagerTrait 新方法）
- [ ] 清理 mcp-rmcp storage doc 中指向旧 manager 的引用
