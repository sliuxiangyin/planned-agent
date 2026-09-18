//! 事件收集 + 回调决策 + 重试循环。

use std::sync::{Arc, Mutex};

use anyhow::Result;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_core::events::{ChatEvent as CoreChatEvent, UIQuestion};
use planned_agent_core::mcp::types::ToolResult;
use planned_agent_prompt_manager::FilePromptManager;
use planned_agent_tool_manager::{
    SubAgentRunOutcome, ToolStreamSender,
};
use serde_json::Value;
use tracing::{error, info, warn};

use crate::chat::service::{ChatEvent, ChatService, SendOutcome, SendTicket};
use crate::chat::storage::StoreMessage;

use super::callback::{
    PreludeOutcome, ResultDecision, SubAgentCall, SubAgentCallContext, SubAgentResultChain,
};
use super::session::ChatSubAgentSession;

/// 监听子 agent 的事件流，转发到 `ToolStreamSender`，
/// 直到对话完成（`Completed`）或挂起（`Suspended`）。
///
/// `Completed` 分支先跑**前置分析**（`SubAgentChainPrelude`：解析输出、判定定稿、定稿
/// 对外文本，或直接收场），再串行跑业务回调，按 [`ResultDecision`] 决定：
/// - `Accept`：接受结果
/// - `Transform`：替换对外 content
/// - `Next`：把值交给链上下一个回调（链尾时不改变对外结果）
/// - `Retry`：重发纠正消息给子 agent（最多重试 2 次，重试后链**从头再走**，前置分析也会重跑）
/// - `Abort`：不重试，直接以失败结果收场（内容为失败原因）
pub(super) async fn collect_until_outcome(
    service: &ChatService<FilePromptManager>,
    ticket: SendTicket,
    stream: &ToolStreamSender,
    depth: u32,
    max_depth: u32,
    result_chain: SubAgentResultChain,
    // 父 agent 传给该子 agent 的原始参数；仅用于填充回调的 `SubAgentCallContext`，
    // 或随挂起会话保留到 resume（见 `ChatSubAgentSession`）。
    arguments: Value,
) -> Result<SubAgentRunOutcome> {
    info!("[子agent] collect_until_outcome 开始，注册事件监听");

    // 克隆 stream 以便传入闭包（闭包需要 'static）
    let stream_clone = stream.clone();

    // 捕获挂起时 UIActionRequest 的 message / questions（用于构造 AwaitingUserAction）
    let ui_request: Arc<Mutex<Option<(String, Vec<UIQuestion>)>>> = Arc::new(Mutex::new(None));
    let ui_request_clone = ui_request.clone();

    // 注册临时事件监听：转发子 agent 内部事件，并捕获挂起 UI 信息。
    //
    // 转发规则：
    // - `UIActionRequest` → 直接转发为 `Chat(CoreChatEvent)`，主 agent GUI 需要
    //   直接处理交互卡片（弹出 PendingUI）。
    // - `RoundStart` / `RoundEnd` → 不转发，避免主 agent 创建多余气泡。
    // - 其余（TextDelta / ReasoningDelta / ToolCall* / ToolExecuted）→
    //   包装为 `SubChatEvent`，通过 `SubChat` 通道转发，GUI 按 `tool_call_id`
    //   路由到对应 `AgentView`。
    let tool_call_id_for_closure = stream.invocation_id().to_string();
    let _guard = service.on_chat_with_guard(move |event| {
        if let ChatEvent::Chat(chat_event) = &event {
            // 捕获 UIActionRequest 的 message / questions
            if let CoreChatEvent::UIActionRequest {
                message, questions, ..
            } = chat_event
            {
                *ui_request_clone.lock().unwrap() = Some((message.clone(), questions.clone()));
            }

            match chat_event {
                // UIActionRequest → 直接转发（主 agent GUI 处理交互卡片）
                CoreChatEvent::UIActionRequest { .. } => {
                    stream_clone.emit_event_sync(chat_event.clone());
                }
                // 其余 → 包装为 SubChat
                _ => {
                    stream_clone.emit_event_sync(CoreChatEvent::SubChat {
                        tool_call_id: tool_call_id_for_closure.clone(),
                        event: Box::new(chat_event.clone()),
                    });
                }
            }
        }
    });

    // 等待子 agent 对话结果（区分完成 / 挂起 / 失败）
    match ticket.wait_outcome().await {
        SendOutcome::Completed => {
            let mut last_text = extract_last_assistant_text(&service.history());
            info!("[子agent] 子 agent 完成，提取结果：{}", last_text);

            // ── 回调决策 + 重试循环 ──
            let max_retries = 2;
            let final_text = if result_chain.is_empty() {
                // 空链（既无前置分析也无回调）：直接用原始结果
                last_text
            } else {
                // 本次调用上下文：核心库只透传参数，具体取哪个字段由回调决定。
                // 先 clone 一份，因为 `arguments` 在后面 Suspended 分支要 move 给挂起会话。
                let call_ctx = SubAgentCallContext {
                    agent_name: stream.tool_name().to_string(),
                    tool_call_id: stream.invocation_id().to_string(),
                    arguments: arguments.clone(),
                };
                // 重试计数与链一起走：同一次逻辑调用内，链上的任何回调要求重试
                // 都消耗同一个额。重试后子 agent 重跑 → 链**从头再走一遍**。
                let mut attempts = 0u32;
                loop {
                    // 每轮重新取历史：重试会跑出新的工具调用，轨迹必须跟着刷新。
                    let history = service.history_store();
                    match run_chain(&result_chain, &call_ctx, &last_text, &history).await {
                        ChainOutcome::Done(text) => break text,
                        ChainOutcome::Abort(reason) => {
                            // 外部动作失败（如流程状态写库）：重试同样会失败，立即以失败结果收场，
                            // 让父 agent 看到 is_error 的 tool result 并知道原因。
                            error!("[子agent] 回调链要求中断本次调用：{}", reason);
                            return Ok(SubAgentRunOutcome::Done(ToolResult {
                                call_id: String::new(),
                                is_error: true,
                                content: Value::String(reason),
                            }));
                        }
                        ChainOutcome::Retry(msg) if attempts < max_retries => {
                            attempts += 1;
                            info!(
                                "[子agent] 回调链要求重试 ({}/{}), 发送纠正消息",
                                attempts, max_retries
                            );
                            match service.send_text(msg) {
                                Ok(retry_ticket) => match retry_ticket.wait_outcome().await {
                                    SendOutcome::Completed => {
                                        last_text = extract_last_assistant_text(&service.history());
                                        info!("[子agent] 重试完成，新结果：{}", last_text);
                                        continue;
                                    }
                                    other => {
                                        info!("[子agent] 重试未正常完成: {:?}，使用原始结果", other);
                                        break last_text;
                                    }
                                },
                                Err(e) => {
                                    info!("[子agent] 重试发送失败: {}，使用原始结果", e);
                                    break last_text;
                                }
                            }
                        }
                        ChainOutcome::Retry(_) => {
                            info!("[子agent] 重试次数耗尽，使用原始结果");
                            break last_text;
                        }
                    }
                }
            };

            let result = ToolResult {
                call_id: String::new(),
                is_error: false,
                content: Value::String(final_text),
            };
            Ok(SubAgentRunOutcome::Done(result))
        }
        SendOutcome::Suspended { .. } => {
            info!("[子agent] 子 agent 挂起，构造 AwaitingUserAction");
            let (message, questions) = ui_request.lock().unwrap().take().unwrap_or_default();
            Ok(SubAgentRunOutcome::AwaitingUserAction {
                session: Box::new(ChatSubAgentSession::new(
                    service.clone(),
                    depth,
                    max_depth,
                    result_chain.clone(),
                    arguments,
                )),
                message,
                actions: serde_json::to_value(questions).unwrap_or_else(|_| Value::Array(vec![])),
            })
        }
        SendOutcome::Failed(e) => {
            info!("[子agent] 子 agent 失败: {}", e);
            Ok(SubAgentRunOutcome::Done(ToolResult {
                call_id: String::new(),
                is_error: true,
                content: Value::String(format!("子 agent 执行失败: {}", e)),
            }))
        }
    }
}

/// 从历史中提取最后一条 assistant 文本消息
fn extract_last_assistant_text(history: &[Message]) -> String {
    for msg in history.iter().rev() {
        if matches!(msg.role, MessageRole::Assistant) {
            if let Some(MessageContent::Text { text }) = &msg.content {
                if !text.is_empty() {
                    return text.clone();
                }
            }
        }
    }
    String::new()
}

/// 回调链的执行去向。
enum ChainOutcome {
    /// 正常收场：对外内容为 `String`。
    Done(String),
    /// 请求重试：`String` 是发给子 agent 的纠正消息。
    Retry(String),
    /// 中断本次调用：`String` 是失败原因（对外 `is_error = true`）。
    Abort(String),
}

/// 跑完结果链（前置分析 → 业务回调），返回最终去向。
///
/// # 执行顺序
///
/// 1. **前置分析**（[`SubAgentChainPrelude`]，已挂时）：跑一次，产物交给链上每个回调，
///    并可**定稿对外文本**（`outer`），或直接收场（`Stop`，链一个回调都不跑）。
/// 2. **业务回调**：按注册顺序串行。
///
/// # 两个独立的传值通道
///
/// - `pipeline_value`：**链内**传递值，初始为 `last_text`，只由 [`Next`] 改写。
///   下游回调在 `result.content` 里看到它 —— 所以链是「处理管道」，而不是每个回调
///   都复核同一份原文。
/// - `final_content`：**对外**结果，初始为 `last_text`，可被前置分析的 `outer` 定稿
///   （去 markdown 围栏、紧凑 JSON、路径规范化这类「出口清洗」），此后只有
///   [`Transform`] 能改写它 —— 而它走的是直接 return 的分支。因此「链上没有任何回调
///   换掉对外结果」时，结果就是 prelude 定稿的那份（没挂 prelude 则为原文）：这正是
///   `Accept`、空链、末位 `Next` 三者对外表现一致的原因。
///
/// 这条分工让回调既能「只做副作用、不改对外呈现」（返回 `Next` / `Accept`），
/// 也能「最终对外就用我这份」（返回 `Transform`）。
///
/// # 终止规则
///
/// 只有 `Next` 续链；`Accept` / `Transform` / `Retry` / `Abort` 一律终止链（**后面的
/// 回调不再执行**）。链走到底（含空链）时对外结果保持原样 —— 所以末位 `Next` 与
/// `Accept` 效果相同，仅多记一条 warn 提醒「没人在消费这个值」。
///
/// [`Next`]: ResultDecision::Next
/// [`Transform`]: ResultDecision::Transform
async fn run_chain(
    chain: &SubAgentResultChain,
    ctx: &SubAgentCallContext,
    last_text: &str,
    history: &[StoreMessage],
) -> ChainOutcome {
    // 链内传递值：只被 `Next` 改写，下游回调看到的就是它。
    let mut pipeline_value = last_text.to_string();
    // 对外结果：初始为原文，等待前置分析定稿（出口清洗）。
    let mut final_content = last_text.to_string();
    // 前置分析产物：未挂 prelude 时为 Null —— 回调可以安全读它，不必自己判空。
    let mut analysis = Value::Null;

    // ── 前置分析：链跑之前一次 ──
    if let Some(prelude) = chain.prelude() {
        let probe = chain_probe(&pipeline_value);
        match prelude.analyze(ctx, &probe, history).await {
            PreludeOutcome::Proceed {
                analysis: produced,
                outer,
            } => {
                analysis = produced;
                if let Some(text) = outer {
                    final_content = text;
                }
            }
            PreludeOutcome::Stop(decision) => {
                info!(
                    "[子agent] 前置分析 {} 要求直接收场，链不执行",
                    prelude.name()
                );
                return stop_outcome(decision, final_content, prelude.name());
            }
        }
    }

    let callbacks = chain.callbacks();
    for (i, cb) in callbacks.iter().enumerate() {
        // 下游回调看到的是链内传递值（上一个 `Next` 的产物），不是原始 last_text。
        let probe = chain_probe(&pipeline_value);
        let call = SubAgentCall {
            ctx,
            result: &probe,
            history,
            analysis: &analysis,
            is_last: i + 1 == callbacks.len(),
        };
        match cb.on_result(&call).await {
            ResultDecision::Accept => return ChainOutcome::Done(final_content),
            ResultDecision::Transform(new) => return ChainOutcome::Done(new),
            ResultDecision::Next(next) => {
                if call.is_last {
                    warn!(
                        "[子agent] 回调链末位的 {} 返回 Next，但无后续消费者；对外结果保持不变",
                        cb.name()
                    );
                }
                pipeline_value = next;
            }
            ResultDecision::Retry(msg) => return ChainOutcome::Retry(msg),
            ResultDecision::Abort(reason) => return ChainOutcome::Abort(reason),
        }
    }

    // 空链，或整条链都以 Next 收尾：对外结果保持原样（= prelude 定稿的那份或原文）。
    ChainOutcome::Done(final_content)
}

/// 把前置分析 `Stop` 携带的决策翻译成链的收场方式。
///
/// `Stop` 的语义是「链不跑」，所以只有终止型决策有意义；若给了 [`Next`]，链上并没有
/// 「下一位」可以交给（回调一个都不会执行），故记 warn 并按 `Accept` 处理。
///
/// [`Next`]: ResultDecision::Next
fn stop_outcome(decision: ResultDecision, outer: String, prelude_name: &str) -> ChainOutcome {
    match decision {
        ResultDecision::Accept => ChainOutcome::Done(outer),
        ResultDecision::Transform(new) => ChainOutcome::Done(new),
        ResultDecision::Retry(msg) => ChainOutcome::Retry(msg),
        ResultDecision::Abort(reason) => ChainOutcome::Abort(reason),
        ResultDecision::Next(_) => {
            warn!(
                "[子agent] 前置分析 {} 返回 Stop(Next)，但链上没有下一位可交给；按 Accept 处理",
                prelude_name
            );
            ChainOutcome::Done(outer)
        }
    }
}

/// 构造链内传值的载体：链首为子 agent 原始输出，之后为上一个 `Next` 的产物。
fn chain_probe(text: &str) -> ToolResult {
    ToolResult {
        call_id: String::new(),
        is_error: false,
        content: Value::String(text.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // 这两个 trait 只在测试里用来造回调 / 分析器（生产路径不需要在本模块可见）。
    use crate::chat::{SubAgentChainPrelude, SubAgentResultCallback};

    /// 预设行为脚本（`ResultDecision` 未实现 `Clone`，故用可复制的脚本来描述）。
    #[derive(Clone, Copy)]
    enum Script {
        Accept,
        Transform(&'static str),
        Next(&'static str),
        Retry(&'static str),
        Abort(&'static str),
    }

    /// 记录它看到的 `result.content`，并按脚本返回决策。
    struct Recorder {
        label: &'static str,
        script: Script,
        seen: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl SubAgentResultCallback for Recorder {
        async fn on_result(&self, call: &SubAgentCall<'_>) -> ResultDecision {
            let content = match &call.result.content {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            self.seen.lock().unwrap().push(content);
            match self.script {
                Script::Accept => ResultDecision::Accept,
                Script::Transform(s) => ResultDecision::Transform(s.to_string()),
                Script::Next(s) => ResultDecision::Next(s.to_string()),
                Script::Retry(s) => ResultDecision::Retry(s.to_string()),
                Script::Abort(s) => ResultDecision::Abort(s.to_string()),
            }
        }

        fn name(&self) -> &str {
            self.label
        }
    }

    fn ctx() -> SubAgentCallContext {
        SubAgentCallContext {
            agent_name: "test".to_string(),
            tool_call_id: "tc-1".to_string(),
            arguments: Value::Null,
        }
    }

    /// 构造链，并返回「各回调依次看到的 content」记录。
    fn chain(
        scripts: Vec<(&'static str, Script)>,
    ) -> (SubAgentResultChain, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let cbs = scripts
            .into_iter()
            .map(|(label, script)| {
                Arc::new(Recorder {
                    label,
                    script,
                    seen: seen.clone(),
                }) as Arc<dyn SubAgentResultCallback>
            })
            .collect();
        (SubAgentResultChain::new(cbs), seen)
    }

    #[tokio::test]
    async fn empty_chain_keeps_last_text() {
        let out = run_chain(&SubAgentResultChain::default(), &ctx(), "原始", &[]).await;
        assert!(matches!(out, ChainOutcome::Done(t) if t == "原始"));
    }

    #[tokio::test]
    async fn single_accept_keeps_last_text() {
        let (cbs, _) = chain(vec![("a", Script::Accept)]);
        let out = run_chain(&cbs, &ctx(), "原始", &[]).await;
        assert!(matches!(out, ChainOutcome::Done(t) if t == "原始"));
    }

    #[tokio::test]
    async fn single_transform_replaces_outer_result() {
        let (cbs, _) = chain(vec![("a", Script::Transform("改写"))]);
        let out = run_chain(&cbs, &ctx(), "原始", &[]).await;
        assert!(matches!(out, ChainOutcome::Done(t) if t == "改写"));
    }

    #[tokio::test]
    async fn next_passes_value_downstream_without_touching_outer_result() {
        let (cbs, seen) = chain(vec![("a", Script::Next("A的产物")), ("b", Script::Accept)]);
        let out = run_chain(&cbs, &ctx(), "原始", &[]).await;
        // 对外结果不受 Next 影响
        assert!(matches!(out, ChainOutcome::Done(t) if t == "原始"));
        // 链首看到原文，第二个看到 A 的产物
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["原始".to_string(), "A的产物".to_string()]
        );
    }

    #[tokio::test]
    async fn accept_terminates_chain() {
        let (cbs, seen) = chain(vec![
            ("a", Script::Accept),
            ("b", Script::Transform("不该被执行")),
        ]);
        let out = run_chain(&cbs, &ctx(), "原始", &[]).await;
        assert!(matches!(out, ChainOutcome::Done(t) if t == "原始"));
        assert_eq!(seen.lock().unwrap().len(), 1, "Accept 之后不应再执行后续回调");
    }

    #[tokio::test]
    async fn trailing_next_keeps_outer_result() {
        let (cbs, seen) = chain(vec![("a", Script::Next("产物"))]);
        let out = run_chain(&cbs, &ctx(), "原始", &[]).await;
        assert!(matches!(out, ChainOutcome::Done(t) if t == "原始"));
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn retry_terminates_chain_with_message() {
        let (cbs, seen) = chain(vec![("a", Script::Retry("请重做")), ("b", Script::Accept)]);
        let out = run_chain(&cbs, &ctx(), "原始", &[]).await;
        assert!(matches!(out, ChainOutcome::Retry(m) if m == "请重做"));
        assert_eq!(seen.lock().unwrap().len(), 1, "Retry 之后不应再执行后续回调");
    }

    #[tokio::test]
    async fn abort_terminates_chain_with_reason() {
        let (cbs, seen) = chain(vec![("a", Script::Abort("写库失败")), ("b", Script::Accept)]);
        let out = run_chain(&cbs, &ctx(), "原始", &[]).await;
        assert!(matches!(out, ChainOutcome::Abort(m) if m == "写库失败"));
        assert_eq!(seen.lock().unwrap().len(), 1, "Abort 之后不应再执行后续回调");
    }

    // ── 前置分析（SubAgentChainPrelude）──

    /// 记录回调看到的 `analysis` / `is_last`，并按脚本决策。
    struct Spy {
        label: &'static str,
        script: Script,
        seen: Arc<Mutex<Vec<String>>>,
        seen_analysis: Arc<Mutex<Vec<Value>>>,
        seen_last: Arc<Mutex<Vec<bool>>>,
    }

    #[async_trait::async_trait]
    impl SubAgentResultCallback for Spy {
        async fn on_result(&self, call: &SubAgentCall<'_>) -> ResultDecision {
            self.seen_analysis
                .lock()
                .unwrap()
                .push(call.analysis.clone());
            self.seen_last.lock().unwrap().push(call.is_last);
            self.seen
                .lock()
                .unwrap()
                .push(call.result.content.as_str().unwrap_or("").to_string());
            match self.script {
                Script::Accept => ResultDecision::Accept,
                Script::Transform(s) => ResultDecision::Transform(s.to_string()),
                Script::Next(s) => ResultDecision::Next(s.to_string()),
                Script::Retry(s) => ResultDecision::Retry(s.to_string()),
                Script::Abort(s) => ResultDecision::Abort(s.to_string()),
            }
        }

        fn name(&self) -> &str {
            self.label
        }
    }

    /// Spy 链的观察记录。
    struct SpyChain {
        chain: SubAgentResultChain,
        seen: Arc<Mutex<Vec<String>>>,
        analyses: Arc<Mutex<Vec<Value>>>,
        last_flags: Arc<Mutex<Vec<bool>>>,
        runs: Arc<Mutex<u32>>,
    }

    /// 固定产出的前置分析：给什么就产出什么；`stop` 非空时直接收场。
    impl SpyChain {
        fn new(scripts: &[Script], prelude: Option<(Value, Option<&str>, Option<Script>)>) -> Self {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let seen_analysis = Arc::new(Mutex::new(Vec::new()));
            let seen_last = Arc::new(Mutex::new(Vec::new()));
            let cbs: Vec<Arc<dyn SubAgentResultCallback>> = scripts
                .iter()
                .map(|script| {
                    Arc::new(Spy {
                        label: "spy",
                        script: *script,
                        seen: seen.clone(),
                        seen_analysis: seen_analysis.clone(),
                        seen_last: seen_last.clone(),
                    }) as Arc<dyn SubAgentResultCallback>
                })
                .collect();
            let runs = Arc::new(Mutex::new(0u32));
            let mut chain = SubAgentResultChain::new(cbs);
            if let Some((analysis, outer, stop)) = prelude {
                chain.set_prelude(Arc::new(FixedPrelude {
                    analysis,
                    outer: outer.map(str::to_string),
                    stop,
                    runs: runs.clone(),
                }));
            }
            Self {
                chain,
                seen,
                analyses: seen_analysis,
                last_flags: seen_last,
                runs,
            }
        }
    }

    /// 固定产出的前置分析实现。
    struct FixedPrelude {
        analysis: Value,
        outer: Option<String>,
        stop: Option<Script>,
        runs: Arc<Mutex<u32>>,
    }

    #[async_trait::async_trait]
    impl SubAgentChainPrelude for FixedPrelude {
        async fn analyze(
            &self,
            _ctx: &SubAgentCallContext,
            result: &ToolResult,
            _history: &[StoreMessage],
        ) -> PreludeOutcome {
            *self.runs.lock().unwrap() += 1;
            // 链首交给分析器的永远是子 agent 的原始输出（文本）。
            assert!(
                result.content.as_str().is_some(),
                "前置分析应收到链首的文本"
            );
            if let Some(stop) = self.stop {
                return PreludeOutcome::Stop(match stop {
                    Script::Accept => ResultDecision::Accept,
                    Script::Transform(s) => ResultDecision::Transform(s.to_string()),
                    Script::Next(s) => ResultDecision::Next(s.to_string()),
                    Script::Retry(s) => ResultDecision::Retry(s.to_string()),
                    Script::Abort(s) => ResultDecision::Abort(s.to_string()),
                });
            }
            PreludeOutcome::Proceed {
                analysis: self.analysis.clone(),
                outer: self.outer.clone(),
            }
        }

        fn name(&self) -> &str {
            "fixed-prelude"
        }
    }

    #[tokio::test]
    async fn prelude_outer_defines_outer_result_while_callbacks_see_pipeline() {
        // step2 形态：prelude 定稿对外文本（出口清洗），业务回调只做副作用并续链。
        let h = SpyChain::new(
            &[Script::Next("下一位"), Script::Accept],
            Some((serde_json::json!({ "status": "success" }), Some("紧凑JSON"), None)),
        );
        let out = run_chain(&h.chain, &ctx(), "原始带围栏", &[]).await;
        // 对外 = prelude 定稿的那份，而不是原文
        assert!(matches!(out, ChainOutcome::Done(t) if t == "紧凑JSON"));
        // 链内传值不受影响：第三个回调看到上一个 Next 的产物
        assert_eq!(
            *h.seen.lock().unwrap(),
            vec!["原始带围栏".to_string(), "下一位".to_string()]
        );
        // 每个回调都拿到同一份前置分析产物
        assert_eq!(*h.analyses.lock().unwrap(), vec![serde_json::json!({ "status": "success" }); 2]);
        // 框架正确标注链尾
        assert_eq!(*h.last_flags.lock().unwrap(), vec![false, true]);
        assert_eq!(*h.runs.lock().unwrap(), 1, "前置分析只跑一次");
    }

    #[tokio::test]
    async fn transform_overrides_prelude_outer() {
        let h = SpyChain::new(&[Script::Transform("回调改写")], Some((Value::Null, Some("prelude定稿"), None)));
        let out = run_chain(&h.chain, &ctx(), "原始", &[]).await;
        assert!(matches!(out, ChainOutcome::Done(t) if t == "回调改写"));
    }

    #[tokio::test]
    async fn prelude_stop_accept_skips_all_callbacks() {
        let h = SpyChain::new(&[Script::Transform("不该被执行")], Some((Value::Null, None, Some(Script::Accept))));
        let out = run_chain(&h.chain, &ctx(), "原始", &[]).await;
        assert!(matches!(out, ChainOutcome::Done(t) if t == "原始"));
        assert!(h.seen.lock().unwrap().is_empty(), "Stop 之后回调不应执行");
    }

    #[tokio::test]
    async fn prelude_stop_retry_and_transform() {
        let retry = SpyChain::new(&[Script::Accept], Some((Value::Null, None, Some(Script::Retry("请重做")))));
        let out = run_chain(&retry.chain, &ctx(), "原始", &[]).await;
        assert!(matches!(out, ChainOutcome::Retry(m) if m == "请重做"));

        let transform = SpyChain::new(&[Script::Accept], Some((Value::Null, None, Some(Script::Transform("分析器改写")))));
        let out = run_chain(&transform.chain, &ctx(), "原始", &[]).await;
        assert!(matches!(out, ChainOutcome::Done(t) if t == "分析器改写"));
    }

    #[tokio::test]
    async fn prelude_stop_next_is_treated_as_accept() {
        // `Stop(Next)` 没有下一位可交给：按 Accept 处理，且**不做定稿**（定稿只走 Proceed）。
        let h = SpyChain::new(&[], Some((Value::Null, None, Some(Script::Next("无人消费")))));
        let out = run_chain(&h.chain, &ctx(), "原始", &[]).await;
        assert!(matches!(out, ChainOutcome::Done(t) if t == "原始"));
    }

    #[tokio::test]
    async fn callbacks_see_null_analysis_without_prelude() {
        let h = SpyChain::new(&[Script::Accept], None);
        let _ = run_chain(&h.chain, &ctx(), "原始", &[]).await;
        assert_eq!(*h.analyses.lock().unwrap(), vec![Value::Null]);
    }
}
