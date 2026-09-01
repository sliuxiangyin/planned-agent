//! 工具注册表 GUI 适配层
//!
//! 设计要点：
//! - `init()` **不依赖** McpContext，只注册 6 个内置 provider
//! - McpManager 由 `app()` 在 MCP 就绪后通过 `set_mcp_manager()` 延后注入
//! - ToolRegistry 内部已用 `RwLock<Option<...>>`，天然支持延后设置与将来替换
//! - **不新增任何占位/扩展 API**——按需再设计

pub mod plans_flexible_tool;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::{ToolCategory, ToolExecutor};
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

    // ========== 自定义工具（运行时增删）==========

    /// 运行时注册自定义工具（透传 `ToolRegistry::register_custom_tool`）
    ///
    /// - 适用场景：插件/动态加载/前端拦截工具接入
    /// - 已存在同名自定义工具：静默覆盖（与底层一致）
    /// - 同时覆盖 `tools`、`metadata`、`custom_executors`
    pub fn register_custom_tool(
        &self,
        tool: Tool,
        categories: Vec<ToolCategory>,
        executor: Arc<dyn ToolExecutor>,
    ) {
        let name = tool.name.clone();
        self.registry
            .register_custom_tool(tool, categories, executor);
        tracing::info!("已注册自定义工具: {}", name);
    }

    /// 运行时卸载自定义工具（透传 `ToolRegistry::unregister_tool`）
    ///
    /// - 仅卸载元数据中 `source == Custom` 的工具；若传入的是 Builtin / MCP 工具名，底层同样会移除
    /// - 工具不存在时返回 `Err`，调用方需自行决定是否吞错
    /// - 不区分「软禁用」与「真删除」，按需再设计
    pub fn unregister_custom_tool(&self, name: &str) -> anyhow::Result<()> {
        self.registry.unregister_tool(name)?;
        tracing::info!("已卸载自定义工具: {}", name);
        Ok(())
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
            "请求用户进行确认、选择或补充信息。调用后必须等待用户响应，不得自行假设用户选择。\n\
             \n\
             动作类型：\n\
             - select：单选列表，可并列多个，自动附带「自定义输入」入口（若选项已穷尽，可设 allow_custom=false 禁用）。\n\
             - multi_select：多选复选框，需搭配一个 confirm 按钮提交；同样自带自定义输入入口（可用 allow_custom=false 禁用）。\n\
             - confirm：确认/批准/跳过类决定；禁止单独用作追问（追问必须提供 select 选项）。\n\
             - input：自由文本输入，通常配 confirm 提交；不要与 select 混搭（前端会丢弃 select，只保留 input）。\n\
             \n\
             组合规则：\n\
             - 追问（让用户选一项）→ 并列多个 select。\n\
             - 多选（让用户勾选多项）→ 一个 multi_select + 一个 confirm 提交。\n\
             - 确认/批准/跳过 → 单个 confirm。\n\
             - 自由输入 → 一个 input（通常配 confirm 提交）。\n\
             - 禁止 select 与 input 混搭。"
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
                                "description": "动作类型：select=单选列表（追问/提供选项专用，自动带自定义输入入口，可多个并列）, confirm=确认/批准/跳过（仅确认场景，禁止用作追问选项）, input=文本输入提示, multi_select=多选复选框"
                            },
                            "label": {
                                "type": "string",
                                "description": "展示文本"
                            },
                            "description": {
                                "type": "string",
                                "description": "补充说明，可选"
                            },
                            "allow_custom": {
                                "type": "boolean",
                                "description": "是否附带「补充输入」入口（仅 select / multi_select 有效，默认 true）。选项已穷尽、无需用户补充时可置 false 隐藏补充输入框"
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
