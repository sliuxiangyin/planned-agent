//! v2 聊天配置
//!
//! [`ChatConfig`] 是 [`crate::chat::ChatService`] 的纯数据配置，
//! 字段与 v1 [`crate::chat::ChatConfig`] 保持一致（不含任何子 agent 概念），
//! 通过 [`ChatConfig::default`] 获得保守默认后按需修改。

/// v2 聊天配置。
#[derive(Debug, Clone)]
pub struct ChatConfig {
    /// 指定 AI provider 名；`None` 时使用 `AiManager` 注册的默认 provider。
    pub provider: Option<String>,
    /// system prompt **模板路径**，对应 `prompts/` 下某个 toml：
    /// - 写法：相对目录、不含 `.toml` 后缀（例如 `"thorough/thorough_system"`）
    /// - 解析：`ChatService` 通过注入的 `PromptManager::render(path, ctx)`
    ///   渲染模板（与 v1 / `LlmCoarsePlanner` 相同路径，支持变量替换）
    /// - `None` 时不注入 system message；调用方需自行保证历史首条合法
    ///
    /// v2 内部维护 history，system prompt 只在首次 `send` 时注入一次，
    /// 后续 `send` 保留（历史首条已是 System 时不再重复注入）。
    pub system_prompt_template: Option<String>,
    /// 采样温度。`None` 表示由 provider 默认值决定。
    pub temperature: Option<f32>,
    /// 最大生成 token 数。`None` 表示由 provider 默认值决定。
    pub max_tokens: Option<u32>,
    /// tool 调用的循环上限。
    ///
    /// 达到此上限后即便 AI 仍要求 tool 调用，循环也会终止并发出
    /// `ChatEvent::Done`（`finish_reason` 为 `None`）。
    pub max_tool_rounds: usize,
    /// 是否启用思考模式标记（仅作 hint，具体行为由 provider 决定）。
    pub enable_thinking: bool,
    /// 工具白名单。
    ///
    /// - `None`：全部工具可用
    /// - `Some(names)`：仅白名单中的工具会暴露给 LLM
    pub allowed_tools: Option<Vec<String>>,
    /// system prompt 的 `{{ context }}` 变量值（`None` 或空串时渲染为空）。
    pub context: Option<String>,
    /// 本次执行的唯一标识（run_id）。
    ///
    /// - `None`：主 agent（UI 交互走 `BlockAndConfirm` 阻塞确认）
    /// - `Some(invocation_id)`：子 agent（UI 交互走 `EmitAndSuspend` 挂起返回，
    ///   `invocation_id` 即父 agent 调用该子 agent 时的 `tool_call_id`，前端
    ///   resume 时据此路由回正确的挂起会话）
    ///
    /// 子 agent 的 `ChatService` 是注册时创建、多次调用复用的，因此 run_id
    /// 由 runner 在每次 `start` 前通过 [`crate::chat::ChatService::set_run_id`]
    /// 动态注入（子 agent 串行执行，复用安全）。
    pub run_id: Option<String>,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            provider: None,
            system_prompt_template: Some("thorough/thorough_system".to_string()),
            temperature: None,
            max_tokens: None,
            max_tool_rounds: 10,
            enable_thinking: true,
            allowed_tools: None,
            context: None,
            run_id: None,
        }
    }
}

impl ChatConfig {
    /// 创建默认配置（`max_tool_rounds=10`，其余 `None`/`true`）。
    pub fn new() -> Self {
        Self::default()
    }
}
