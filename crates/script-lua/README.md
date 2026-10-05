# planned-agent-script-lua

Lua 脚本宿主：把**统一工具调用**与**LLM 结构化抽取**两项能力暴露给沙箱里的 Lua 脚本。

## 它解决什么问题

让 LLM 生成的代码能干活，但不给它任意执行权：脚本只能通过 `tools.call` 碰世界，
而每条调用都走统一的 `ToolRegistry` —— 有 schema、有审计、可被宿主拦截。
脚本自己的 I/O 通道（`os` / `io` / `load` / `require`）**全部摘掉**。

## 依赖方向

```text
script-lua  →  core          （只有这一个）
```

工具能力经由 `core::tool_registry::ToolRegistryTrait` 注入，AI 能力经由
`core::ai::AiClient` 注入 —— 所以本 crate **不认识** `tool-manager`，更不认识
`mcp-rmcp` / 内置工具。任何「能按名字调工具」的东西都能当宿主。

---

## 快速开始

```rust
use std::sync::Arc;
use planned_agent_script_lua::{CancelToken, RunOptions, ScriptHost};
use serde_json::json;

// 1) 注入工具（下面讲怎么拿真的）
let host = ScriptHost::new(tools);

// 2) 注入 AI（可选；不接就没有 `llm`）
let host = host.with_ai(ai);

// 3) 跑脚本
let out = host
    .run(
        r#"
        local r = tools.call("builtin_search_files", { pattern = args.base_dir .. "/**/*.log" })
        if r.is_error then error("搜索失败: " .. tostring(r.content)) end
        return { count = #r.content, raw = r.content }
        "#,
        json!({ "base_dir": "/var/log" }),   // → 脚本里的 `args`
        &RunOptions {
            timeout: Some(std::time::Duration::from_secs(30)),
            cancel: Some(CancelToken::new()),
        },
    )
    .await?;   // -> serde_json::Value（脚本 return 的值）
```

---

## 接入真实实现

### 工具：`Arc<ToolRegistry>` 直接传

`ToolRegistry`（`planned-agent-tool-manager`）实现了 `ToolRegistryTrait`，
所以**不需要写适配器**，`Arc<ToolRegistry>` 会自动 coerce 成
`Arc<dyn ToolRegistryTrait>`：

```rust
use std::sync::Arc;
use planned_agent_script_lua::ScriptHost;
use planned_agent_tool_manager::ToolRegistry;

let registry = ToolRegistry::new();
// …按需注册（与 agent-gui 的 ToolsContext::init 一样）…
// registry.register_builtin_provider(&FilesystemProvider::new(&roots)?);
// registry.set_mcp_manager(mcp_manager.clone());   ← MCP 也走这里

let host = ScriptHost::new(Arc::new(registry));
```

> **MCP 工具不需要做任何额外的事。** `tools.call` 路由到 `ToolRegistry::call_tool`，
> 而它内部已经把 MCP / 自定义 / 内置统一起来了 —— 脚本调 `mcp_xxx` 和调
> `builtin_write_file` 是同一条路径。

### AI：从 `AiManager` 拿

```rust
use planned_agent_ai_manager::AiManager;

let manager = AiManager::from_config(configs)?;

// 默认 provider，或按名字取
let ai = manager.default()?;            // -> Arc<dyn AiClient>
// let ai = manager.get("my-provider")?;

let host = ScriptHost::new(tools).with_ai(ai);
```

不调 `with_ai` 也能跑，只是脚本里的 `llm` **不存在**（一用就报
`attempt to index a nil value (global 'llm')`）—— 不留一个每次调用都失败的空壳。

### 在 agent-gui 宿主里（本仓库的真实写法）

宿主侧两个 context 已经具备：

| context | 字段 | 位置 |
|---|---|---|
| `ToolsContext` | `registry: Arc<ToolRegistry>` | `crates/agent-gui/src/context/tools/mod.rs:59` |
| `AiContext` | `manager: Arc<AiManager>` | `crates/agent-gui/src/context/ai.rs:15` |

拿到 `Arc<ToolsContext>` / `Arc<AiContext>` 之后：

```rust
let host = ScriptHost::new(tools_ctx.registry.clone())
    .with_ai(ai_ctx.manager.default()?);
```

---

## Lua 侧 API

| API | 说明 |
|---|---|
| `args.<name>` | 计划参数。由 `run` 的第二个参数注入（JSON object → Lua table）。 |
| `tools.call(name, table)` | 调工具。入参写 Lua table，宿主转 JSON。返回 `{ call_id, content, is_error }` —— 结构化的，脚本不必解 JSON。 |
| `tools.list()` | 全部可用工具名（数组）。 |
| `llm.extract{ text, fields }` | 从 `text` 里抽 `fields` 指定的字段（`fields` 是 `名 = "这个字段是什么"`）。返回 JSON object。**只有接了 AI 才存在。** |

失败与错误：**没有 `pcall` / `xpcall` / `coroutine`**（原因见下「沙箱红线」）。

- 工具**执行失败** → `tools.call` 返回的 `r.is_error == true`，用 `if` 判断；
- 工具**调用本身失败**（未知工具 / 注册表报错）→ 抛 Lua error，冒泡成 `run` 的 `Err`。

也就是说脚本要么成功、要么整体失败，不做局部容错。这是有意的取舍：`pcall` 会把护栏
（超时 / 取消）抛出的错误一起吞掉，那比「脚本不能容错」危险得多。

---

## 护栏：超时与取消

```rust
let token = CancelToken::new();
let token_for_ui = token.clone();     // UI 上点「停止」时 token_for_ui.cancel()

host.run(script, args, &RunOptions {
    timeout: Some(Duration::from_secs(30)),
    cancel: Some(token),
    memory_limit: None,          // None → 默认 64 MiB（DEFAULT_MEMORY_LIMIT）
}).await?;
```

> ⚠️ **不要在外面再套 `tokio::time::timeout`** —— 那是无效的。脚本纯计算时没有
> `await` 点，`eval_async` 会在**一次 poll 里跑到底**，future 永远不把控制权交回去，
> 异步超时根本没机会触发。超时与取消都由 Lua debug hook 实现（见下）。

两个护栏都没设时**不装 hook**，零开销。

还有一项 `RunOptions::memory_limit`（默认 64 MiB）：debug hook 只在**两条 Lua 指令之间**
检查，而 `string.rep("x", 1e9)` 这类在 C 里一口气跑完的调用中间没有指令边界、后时也打断不了
—— 挡它靠的是内存上限，不是超时。

## 沙箱红线（改代码前先读）

1. **不要**把 `os` / `io` / `package` / `debug` 加回来，也**不要**删掉摘除 `load`
   家族 / `dofile` / `require` 的那个循环。脚本一旦有独立于工具系统的 I/O 通道，
   就等于给了 LLM 生成的代码任意执行权 —— 比现状危险一个量级（现状每条调用都是
   有 schema、有审计的显式工具调用）。
2. **也不要**把 `pcall` / `xpcall` / `coroutine` 加回来。hook 抛的是**普通 Lua error**，
   只要 `pcall` 在，`while true do pcall(function() while true do end end) end` 就能把
   deadline 一次次吞掉，让超时 / 取消**永不落地**、tokio worker 被永久占死。
   `coroutine` 则是双重问题：Lua 自建的线程拿不到 `set_global_hook` 的钩子，
   而且 `coroutine.resume` 会把 hook 的错误当返回值吞掉。`print` 也一并摘了 ——
   它是一条绕过工具系统直写宿主 stdout 的通道。
3. 护栏**必须**用 `Lua::set_global_hook`，**不是** `Lua::set_hook`。后者只作用于
   「当前线程」，而 `eval_async` 把脚本跑在 mlua 新建的**协程**里 —— hook 装不上，
   而且 `set_hook` 会**返回 `Ok`**（不报错），死循环照跑到底。
   mlua 源码 `state.rs:703` 原文：「All new threads created (by mlua) after this call
   will use the global hook function」。
4. 工具是 `async` 的，桥接只能走 `create_async_function`。**绝不能在 tokio worker 里
   `block_on`** —— 直接死锁。

---

## 测试

```powershell
cargo test -p planned-agent-script-lua
```

- `src/lib.rs` —— 业务逻辑（宿主、沙箱、护栏、抽取）
- `src/testing.rs` —— 测试桩（`FakeRegistry` / `FakeAi`），仅 `#[cfg(test)]`
- `src/tests.rs` —— 行为测试（11 例）

护栏用例刻意用**有限**循环（1e8 次）而不是 `while true do end`：万一护栏失效，
表现出来是断言失败，而不是把测试进程永久挂死。
