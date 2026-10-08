//! v2 聊天配置
//!
//! [`ChatConfig`] 是 [`crate::chat::ChatService`] 的纯数据配置，
//! 字段与 v1 [`crate::chat::ChatConfig`] 保持一致（不含任何子 agent 概念），
//! 通过 [`ChatConfig::default`] 获得保守默认后按需修改。

use std::sync::Arc;

use async_trait::async_trait;

/// 每轮请求前现算的「临时上下文」来源。
///
/// 由宿主实现（如 GUI 的灵活模式现读 `flexible_state`），driver 在**每轮**组请求时调用
/// [`render`](PerRoundContextSource::render)，把返回文本作为一条**临时 system 消息**插入
/// 本轮请求 —— **不写入 history**，故不落库、不出现在 UI，也不会随轮次累积或在历史里陈旧化。
#[async_trait]
pub trait PerRoundContextSource: Send + Sync {
    /// 返回本轮要注入的文本；`None` 表示本轮不注入。
    ///
    /// 注入是**增强项、不是对话的前置条件**：实现方读不到数据时应记日志并返回 `None`
    /// （让本轮照常进行），而不是把错误抛上来中断对话。
    async fn render(&self) -> Option<String>;
}

/// [`PerRoundContextSource`] 的持有者（由 [`ChatConfig`] 持有）。
///
/// 包一层 newtype 是为了让 `ChatConfig` 继续 `derive(Clone)` 并手写 `Debug`
/// —— trait 对象本身不满足 `Debug`。
#[derive(Clone)]
pub struct PerRoundContext(Arc<dyn PerRoundContextSource>);

impl PerRoundContext {
    /// 包装一个来源。
    pub fn new(source: impl PerRoundContextSource + 'static) -> Self {
        Self(Arc::new(source))
    }

    /// 取内部来源（driver 用，crate 内可见）。
    pub(crate) fn source(&self) -> &Arc<dyn PerRoundContextSource> {
        &self.0
    }
}

impl std::fmt::Debug for PerRoundContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PerRoundContext(..)")
    }
}

/// system prompt 的来源：模板路径或已渲染好的字符串，二者互不耦合。
#[derive(Debug, Clone)]
pub enum SystemPrompt {
    /// 模板路径，对应 `prompts/` 下某个 toml（相对目录、不含 `.toml` 后缀，
    /// 例如 `"thorough/thorough_system"`）。运行时由注入的 `PromptManager`
    /// 渲染（使用空的 `PromptContext`）。
    Template(String),
    /// 已经渲染好的 system prompt 字符串，直接作为 system message 注入，
    /// 不再经过 `PromptManager`。适合调用方自行构建 `PromptContext`、
    /// 调用 `PromptManager::render(path, &ctx)` 后把结果传进来。
    Rendered(String),
}

/// v2 聊天配置。
#[derive(Debug, Clone)]
pub struct ChatConfig {
    /// 指定 AI provider 名；`None` 时使用 `AiManager` 注册的默认 provider。
    pub provider: Option<String>,
    /// system prompt 来源，二选一（两种来源互不耦合）：
    /// - [`SystemPrompt::Template`]：传模板 path，内部通过注入的
    ///   `PromptManager::render(path, ctx)` 渲染（与 v1 / `LlmCoarsePlanner` 同路径）
    /// - [`SystemPrompt::Rendered`]：传已渲染好的 prompt 字符串，直接注入
    /// - `None`：不注入 system message；调用方需自行保证历史首条合法
    ///
    /// v2 内部维护 history，system prompt 只在首次 `send` 时注入一次，
    /// 后续 `send` 保留（历史首条已是 System 时不再重复注入）。
    pub system_prompt: Option<SystemPrompt>,
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
    /// 工具白名单：控制哪些工具暴露给 LLM。
    ///
    /// - `None`：**全部**工具可用（含 `Utility`、`SubAgent` 等协调/专属类工具，不过滤）。
    /// - `Some(tokens)`：仅按下列 token 命中的工具暴露（各 token 取**并集**）：
    ///   - `"all"`：加载**除 `Utility` 与 `SubAgent` 两类外**的全部工具。
    ///     适合"纯业务执行"型 agent（如参数化步 / thorough），避免背上数据库查找、专属工具、
    ///     以及各类子 agent 工具。
    ///   - 分类名：如 `"Utility"` / `"SubAgent"` / `"Browser"` / `"Data"` 等，加载该分类下全部工具。
    ///   - 精确工具名：如 `"flexible_state"` / `"flexible_clarify"`，加载该工具（跨分类放行）。
    ///
    /// 示例：
    /// - 协调器只做调度，要 4 个 step 子 agent（需求澄清 / 计划 / 参数化 / 保存）+ `flexible_state` + 用户交互、不要业务工具：
    ///   `Some(["flexible_clarify","flexible_plan","flexible_parameterize","flexible_save",
    ///         "flexible_state","request_user_action"])`
    /// - 参数化步要全部业务工具但不要 Utility/SubAgent：
    ///   `Some(["all"])`
    /// - 全量基础上再补回整个 Utility 与 SubAgent 类（≈ 等价 `None`，但显式）：
    ///   `Some(["all","Utility","SubAgent"])`
    pub allowed_tools: Option<Vec<String>>,
    /// 本次执行的唯一标识（run_id）。
    ///
    /// - `None`：主 agent（UI 交互走 `BlockAndConfirm` 阻塞确认）
    /// - `Some(invocation_id)`：子 agent（UI 交互走 `EmitAndSuspend` 挂起返回，
    ///   `invocation_id` 即父 agent 调用该子 agent 时的 `tool_call_id`，前端
    ///   resume 时据此路由回正确的挂起会话）
    ///
    /// 子 agent 的 `ChatService` 在每次 `start()` 时新建、完成即 drop，
    /// run_id 在构造时写入 config，天然隔离。
    pub run_id: Option<String>,
    /// 不写入子 agent task 文本的参数名（宿主/调用方注入的控制字段）。
    ///
    /// [`crate::chat::SubAgentRunner`] 会把父 agent 传入的 `arguments` 序列化后作为子 agent
    /// 的 task 文本；其中子 agent 用不到的**控制字段**（如宿主会话标识）可在此列出，
    /// 避免泄漏进子 agent 上下文。默认空（不减任何参数，保持既有行为）。
    pub hidden_args: Vec<String>,
    /// 每轮请求前现算的「临时上下文」来源（见 [`PerRoundContextSource`]）。
    ///
    /// 返回文本作为一条**临时 system 消息**注入**本轮请求**：不写 history —— 所以它每轮都是
    /// 最新的，也不会像 tool 结果那样留在历史里变成过时信息（历史里的旧快照是幻觉的燃料）。
    ///
    /// `None`（默认）：不注入，行为与以前完全一致。
    pub per_round_context: Option<PerRoundContext>,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            provider: None,
            system_prompt: Some(SystemPrompt::Template(
                "thorough/thorough_system".to_string(),
            )),
            temperature: None,
            max_tokens: None,
            max_tool_rounds: 10,
            enable_thinking: true,
            allowed_tools: None,
            run_id: None,
            hidden_args: Vec::new(),
            per_round_context: None,
        }
    }
}

impl ChatConfig {
    /// 创建默认配置（`max_tool_rounds=10`，其余 `None`/`true`）。
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认**不注入** —— 这是"对既有使用方零影响"的硬保证：只有显式设置了
    /// `per_round_context` 的会话（当前仅灵活模式协调器）才会多出一条临时 system 消息，
    /// 其余（普通 chat / 子 agent / testkit）的请求与以前逐字节一致。
    #[test]
    fn default_has_no_per_round_context() {
        assert!(ChatConfig::default().per_round_context.is_none());
    }

    /// `Debug` 是手写实现、只打占位符：来源里可能持有 service 与会话 ID，不应外泄到日志。
    #[test]
    fn debug_does_not_expose_source() {
        struct Dummy;

        #[async_trait]
        impl PerRoundContextSource for Dummy {
            async fn render(&self) -> Option<String> {
                None
            }
        }

        let ctx = PerRoundContext::new(Dummy);
        assert_eq!(format!("{ctx:?}"), "PerRoundContext(..)");
        let _cloned = ctx.clone(); // ChatConfig 需要 Clone，来源必须可克隆
    }
}
