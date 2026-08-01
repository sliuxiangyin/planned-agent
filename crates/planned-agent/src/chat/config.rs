//! 聊天配置
//!
//! `ChatConfig` 是 `ChatService` 的纯数据配置,不含任何运行时对象。
//! 通过 [`ChatConfig::default`] 可获得保守默认,再按需修改。

/// 单轮 chat 请求的完整配置。
#[derive(Debug, Clone)]
pub struct ChatConfig {
    /// 指定 AI provider 名;`None` 时使用 `AiManager` 注册的默认 provider。
    pub provider: Option<String>,
    /// system prompt **模板路径**，对应 `prompts/` 下某个 toml：
    /// - 写法：相对目录、不含 `.toml` 后缀（例如 `"chat/system"` → `prompts/chat/system.toml`）
    /// - 解析：`ChatService` 通过注入的 `PromptManager::render(path, ctx)`
    ///   渲染模板（与 `LlmCoarsePlanner` 走相同路径，支持变量替换）
    /// - `None` 时不主动注入 system message；调用方需自行保证历史首条不是 `System`
    ///   或自行追加。
    pub system_prompt_template: Option<String>,
    /// 采样温度。`None` 表示由 provider 默认值决定。
    pub temperature: Option<f32>,
    /// 最大生成 token 数。`None` 表示由 provider 默认值决定。
    pub max_tokens: Option<u32>,
    /// tool 调用的循环上限。
    ///
    /// 达到此上限后即便 AI 仍要求 tool 调用,循环也会终止,
    /// `Done` 事件的 `ChatResponse.finish_reason` 将为 `None`。
    pub max_tool_rounds: usize,
    /// 是否启用思考模式标记。
    ///
    /// 仅作为 hint 透传给底层 `AiClient`,具体行为由 provider 实现决定
    /// (例如某些模型会在请求中追加 `thinking` metadata)。
    pub enable_thinking: bool,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            provider: None,
            system_prompt_template: Some("chat/system".to_string()),
            temperature: None,
            max_tokens: None,
            max_tool_rounds: 10,
            enable_thinking: true,
        }
    }
}

impl ChatConfig {
    /// 创建默认配置(`max_tool_rounds=10`,其余 `None`/`false`)。
    pub fn new() -> Self {
        Self::default()
    }
}
