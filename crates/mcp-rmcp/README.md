# planned-agent-mcp-rmcp

MCP（Model Context Protocol）接入 crate。对外只暴露**一个门面 `McpManager`**，同时承担两类职责：

- **持久化**：MCP server 配置 / 工具缓存 / 连接状态（后端可插拔：GUI 用 KV、CLI/文件用 File、测试用内存）。
- **运行时**：连接 / 断开 / 懒连接 / 工具调用与路由（`tool → server` 映射）。

> `McpConfigManager` / `McpBundle` 是 **crate 内部实现**（`pub(crate)`），不对外导出；请一律使用 `McpManager`。

---

## 目录结构 / 文件说明

```text
crates/mcp-rmcp/
├── Cargo.toml
├── README.md          # 本文档
├── src/
│   ├── lib.rs              # 对外导出：McpManager + 数据模型 + storage + McpClientImpl
│   ├── model.rs            # （未启用）规划中的数据模型集中文件
│   ├── client.rs           # McpClientImpl —— 单 server 真实连接（spawn 子进程 / 握手 / list_tools / call_tool）
│   ├── command_resolver.rs # resolve_command —— 跨平台命令解析（解决 Windows 下 `npx`→`npx.cmd` 等问题）
│   ├── tools.rs            # ToolManager —— server→tools 运行时路由表（McpManager 内部组件）
│   ├── bundle.rs           # McpBundle —— 内部持久化门面（config + status 聚合），被 McpManager 持有
│   ├── config.rs           # 数据模型（McpConfigFile/McpServerEntry/ToolEntry）+ McpConfigManager(内部)
│   ├── storage/            # 持久化 traits + 实现
│   │   ├── trait_def.rs    #   McpConfigStorage（server 列表 + tools cache）
│   │   ├── file_storage.rs / memory_storage.rs      #   config 后端：文件 / 内存
│   │   ├── status_trait.rs #   McpStatusStorage（连接状态）
│   │   └── status_file_storage.rs / status_memory_storage.rs  # status 后端：文件 / 内存
│   └── manager/            # McpManager 的 struct + impl（按主题拆分）
│       ├── mod.rs          #   struct McpManager + McpManagerInner + 构造(new / with_backends)
│       ├── routing.rs      #   运行时：连接/断开/懒连/工具注入/调用/查询 + McpManagerTrait impl
│       ├── config.rs       #   ① 服务 CRUD：load_config / add_server / update_server / delete_server
│       ├── status.rs       #   ③ 状态读写：record/get/list/delete/has_status / record_failure
│       ├── views.rs        #   load_servers / get_server（config+status join 视图）
│       └── refresh.rs      #   preload_cached_tools / refresh_server_tools
└── tests/
    └── connect_error.rs    # 连接错误分类的集成测试
```

---

## 快速开始

### 1. 构造（选择存储后端）

```ignore
use planned_agent_mcp_rmcp::McpManager;

// a) 默认文件后端（config=./data/mcp-config.json，status=./data/mcp-status.json）
let mgr = McpManager::new();

// b) 可插拔后端：config / status 各自独立选择
let mgr = McpManager::with_backends(config_storage, status_storage);
// config_storage / status_storage: Arc<dyn McpConfigStorage> / Arc<dyn McpStatusStorage>
```

### 2. 冷启动（不连接任何 server）

```ignore
// 把持久化缓存里的工具预载进内部路由表（供懒连接），并记 Ready(n)
let n = mgr.preload_cached_tools()?; // n = 预载工具数

// 把 McpManager 交给 tool-manager 的 ToolRegistry 统一注册（MCP 工具进入注册表唯一入口）
registry.set_mcp_manager(Arc::new(mgr));
```

> 之后用户首次调用某 server 的工具时，`call_tool_auto` 会**懒连接**：找到 server → 用缓存的 config 建连 → 调用。

### 3. 服务 CRUD

```ignore
// 新增 / 更新 / 删除（仅持久化；delete 会联动清 config+status+运行时路由）
mgr.add_server(entry)?;
mgr.update_server(&old_name, entry)?;
mgr.delete_server(&name)?;

// 视图（config + status 已 join，供 UI 列表）
let views = mgr.list_servers()?;      // Vec<McpServerView>
let one = mgr.get_server(&name)?;     // Option<McpServerView>
```

### 4. 刷新某 server 的工具（真正连接）

```ignore
// 连接 → list_tools → 缓存 → 更新路由表；内部自动记录 Ready(n)/Failed(kind,msg)
let (name, count) = mgr.refresh_server_tools("playwright").await?;
// 再让 tool-manager 按登记表重注册（同步 ToolRegistry）
registry.sync_mcp_server("playwright")?;
```

### 5. 运行时连接 / 断开

```ignore
// 显式 eager 连接（连 + list_tools + 登记）
mgr.connect_server(config).await?;
mgr.connect_all(vec![cfg1, cfg2]).await?;

mgr.disconnect_server("name").await?;
mgr.disconnect_all().await?;
mgr.is_server_connected("name"); // bool
```

### 6. 工具列表（只读，不触发连接）

```ignore
let tools = mgr.server_tools("name");   // 该 server 已登记工具（读登记表，无数据返回空）
let all = mgr.get_all_tools();          // 全部登记工具
```

### 7. 工具调用

```ignore
// 显式指定 server
mgr.call_tool("server", "tool", args_json).await?;
// 自动路由 + 懒连接（LLM 场景）
mgr.call_tool_auto("tool", args_json).await?;
```

### 8. 状态读写

```ignore
mgr.record_status("server", ServerStatus::ready(3, ServerStatus::now()))?;
mgr.record_failure("server", &connection_error);
let s = mgr.get_status("server")?;      // Option<ServerStatus>
let list = mgr.list_status()?;          // Vec<(String, ServerStatus)>
```

---

## 门面方法分组

| 组 | 方法 | 文件 |
|---|---|---|
| 构造 | `new()`(File) / `with_backends(config,status)` | `manager/mod.rs` |
| 服务 CRUD | `load_config` / `add_server` / `update_server` / `delete_server` | `manager/config.rs` |
| 视图 | `list_servers` / `get_server` | `manager/views.rs` |
| 状态 | `record/get/list/delete/has_status` / `record_failure` | `manager/status.rs` |
| 预载 / 刷新 | `preload_cached_tools` / `refresh_server_tools` | `manager/refresh.rs` |
| 连接 | `connect_server/all` / `disconnect_server/all` / `is_server_connected` | `manager/routing.rs` |
| 工具注入/查询 | `set_server_tools(_with_config)` / `get_all_tools` / `get_server_tools` / `server_tools` | `manager/routing.rs` |
| 调用 | `call_tool` / `call_tool_auto` | `manager/routing.rs` |
| trait | `McpManagerTrait`（供 tool-manager 注入） | `manager/routing.rs` |

**读写路径约定**

- **读路径**（`list_servers` / `get_server` / `server_tools` / 状态）一律**无副作用、不触发连接**。
- **写路径**（`refresh_server_tools` / 显式 `connect_server` / `call_tool_auto` 懒连）才真正连接/拉取。
- 连接失败返回**结构化**错误（Spawn / Handshake / Timeout + stderr），不被吞成字符串。

---

## 与上下游的关系

```text
planned-agent-core (core)
   ├── McpClient trait          ← client.rs 的 McpClientImpl 实现
   └── McpManagerTrait          ← manager/routing.rs 的 McpManager 实现
                                  （供 tool-manager 以 Arc<dyn McpManagerTrait> 注入）

mcp-rmcp(McpManager) ──(Arc<dyn McpManagerTrait>)──▶ tool-manager(ToolRegistry)
```

- **不依赖 tool-manager**（依赖方向反转，`McpManagerTrait` 下沉 core）。
- 需要"拿工具/调用工具"给 LLM 的上层，通常把 `McpManager` 注入 `ToolRegistry`，之后经 `call_tool` 路由。

---

## 存储后端选择

`McpConfigStorage`（server + tools cache）与 `McpStatusStorage`（状态）是**两个独立 trait**，可各自选择后端：

- 文件实现：`FileMcpConfigStorage` / `FileMcpStatusStorage`
- 内存实现：`InMemoryMcpConfigStorage` / `InMemoryMcpStatusStorage`
- 自定义：实现对应 trait 后经 `McpManager::with_backends` 注入。
