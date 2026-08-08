//! 工具注册表 GUI 适配层
//!
//! 设计要点：
//! - `init()` **不依赖** McpContext，只注册 6 个内置 provider
//! - McpManager 由 `app()` 在 MCP 就绪后通过 `set_mcp_manager()` 延后注入
//! - ToolRegistry 内部已用 `RwLock<Option<...>>`，天然支持延后设置与将来替换
//! - **不新增任何占位/扩展 API**——按需再设计

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use anyhow::Result;
use serde_json::{json, Value};

use planned_agent_core::tool_registry::{ToolCategory, ToolExecutor};
use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_mcp_rmcp::McpManager;
use planned_agent_tool_manager::builtin::{
    ai_tools::AiToolsProvider, data_tools::DataToolsProvider, doc_tools::DocToolsProvider,
    file_tools::FileToolsProvider, system_tools::SystemToolsProvider,
    text_tools::TextToolsProvider, web_tools::WebToolsProvider,
};
use planned_agent_tool_manager::ToolRegistry;

/// GUI 层 Tools 上下文
///
/// 组件通过 `use_context::<Resource<Option<Arc<ToolsContext>>>>()` 获取，
/// 再通过 `ctx.registry.get_all_tools()` / `ctx.registry.call_tool(...)` 访问工具。
pub struct ToolsContext {
    pub registry: Arc<ToolRegistry>,
}

// Arc<ToolRegistry> 不实现 PartialEq；我们手动用指针地址比较
impl PartialEq for ToolsContext {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.registry, &other.registry)
    }
}

impl ToolsContext {
    /// 同步初始化：构造 ToolRegistry + 注册 7 个内置 provider
    ///
    /// 此时 `mcp_manager` 为 None；MCP 工具由后续 `set_mcp_manager` 触发注入。
    pub fn init(docs_dir: PathBuf) -> anyhow::Result<Self> {
        let registry = ToolRegistry::new();

        // 按 CLI 既有顺序注册内置 provider（顺序无功能影响，仅日志可读性）
        registry.register_builtin_provider(&FileToolsProvider);
        registry.register_builtin_provider(&TextToolsProvider);
        registry.register_builtin_provider(&SystemToolsProvider);
        registry.register_builtin_provider(&DataToolsProvider);
        registry.register_builtin_provider(&AiToolsProvider);
        registry.register_builtin_provider(&WebToolsProvider);
        registry.register_builtin_provider(&DocToolsProvider::new(docs_dir));

        // 注册 UI 交互工具 `request_user_action`（前端拦截，不实际执行）
        registry.register_custom_tool(
            request_user_action_tool(),
            vec![ToolCategory::Utility],
            Arc::new(NoopExecutor),
        );

        let stats = registry.get_stats();
        tracing::info!(
            "ToolRegistry 初始化完成（仅内置）: {} builtin tools",
            stats.builtin_count
        );

        Ok(Self {
            registry: Arc::new(registry),
        })
    }

    /// 延后注入 McpManager：转发到 `ToolRegistry::set_mcp_manager`
    ///
    /// 调用时机：McpContext 异步初始化完成后由 `app()` 主动调用。
    /// 多次调用行为：以后一次为准（McpManagerTrait 整体替换）。
    pub fn set_mcp_manager(&self, mgr: Arc<McpManager>) {
        self.registry.set_mcp_manager(mgr);
        let stats = self.registry.get_stats();
        tracing::info!(
            "MCP 注入完成: 总计 {} 工具 (内置 {} / MCP {})",
            stats.total,
            stats.builtin_count,
            stats.mcp_count
        );
    }
}

// ── UI 交互工具定义 ─────────────────────────────────────────────────────────

/// 空操作执行器——`request_user_action` 工具由前端拦截处理，后端永不实际执行。
struct NoopExecutor;

#[async_trait]
impl ToolExecutor for NoopExecutor {
    async fn execute(&self, _tool_name: &str, _arguments: Value) -> Result<ToolResult> {
        Ok(ToolResult {
            call_id: String::new(),
            content: Value::String("ok".into()),
            is_error: false,
        })
    }

    fn name(&self) -> &str {
        "NoopExecutor"
    }

    fn description(&self) -> &str {
        "No-op executor for frontend-intercepted tools"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec!["request_user_action".into()]
    }
}

/// 构造 `request_user_action` 工具定义
fn request_user_action_tool() -> Tool {
    Tool {
        name: "request_user_action".into(),
        description:
            "当需要用户确认、选择或补充信息时调用。用于引导用户完善需求、确认计划生成等交互场景。\
             调用后等待用户操作，不要自行假设用户选择。"
                .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "展示给用户的引导文本，应清晰说明需要用户做什么决定"
                },
                "actions": {
                    "type": "array",
                    "description": "用户可选的动作列表（按钮/选项）",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "动作唯一标识"
                            },
                            "type": {
                                "type": "string",
                                "enum": ["confirm", "select", "input", "multi_select"],
                                "description": "动作类型：confirm=确认按钮, select=单选列表, input=文本输入提示, multi_select=多选复选框"
                            },
                            "label": {
                                "type": "string",
                                "description": "展示文本"
                            },
                            "description": {
                                "type": "string",
                                "description": "补充说明，可选"
                            },
                            "options": {
                                "type": "array",
                                "description": "MultiSelect 的复选框选项列表（仅 multi_select 类型使用）",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string", "description": "选项唯一标识" },
                                        "label": { "type": "string", "description": "选项展示文本" },
                                        "value": { "type": "string", "description": "选项的实际数据值（推荐）。勾选后回传为 id=value；不填则仅回传 id" },
                                        "default": { "type": "boolean", "description": "是否默认勾选，默认 false" }
                                    },
                                    "required": ["id", "label"]
                                }
                            }
                        },
                        "required": ["id", "type", "label"]
                    }
                }
            },
            "required": ["message", "actions"]
        }),
    }
}