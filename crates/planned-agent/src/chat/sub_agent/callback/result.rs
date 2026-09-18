//! 子 agent **完成后**回调：决定最终 tool result 的内容。
//!
//! # 一条链的三个组成部分
//!
//! ```text
//! prelude（前置分析，可选）→ callback*（业务回调，串行）
//! ```
//!
//! - [`SubAgentChainPrelude`]：链跑之前跑**一次**的前置分析（解析输出、判定定稿、
//!   定位归属等）。产出交给链上每个回调（[`SubAgentCall::analysis`]），并可**定稿对外
//!   文本**（[`PreludeOutcome::Proceed::outer`]）。
//! - [`SubAgentResultCallback`]：串行业务回调，各自决策。
//! - [`ResultDecision`]：回调的决策，决定「对外结果」与「是否续链」。
//!
//! 为什么 prelude 是**框架槽位**而不是「链首再挂一个回调」：它是所有回调的共同前置
//! 条件（解析失败要统一重试、定稿前不该写任何产物），靠每个注册点手动挂容易漏、顺序
//! 也容易放错；做成槽位后框架保证「先分析、再跑链」。但核心库**不解释分析规则** ——
//! 业务字段名（如 `status` / `host_session_id`）由注册方在 prelude 实现里定义。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent_core::mcp::types::ToolResult;
use serde_json::Value;

use crate::chat::storage::StoreMessage;

use super::SubAgentCallContext;

/// 子 agent 结果处理决策。
///
/// 回调返回此枚举，决定子 agent 最终 tool result 的内容。
/// 决策结果会流入：父 agent 上下文（history）→ 父 agent 后续 LLM 调用 → GUI `ToolExecuted.content` → 持久化。
pub enum ResultDecision {
    /// 接受结果，原样返回。
    ///
    /// 父 agent 看到的是**当前对外结果**（通常是子 agent 原始输出，或 prelude 定稿的规范化
    /// 文本；若链上更早有回调 [`Transform`](Self::Transform) 过，则为那份文本）。
    Accept,
    /// 处理后的结果：用 `new_text` 替换对外 content，并**终止链**。
    ///
    /// 原始文本被丢弃，父 agent 及所有下游看到的都是你提供的 `new_text`。
    /// 典型场景：从子 agent 大段 Markdown 中提取 JSON 块、清理格式、摘要等。
    Transform(String),
    /// 拒绝结果，发送纠正消息给子 agent，要求重新生成。
    ///
    /// - `String` 作为新的 user 消息发送给子 agent（如「输出格式错误，请严格输出 JSON」）
    /// - 子 agent 重新生成后，链会被**从头再走一遍**（prelude 也会重跑），最多重试 2 次
    /// - 重试耗尽或子 agent 失败时，自动兜底使用原始结果
    Retry(String),
    /// 中断本次子 agent 调用：**不再重试**，直接把 `String` 作为失败原因返回。
    ///
    /// 用于回调侧发生**不可重试的硬错误**（如流程状态写库失败、外部依赖不可用）——
    /// 重试也修不好，而静默 [`Accept`](Self::Accept) 又会让上层误以为成功。
    /// 返回后本次调用的 tool result 标记为 `is_error = true`、内容为 `String`，
    /// 子 agent 不会重新生成。
    ///
    /// 与 [`Retry`](Self::Retry) 的分工：`Retry` 是「模型输出不对，让它重做」；
    /// `Abort` 是「模型没错，但外部动作失败了，立刻收场并如实上报」。
    Abort(String),
    /// 交给链上的**下一个**回调继续处理。
    ///
    /// `String` 成为链内传递值：下游回调在 [`SubAgentCall::result`] 里看到它，但**它不改变
    /// 对外结果** —— 父 agent / GUI / 持久化看到的仍是当前对外结果（原始输出，或 prelude
    /// 定稿的文本）。
    ///
    /// 与 [`Transform`](Self::Transform) 的分工：`Next` 是「我处理完了，交给下一位」，
    /// 不影响父 agent；`Transform` 是「最终对外就用我这份」，但它终止链。
    ///
    /// 链上**最后一个**回调返回 `Next` 是**合法**写法（「我这一环不需要改对外结果」），
    /// 此时值无人消费，对外结果保持不变（等价于 [`Accept`](Self::Accept)）；框架只记一条
    /// debug 日志。
    Next(String),
}

/// 一次回调调用的全部入参。
///
/// 聚成一个结构体而不是摊成多个参数：链上回调日后要看的输入只会更多（历史、前置分析、
/// 链内传值……），聚合后扩展不必再改 trait 签名。
pub struct SubAgentCall<'a> {
    /// 本次调用的上下文（工具名 / tool_call_id / 父 agent 传入的原始参数）。
    pub ctx: &'a SubAgentCallContext,
    /// 链内传递值：链首为 `extract_last_assistant_text` 的文本，之后为上一个回调
    /// [`Next`](ResultDecision::Next) 的产物。**不一定**是对外结果。
    pub result: &'a ToolResult,
    /// 本子 agent 到目前为止的**完整对话历史**（本轮迭代开始时的快照，重试后刷新）。
    ///
    /// 用 [`export_tool_trace`](crate::chat::trace::export_tool_trace) 可从它导出**真实**
    /// 的工具执行轨迹 —— 轨迹类产物的唯一可信来源，不要改用模型自述。
    pub history: &'a [StoreMessage],
    /// 前置分析（[`SubAgentChainPrelude`]）的产物。结构由注册方定义，核心库只搬运。
    ///
    /// 未挂 prelude 时为 [`Value::Null`]。
    pub analysis: &'a Value,
    /// 本回调是否是链上**最后一个**（后面没有别的回调）。
    ///
    /// 框架提供它，回调便不必关心自己在链上的位置：链尾返回
    /// [`Next`](ResultDecision::Next) 只是把值交给不存在的下一位（等价 `Accept`），
    /// 故典型写法是「链尾 `Accept`、非链尾 `Next`」。
    pub is_last: bool,
}

impl SubAgentCall<'_> {
    /// 链内文本：`result.content` 是字符串时取原文，否则返回空串。
    ///
    /// 子 agent 的正常输出路径总是字符串（`extract_last_assistant_text`），所以这是取
    /// 「模型这一步说了什么」的常态入口。
    pub fn text(&self) -> &str {
        self.result.content.as_str().unwrap_or("")
    }
}

/// 前置分析的产出。
pub enum PreludeOutcome {
    /// 正常：链照常跑，并把产物交给每个回调。
    Proceed {
        /// 分析产物（交给 [`SubAgentCall::analysis`]）。结构由实现方定义。
        analysis: Value,
        /// 若为 `Some`，即**定稿对外结果**：链上没有任何回调 [`Transform`](ResultDecision::Transform)
        /// 时，父 agent / 持久化看到的就是这份文本（典型用途：去 markdown 围栏、紧凑 JSON、
        /// 路径规范化）。链上更早的 `Transform` 会覆盖它。
        outer: Option<String>,
    },
    /// 直接收场：**链不跑**（回调一个都不执行）。
    ///
    /// 用途：解析失败 → `Retry`；非定稿 / 会话归属缺失 → `Accept`（「宁可『不写』也不写错」）。
    /// [`Next`](ResultDecision::Next) 放这里没有意义（没有下游消费者），框架会记 warn 并按
    /// `Accept` 处理。
    Stop(ResultDecision),
}

/// 回调链的**前置分析**：在回调链运行前跑一次。
///
/// 核心库只保证「先跑它、产物交给链上每个回调、可定稿对外文本」，**不理解分析规则** ——
/// 具体解析什么、判定什么由实现方定义（如「输出必须是带 `status` 的 JSON」「必须带会话 id」）。
///
/// 重试（[`ResultDecision::Retry`]）后子 agent 会重新生成，链从头再走，**本分析也会重跑**：
/// 实现方可以直接依赖「本次分析的输入就是当前输出」。
///
/// 建议：把「解析失败要重试、非定稿不写库」这类**共同守门**放在这里，一处实现、各 step 共享；
/// 链上的业务回调便只需假设自己拿到的分析结果一定是「可用的定稿」。
#[async_trait]
pub trait SubAgentChainPrelude: Send + Sync {
    /// 前置分析：在回调链运行前调用一次。
    ///
    /// - `ctx`：本次调用的上下文
    /// - `result`：**链首**的链内传值（即子 agent 的原始输出文本）
    /// - `history`：本子 agent 到目前为止的完整对话历史
    async fn analyze(
        &self,
        ctx: &SubAgentCallContext,
        result: &ToolResult,
        history: &[StoreMessage],
    ) -> PreludeOutcome;

    /// 分析器名，仅用于日志。
    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }
}

/// 子 agent 结果回调：完成后可获取最终 tool result（用于外部解析/提取）。
///
/// 多个实现可**串行**挂在同一次调用上：每个回调按顺序跑，只有
/// [`Next`](ResultDecision::Next) 会把链交给下一个，其余决策一律终止链。链的执行细节
/// （两个传值通道、终止规则、对外结果的定稿）见 `collect::run_chain`。
///
/// `on_result` 为 **async**：它在触发方的异步上下文（`collect_until_outcome`）中被
/// `await`，回调实现因此可以直接 `await` 持久化等异步副作用，无需自行 `tokio::spawn`
/// （这是之前同步签名的权宜）。通过 [`async_trait`] 实现，返回的 future 需 `Send`。
#[async_trait]
pub trait SubAgentResultCallback: Send + Sync {
    /// 子 agent 完成后触发。
    ///
    /// 入参见 [`SubAgentCall`]（上下文 / 链内传值 / 历史 / 前置分析）。
    ///
    /// 返回 [`ResultDecision`]：
    /// - `Accept`：接受结果（终止链，对外保持当前对外结果）
    /// - `Transform(text)`：替换对外 content（终止链）
    /// - `Next(text)`：把 `text` 传给下一个回调（末位则等价 `Accept`）
    /// - `Retry(msg)`：发送纠正消息给子 agent，重试后链从头再走一遍
    /// - `Abort(reason)`：中断本次调用，`reason` 作为失败结果返回（不重试）
    async fn on_result(&self, call: &SubAgentCall<'_>) -> ResultDecision;

    /// 回调名，仅用于日志。
    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }
}

/// 结果链：一个可选的前置分析 + 一串串行业务回调。
///
/// 注册给子 agent 的**唯一**结果侧入口（取代裸 `Vec<Arc<dyn SubAgentResultCallback>>`）：
/// 让「前置分析」成为注册框架的一部分，而不是每个注册点各自记得挂。
#[derive(Default, Clone)]
pub struct SubAgentResultChain {
    prelude: Option<Arc<dyn SubAgentChainPrelude>>,
    callbacks: Vec<Arc<dyn SubAgentResultCallback>>,
}

impl SubAgentResultChain {
    /// 只有回调（无前置分析）的链。
    pub fn new(callbacks: Vec<Arc<dyn SubAgentResultCallback>>) -> Self {
        Self {
            prelude: None,
            callbacks,
        }
    }

    /// 挂上前置分析。
    pub fn with_prelude(mut self, prelude: Arc<dyn SubAgentChainPrelude>) -> Self {
        self.prelude = Some(prelude);
        self
    }

    /// 挂上前置分析（可变版，便于先建链后补）。
    pub fn set_prelude(&mut self, prelude: Arc<dyn SubAgentChainPrelude>) {
        self.prelude = Some(prelude);
    }

    /// **完全空**链（既无前置分析也无回调）：调用方可跳过整段结果处理逻辑。
    ///
    /// 注意「有前置分析、无回调」不算空 —— 分析本身可能就要跑（并定稿对外文本）。
    pub fn is_empty(&self) -> bool {
        self.prelude.is_none() && self.callbacks.is_empty()
    }

    /// 前置分析（若有）。
    pub fn prelude(&self) -> Option<&Arc<dyn SubAgentChainPrelude>> {
        self.prelude.as_ref()
    }

    /// 业务回调序列。
    pub fn callbacks(&self) -> &[Arc<dyn SubAgentResultCallback>] {
        &self.callbacks
    }
}

impl From<Vec<Arc<dyn SubAgentResultCallback>>> for SubAgentResultChain {
    fn from(callbacks: Vec<Arc<dyn SubAgentResultCallback>>) -> Self {
        Self::new(callbacks)
    }
}
