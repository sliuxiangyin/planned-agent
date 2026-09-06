//! 工具注册表 GUI 适配层
//!
//! 设计要点：
//! - `init()` **不依赖** McpContext，只注册 6 个内置 provider
//! - McpManager 由 `app()` 在 MCP 就绪后通过 `set_mcp_manager()` 延后注入
//! - ToolRegistry 内部已用 `RwLock<Option<...>>`，天然支持延后设置与将来替换
//! - **不新增任何占位/扩展 API**——按需再设计

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
            "向用户发起一批并列问题（1-4 个，彼此独立），前端渲染成逐题向导卡片：一次只显示一题并带步骤进度，用户逐题作答，到最后一题才统一提交、整体回传一次。调用后必须等待用户作答，不得自行假设或套用默认值。\n\
             \n\
             每个问题（question）提供若干选项（options）供用户选择：\n\
             - multi=false（默认）单选：点选即选中并自动进入下一题。需要「确认/跳过/执行」类决定时，把它们做成单选 options（如「执行 / 暂不执行」）。\n\
             - multi=true 多选：勾选后需点「下一步」继续。\n\
             - allow_input：每题**默认都带**一个「自定义回答」按钮（点击后原地展开输入框），供用户对预设都不满意时自由填写。单选场景下只要用户填了自定义文本，就**以该文本覆盖所选预设**作为该题答案（单选仍为单值）；仅当某问 options 已穷尽、确不需用户补充时才设 allow_input=false 隐藏。\n\
             - 卡片底部有「取消」（跳过整批）/「上一步」；若用户取消整批，回传为空串，按未作答处理。\n\
             - options[].label 给人看，options[].value 给程序用；回传用 value（缺省回 label）。每问 2-5 项，推荐项放第一个。\n\
             - 同批问题必须彼此独立、不可存在依赖；一次调用只发起一次用户交互，作答统一在最后一题一次性回传。"
                .into(),
            input_schema: json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "可选：整批问题的引导文本"
                },
                "questions": {
                    "type": "array",
                    "description": "并列问题数组（1-4 个，彼此独立）",
                    "items": {
                        "type": "object",
                        "required": ["header", "question"],
                        "properties": {
                            "header": { "type": "string", "description": "短标签（建议 ≤4 字）；同批内唯一，作为该题答案的键" },
                            "question": { "type": "string", "description": "问题全文，说明需要用户做什么决定" },
                            "multi": { "type": "boolean", "description": "false=单选（默认，点选即自动进入下一题）；true=多选（勾选后需点「下一步」）" },
                            "allow_input": { "type": "boolean", "description": "默认 true = 选项下显示「自定义回答」按钮（点击原地展开输入框；单选填了自定义即以其覆盖预设）。仅当该问 options 已穷尽、确不需用户补充时设 false 隐藏" },
                            "options": {
                                "type": "array",
                                "description": "可选答案（2-5 项；推荐项放第一个）",
                                "items": {
                                    "type": "object",
                                    "required": ["label"],
                                    "properties": {
                                        "label": { "type": "string", "description": "人看的展示文本" },
                                        "description": { "type": "string", "description": "tooltip 补充说明，可选" },
                                        "value": { "type": "string", "description": "程序用实际数据值（可选）；回传用 value，缺省回 label" }
                                    }
                                }
                            }
                        }
                    }
                }
            },
            "required": ["questions"]
        }),
    }
}
