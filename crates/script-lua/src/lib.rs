//! Lua 脚本宿主：把「统一工具调用」能力暴露给沙箱里的 Lua 脚本。
//!
//! 这是一个**通用能力** crate —— 它不认识任何上层业务。`flexible` 的「固化脚本」
//! 只是它的第一个消费者。设计见 `docs/planned-agent/flexible-step-fixation.md`。
//!
//! # 依赖方向
//!
//! 只依赖 `planned-agent-core`：工具能力经由 [`ToolRegistryTrait`] 注入，
//! 所以本 crate **不需要**认识 `tool-manager`，更不认识 `mcp-rmcp` / 内置工具。
//! 任何「能按名字调工具」的东西都能当宿主 —— 这也让 `tools.call` 天然覆盖
//! MCP / 自定义 / 内置**全部**工具（路由是宿主的事）。
//!
//! # 两条硬约束（改这个文件前先读）
//!
//! 1. **沙箱**：只加载 `coroutine` / `table` / `string` / `math`（外加 Lua 引擎总是
//!    加载的 base），并逐个摘掉 `load` 家族 / `dofile` / `require`。脚本**不得**有
//!    独立于工具系统的 I/O 通道 —— 要文件走 `tools.call`、要命令走
//!    `builtin_execute_command`。放开 `os` / `io` 等于给 LLM 生成的代码任意执行权，
//!    比现状危险一个量级（现状每条调用都是有 schema、有审计的显式工具调用）。
//! 2. **异步桥**：Lua 同步、工具 `async`，用 `create_async_function` 让 Lua 在调用点
//!    挂起。**绝不能在 tokio worker 里 `block_on`** —— 那会直接死锁。
//!
//! # Lua 侧 API
//!
//! ```lua
//! local r = tools.call("builtin_search_files", { pattern = args.base_dir .. "/**/*.log" })
//! if r.is_error then error("搜索失败: " .. tostring(r.content)) end
//! return r.content
//! ```
//!
//! `tools.call` 返回工具结果的**结构**（`{ call_id, content, is_error }`），脚本不必解 JSON。
//! 入参直接写 Lua table，宿主转成 JSON。

use std::sync::Arc;

use anyhow::Result;
use mlua::{Lua, LuaOptions, LuaSerdeExt, StdLib, Value as LuaValue};
use planned_agent_core::tool_registry::ToolRegistryTrait;

/// Lua 侧可用的宿主能力。
pub struct ScriptHost {
    tools: Arc<dyn ToolRegistryTrait>,
}

impl ScriptHost {
    pub fn new(tools: Arc<dyn ToolRegistryTrait>) -> Self {
        Self { tools }
    }

    /// 建一个**沙箱化**的 Lua 状态，注入宿主 API。
    pub fn lua(&self) -> Result<Lua> {
        let lua = sandbox()?;
        self.install_tools(&lua)?;
        Ok(lua)
    }

    /// 注入 `tools.call(name, args) -> { call_id, content, is_error }`。
    fn install_tools(&self, lua: &Lua) -> Result<()> {
        let registry = self.tools.clone();
        let call = lua.create_async_function(
            move |lua: Lua, (name, arguments): (String, LuaValue)| {
                // mlua 0.12 的回调第一个参数已经是 owned 的 `Lua`（共享句柄），直接 move 进 async 块。
                let registry = registry.clone();
                async move {
                    let arguments: serde_json::Value = lua.from_value(arguments)?;
                    let result = registry.call_tool(&name, arguments).await.map_err(|e| {
                        mlua::Error::RuntimeError(format!("tools.call('{name}') 失败: {e:#}"))
                    })?;
                    // 工具结果本身就是结构化 `Value` —— 直接给回 Lua，脚本不必解 JSON。
                    lua.to_value(&result)
                }
            },
        )?;

        let tools = lua.create_table()?;
        tools.set("call", call)?;
        lua.globals().set("tools", tools)?;
        Ok(())
    }
}

/// 沙箱化：只加载必要标准库，并摘掉逃逸出口。
fn sandbox() -> Result<Lua> {
    // base 库（`pairs` / `tostring` / `pcall` …）是**总是**加载的，这里去不掉，
    // 所以它的逃逸入口（`load` 家族 / `dofile` / `require`）在下面逐个摘。
    let lua = Lua::new_with(
        StdLib::COROUTINE | StdLib::TABLE | StdLib::STRING | StdLib::MATH,
        LuaOptions::default(),
    )?;
    // `os` / `io` / `package` / `debug` 根本没加载 —— 不存在比事后清空更稳。
    let globals = lua.globals();
    for name in ["load", "loadstring", "loadfile", "dofile", "require"] {
        globals.set(name, LuaValue::Nil)?;
    }
    Ok(lua)
}

#[cfg(test)]
mod tests {
    use super::*;
    use planned_agent_core::mcp::types::ToolResult;
    use serde_json::{json, Value};
    use std::sync::Mutex;

    /// 只认识一个 `echo` 工具的假统一入口。
    ///
    /// 测试桩放在**使用方**（本 crate），不放 `core` —— 仓库约定。
    struct FakeRegistry {
        result: Value,
        calls: Mutex<Vec<(String, Value)>>,
    }

    #[async_trait::async_trait]
    impl ToolRegistryTrait for FakeRegistry {
        async fn call_tool(&self, tool_name: &str, arguments: Value) -> anyhow::Result<ToolResult> {
            if tool_name != "echo" {
                anyhow::bail!("未知工具: {tool_name}");
            }
            self.calls
                .lock()
                .expect("calls 锁")
                .push((tool_name.to_string(), arguments));
            Ok(ToolResult {
                call_id: "call-1".to_string(),
                content: self.result.clone(),
                is_error: false,
            })
        }

        fn tool_names(&self) -> Vec<String> {
            vec!["echo".to_string()]
        }
    }

    /// 一个注册了 `echo` 假工具的宿主。
    fn host() -> (ScriptHost, Arc<FakeRegistry>) {
        let registry = Arc::new(FakeRegistry {
            result: json!({"echoed": true}),
            calls: Mutex::new(Vec::new()),
        });
        (ScriptHost::new(registry.clone()), registry)
    }

    /// 核心一条：Lua 里 `await` 一个 async 宿主调用，真的打到注册表，且能拿回结构。
    ///
    /// 这一条通了，才说明「同步 Lua ↔ 异步工具」这座桥搭得起来。
    #[tokio::test]
    async fn lua_call_reaches_registry_and_returns_structured_result() {
        let (host, registry) = host();
        let lua = host.lua().expect("建 Lua");

        let out: LuaValue = lua
            .load(
                r#"
                local r = tools.call("echo", { path = "a.txt" })
                if r.is_error then error("不该是错误") end
                return r.content
                "#,
            )
            .eval_async()
            .await
            .expect("脚本执行");

        // 工具真的被调到了，且入参从 Lua table 转成了 JSON
        let calls = registry.calls.lock().expect("calls 锁");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "echo");
        assert_eq!(calls[0].1, json!({"path": "a.txt"}));
        drop(calls);

        // 返回值是结构化 table
        let content: Value = lua.from_value(out).expect("值转 JSON");
        assert_eq!(content, json!({"echoed": true}));
    }

    /// 沙箱：逃逸出口**不存在**。
    #[tokio::test]
    async fn sandbox_has_no_escape_hatches() {
        let (host, _) = host();
        let lua = host.lua().expect("建 Lua");
        let globals = lua.globals();
        for name in [
            "os", "io", "package", "debug", "load", "loadstring", "loadfile", "dofile", "require",
        ] {
            let got: LuaValue = globals.get(name).expect("读全局");
            assert_eq!(got, LuaValue::Nil, "`{name}` 不该可用");
        }
        // 必需的那些仍然在
        for name in ["pairs", "ipairs", "tostring", "type", "error", "pcall"] {
            let got: LuaValue = globals.get(name).expect("读全局");
            assert_ne!(got, LuaValue::Nil, "`{name}` 应当可用");
        }
    }

    /// 工具报错要能回到 Lua（可 `pcall` 捕获），而不是炸掉整个宿主。
    #[tokio::test]
    async fn tool_errors_surface_to_lua() {
        let (host, _) = host();
        let lua = host.lua().expect("建 Lua");
        let err = lua
            .load(r#"return tools.call("nonexistent", {})"#)
            .eval_async::<LuaValue>()
            .await
            .expect_err("未知工具应当报错");
        assert!(err.to_string().contains("nonexistent"), "{err}");
    }
}
