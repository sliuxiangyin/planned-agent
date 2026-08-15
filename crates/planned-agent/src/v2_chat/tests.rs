//! v2 聊天服务的集成测试。
//!
//! 完整对话流程的端到端测试，覆盖：单轮文本、UI 确认闭环、串行队列、取消、
//! 错误处理、panic 隔离、history 回滚等。

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;

    use anyhow::{anyhow, Result};
    use async_trait::async_trait;
    use planned_agent_core::ai::types::{
        ChatCompletionChunk, ChatCompletionRequest, ChunkChoice, DeltaFunctionCall, DeltaMessage,
        DeltaToolCall, Message, MessageContent, MessageRole, ToolType,
    };
    use planned_agent_core::ai::ChatCompletionStream;
    use planned_agent_core::events::ChatEvent;
    use planned_agent_core::prompt::{PromptContext, PromptInfo, PromptManager, PromptTemplate};
    use planned_agent_tool_manager::ToolRegistry;
    use serde_json::{json, Value};
    use tokio::sync::mpsc;

    use crate::v2_chat::service::SubscriptionGuard;
    use crate::v2_chat::{V2ChatConfig, V2ChatEvent, V2ChatService};

    /// Mock PromptManager：仅 `render` 返回固定文本。
    struct MockPromptManager;

    #[async_trait]
    impl PromptManager for MockPromptManager {
        async fn load_template(&self, _name: &str) -> Result<PromptTemplate> {
            unimplemented!()
        }
        async fn render(&self, name: &str, _ctx: &PromptContext) -> Result<String> {
            // 按模板名返回不同内容，便于测试验证模板热切换
            Ok(format!("system:{}", name))
        }
        async fn list_prompts(&self) -> Result<Vec<PromptInfo>> {
            Ok(vec![])
        }
        async fn exists(&self, _name: &str) -> Result<bool> {
            Ok(true)
        }
        async fn reload(&self) -> Result<()> {
            Ok(())
        }
        async fn get_output_schema(&self, _name: &str) -> Result<Option<Value>> {
            Ok(None)
        }
        async fn parse_response<T: serde::de::DeserializeOwned>(
            &self,
            _name: &str,
            _response: &str,
        ) -> Result<T> {
            unimplemented!()
        }
        async fn validate_response(&self, _name: &str, _response: &str) -> Result<bool> {
            Ok(true)
        }
    }

    /// 脚本化 AiClient：按调用顺序返回预置的 chunk 批次。
    struct ScriptedAiClient {
        script: std::sync::Mutex<VecDeque<Vec<ChatCompletionChunk>>>,
    }

    impl ScriptedAiClient {
        fn new(batches: Vec<Vec<ChatCompletionChunk>>) -> Self {
            Self {
                script: std::sync::Mutex::new(batches.into()),
            }
        }
        fn text_chunk(text: &str) -> Vec<ChatCompletionChunk> {
            vec![ChatCompletionChunk {
                id: "c".into(),
                object: "chat.completion.chunk".into(),
                created: 0,
                model: "mock".into(),
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: DeltaMessage {
                        role: None,
                        content: Some(text.to_string()),
                        tool_calls: None,
                        reasoning_content: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                }],
                system_fingerprint: None,
                usage: None,
            }]
        }
        fn tool_calls_chunk(calls: &[(&str, &str, Value)]) -> Vec<ChatCompletionChunk> {
            vec![ChatCompletionChunk {
                id: "c".into(),
                object: "chat.completion.chunk".into(),
                created: 0,
                model: "mock".into(),
                choices: vec![ChunkChoice {
                    index: 0,
                    delta: DeltaMessage {
                        role: None,
                        content: None,
                        tool_calls: Some(
                            calls
                                .iter()
                                .enumerate()
                                .map(|(i, (id, name, args))| DeltaToolCall {
                                    index: i as u32,
                                    id: Some(id.to_string()),
                                    r#type: Some(ToolType::Function),
                                    function: Some(DeltaFunctionCall {
                                        name: Some(name.to_string()),
                                        arguments: Some(args.to_string()),
                                    }),
                                })
                                .collect(),
                        ),
                        reasoning_content: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                }],
                system_fingerprint: None,
                usage: None,
            }]
        }
    }

    #[async_trait]
    impl planned_agent_core::ai::AiClient for ScriptedAiClient {
        async fn chat_completion(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<planned_agent_core::ai::types::ChatCompletionResponse> {
            unimplemented!("测试仅使用 chat_completion_stream")
        }
        async fn chat_completion_stream(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<ChatCompletionStream> {
            let chunks = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow!("脚本化 client 响应耗尽"))?;
            let s = futures::stream::iter(chunks.into_iter().map(Ok::<_, anyhow::Error>));
            Ok(ChatCompletionStream::new(Box::new(s)))
        }
        fn provider_name(&self) -> &str {
            "scripted"
        }
        fn model_name(&self) -> &str {
            "scripted-model"
        }
        fn default_config(&self) -> ChatCompletionRequest {
            unimplemented!()
        }
    }

    fn make_service(script: Vec<Vec<ChatCompletionChunk>>) -> V2ChatService<MockPromptManager> {
        V2ChatService::from_ai_client(
            Arc::new(ScriptedAiClient::new(script)),
            Arc::new(ToolRegistry::new()),
            Arc::new(MockPromptManager),
            V2ChatConfig::new(),
        )
    }

    fn request_user_action_args(message: &str, action_ids: &[&str]) -> Value {
        json!({
            "message": message,
            "actions": action_ids.iter().map(|id| json!({
                "id": id,
                "type": "confirm",
                "label": id,
            })).collect::<Vec<_>>(),
        })
    }

    /// 事件收集 helper：handler 转发到 channel，测试侧顺序消费。
    fn collect_events(
        svc: &V2ChatService<MockPromptManager>,
    ) -> mpsc::UnboundedReceiver<V2ChatEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        svc.on_chat(move |ev| {
            let _ = tx.send(ev);
        });
        rx
    }

    /// 完整闭环：单轮无工具 → 文本 → Done。
    #[tokio::test(flavor = "current_thread")]
    async fn send_text_completes_with_done() {
        let svc = make_service(vec![ScriptedAiClient::text_chunk("你好，我是助手")]);
        let mut events = collect_events(&svc);

        let ticket = svc.send_text("你好").expect("send 成功");
        ticket.wait().await.expect("对话正常结束");

        // 事件序列：RoundStart → TextDelta → RoundEnd → Done
        let mut texts = vec![];
        let mut done = false;
        while let Ok(ev) = events.try_recv() {
            match ev {
                V2ChatEvent::Chat(ChatEvent::TextDelta(t)) => texts.push(t),
                V2ChatEvent::Done { cancelled } => {
                    assert!(!cancelled);
                    done = true;
                }
                _ => {}
            }
        }
        assert_eq!(texts.join(""), "你好，我是助手");
        assert!(done, "应收到 Done 事件");

        // 历史：System + User + Assistant
        let history = svc.history();
        let roles: Vec<_> = history.iter().map(|m| m.role.clone()).collect();
        assert_eq!(roles.len(), 3);
        assert!(matches!(roles[0], MessageRole::System));
        assert!(matches!(roles[1], MessageRole::User));
        assert!(matches!(roles[2], MessageRole::Assistant));
        assert!(
            !svc.is_awaiting_user_action(),
            "完成后不应处于 awaiting 状态"
        );
    }

    /// 多轮 UI 确认闭环（用户示例场景）：
    /// user → assistant(tool_calls:[req(c1)]) → confirm(c1,"JSON") 压入 tool(c1)
    /// → assistant(tool_calls:[req(c2)]) → confirm(c2,"确认") 压入 tool(c2)
    /// → assistant 最终文本 → Done
    #[tokio::test(flavor = "current_thread")]
    async fn confirm_user_action_multi_round_pushes_tool_messages() {
        let svc = make_service(vec![
            ScriptedAiClient::tool_calls_chunk(&[(
                "c1",
                "request_user_action",
                request_user_action_args("请选择计划方式", &["json", "markdown"]),
            )]),
            ScriptedAiClient::tool_calls_chunk(&[(
                "c2",
                "request_user_action",
                request_user_action_args("确认计划", &["confirm"]),
            )]),
            ScriptedAiClient::text_chunk("好的，计划如下：..."),
        ]);
        let mut events = collect_events(&svc);

        let ticket = svc.send_text("帮我做一个计划").expect("send 成功");

        // 第 1 次 UI 请求
        let mut pending_count = 0usize;
        loop {
            let ev = events.recv().await.expect("收到事件");
            match ev {
                V2ChatEvent::Chat(ChatEvent::UIActionRequest { message, .. }) => {
                    assert_eq!(message, "请选择计划方式");
                    pending_count += 1;
                    break;
                }
                V2ChatEvent::Done { .. } => panic!("不应提前结束"),
                _ => {}
            }
        }
        assert!(
            svc.is_awaiting_user_action(),
            "等待确认期间状态应为 awaiting"
        );
        svc.confirm_user_action("c1", "JSON", "json")
            .expect("confirm 成功");

        // 第 2 次 UI 请求
        loop {
            let ev = events.recv().await.expect("收到事件");
            match ev {
                V2ChatEvent::Chat(ChatEvent::UIActionRequest { message, .. }) => {
                    assert_eq!(message, "确认计划");
                    pending_count += 1;
                    break;
                }
                V2ChatEvent::Done { .. } => panic!("不应提前结束"),
                _ => {}
            }
        }
        svc.confirm_user_action("c2", "确认", "confirm")
            .expect("confirm 成功");

        ticket.wait().await.expect("整段对话正常结束");
        assert_eq!(pending_count, 2);
        assert!(!svc.is_awaiting_user_action(), "结束后不再 awaiting");

        // 历史校验：assistant(tool_calls) 后紧跟对应 tool 消息，且 content 为 {"choice": ...}
        let history = svc.history();
        let tool_msgs: Vec<&Message> = history
            .iter()
            .filter(|m| matches!(m.role, MessageRole::Tool))
            .collect();
        assert_eq!(tool_msgs.len(), 2, "应有两条 tool 消息（c1/c2）");
        assert_eq!(tool_msgs[0].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(tool_msgs[1].tool_call_id.as_deref(), Some("c2"));
        let c1_content: Value = serde_json::from_str(
            &tool_msgs[0]
                .content
                .as_ref()
                .and_then(|c| match c {
                    MessageContent::ToolResult { content, .. } => Some(content.as_str()),
                    _ => None,
                })
                .expect("tool 消息内容"),
        )
        .unwrap();
        assert_eq!(c1_content, json!({ "choice": "JSON", "action_id": "json" }));
        let c2_content: Value = serde_json::from_str(
            &tool_msgs[1]
                .content
                .as_ref()
                .and_then(|c| match c {
                    MessageContent::ToolResult { content, .. } => Some(content.as_str()),
                    _ => None,
                })
                .expect("tool 消息内容"),
        )
        .unwrap();
        assert_eq!(
            c2_content,
            json!({ "choice": "确认", "action_id": "confirm" })
        );

        // 顺序：assistant 消息的 tool_calls 之后必须紧跟同 id 的 tool 消息
        let final_assistant_idx = history
            .iter()
            .position(|m| matches!(m.role, MessageRole::Assistant) && m.tool_calls.is_none())
            .expect("最终 assistant 文本消息");
        assert!(
            matches!(history[final_assistant_idx - 1].role, MessageRole::Tool),
            "最终文本前应是最后一个 tool 消息"
        );
    }

    /// 串行队列：awaiting 期间的 send 排队，当前对话结束后再处理下一条。
    #[tokio::test(flavor = "current_thread")]
    async fn send_during_awaiting_is_queued() {
        let svc = make_service(vec![
            ScriptedAiClient::tool_calls_chunk(&[(
                "c1",
                "request_user_action",
                request_user_action_args("请确认", &["ok"]),
            )]),
            ScriptedAiClient::text_chunk("第一轮完成"),
            ScriptedAiClient::text_chunk("第二轮完成"),
        ]);
        let mut events = collect_events(&svc);

        let t1 = svc.send_text("第一问").expect("send 成功");

        loop {
            let ev = events.recv().await.expect("收到事件");
            if matches!(ev, V2ChatEvent::Chat(ChatEvent::UIActionRequest { .. })) {
                break;
            }
            if matches!(ev, V2ChatEvent::Done { .. }) {
                panic!("不应提前结束");
            }
        }

        let t2 = svc
            .send_text("排队消息")
            .expect("awaiting 期间 send 应入队成功");
        svc.confirm_user_action("c1", "ok", "ok")
            .expect("confirm 成功");

        t1.wait().await.expect("t1 完成");
        t2.wait().await.expect("t2 完成");

        let history = svc.history();
        let user_msgs: Vec<String> = history
            .iter()
            .filter(|m| matches!(m.role, MessageRole::User))
            .filter_map(|m| match &m.content {
                Some(MessageContent::Text { text }) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_msgs,
            vec!["第一问", "排队消息"],
            "两条 user 消息按发送顺序入历史"
        );
    }

    /// 取消：awaiting 期间 stop → Done{cancelled:true}，ticket 正常返回。
    #[tokio::test(flavor = "current_thread")]
    async fn stop_during_awaiting_yields_cancelled_done() {
        let svc = make_service(vec![ScriptedAiClient::tool_calls_chunk(&[(
            "c1",
            "request_user_action",
            request_user_action_args("请确认", &["ok"]),
        )])]);
        let mut events = collect_events(&svc);

        let ticket = svc.send_text("问一下").expect("send 成功");

        loop {
            let ev = events.recv().await.expect("收到事件");
            if matches!(ev, V2ChatEvent::Chat(ChatEvent::UIActionRequest { .. })) {
                break;
            }
        }
        svc.stop();
        ticket
            .wait()
            .await
            .expect("取消也算正常结束（Done{cancelled:true}）");

        let mut done_cancelled = false;
        while let Ok(ev) = events.try_recv() {
            if let V2ChatEvent::Done { cancelled } = ev {
                done_cancelled = cancelled;
            }
        }
        assert!(done_cancelled, "应收到 Done cancelled=true");
        assert!(
            svc.is_cancelled(),
            "stop 后 cancelled 保持 true 直到下次 send"
        );
    }

    /// confirm 的 tool_call_id 不匹配 → Error 事件，等待继续；匹配后正常完成。
    #[tokio::test(flavor = "current_thread")]
    async fn confirm_wrong_tool_call_id_emits_error_and_retry() {
        let svc = make_service(vec![
            ScriptedAiClient::tool_calls_chunk(&[(
                "c1",
                "request_user_action",
                request_user_action_args("请确认", &["ok"]),
            )]),
            ScriptedAiClient::text_chunk("完成"),
        ]);
        let mut events = collect_events(&svc);

        let ticket = svc.send_text("问一下").expect("send 成功");
        loop {
            let ev = events.recv().await.expect("收到事件");
            if matches!(ev, V2ChatEvent::Chat(ChatEvent::UIActionRequest { .. })) {
                break;
            }
        }

        svc.confirm_user_action("wrong_id", "x", "a")
            .expect("confirm 发送成功");
        loop {
            let ev = events.recv().await.expect("收到事件");
            match ev {
                V2ChatEvent::Error(e) => {
                    assert!(e.contains("不匹配"), "错误信息应说明不匹配: {}", e);
                    break;
                }
                V2ChatEvent::Done { .. } => panic!("不应提前结束"),
                _ => {}
            }
        }

        svc.confirm_user_action("c1", "ok", "a")
            .expect("正确 confirm");
        ticket.wait().await.expect("完成");
    }

    /// 取消后历史不残留未闭合的 assistant tool_calls（协议闭合）。
    #[tokio::test(flavor = "current_thread")]
    async fn cancel_during_awaiting_cleans_unclosed_tool_calls() {
        let svc = make_service(vec![ScriptedAiClient::tool_calls_chunk(&[(
            "c1",
            "request_user_action",
            request_user_action_args("请确认", &["ok"]),
        )])]);
        let mut events = collect_events(&svc);

        let ticket = svc.send_text("问一下").expect("send 成功");
        loop {
            let ev = events.recv().await.expect("收到事件");
            if matches!(ev, V2ChatEvent::Chat(ChatEvent::UIActionRequest { .. })) {
                break;
            }
        }
        svc.stop();
        ticket.wait().await.expect("取消后正常结束");

        let history = svc.history();
        let has_unclosed = history
            .iter()
            .any(|m| matches!(m.role, MessageRole::Assistant) && m.tool_calls.is_some());
        assert!(
            !has_unclosed,
            "取消后不应存在带 tool_calls 的 assistant 消息"
        );
    }

    /// 订阅者 panic 被隔离：一个 handler 崩溃不影响其它订阅者与 driver。
    #[tokio::test(flavor = "current_thread")]
    async fn handler_panic_is_isolated() {
        let svc = make_service(vec![ScriptedAiClient::text_chunk("你好")]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        // 第一个订阅者：panic
        svc.on_chat(move |_| panic!("订阅者故意 panic"));
        // 第二个订阅者：正常转发
        svc.on_chat(move |ev| {
            let _ = tx.send(ev);
        });

        let ticket = svc.send_text("你好").expect("send 成功");
        ticket.wait().await.expect("对话正常结束");

        let mut done = false;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, V2ChatEvent::Done { .. }) {
                done = true;
            }
        }
        assert!(done, "panic 订阅者不影响正常订阅者收到 Done");
    }

    /// clear() 清空会话历史。
    #[tokio::test(flavor = "current_thread")]
    async fn clear_resets_history() {
        let svc = make_service(vec![ScriptedAiClient::text_chunk("你好")]);
        svc.send_text("你好")
            .expect("send 成功")
            .wait()
            .await
            .expect("完成");
        assert!(!svc.history().is_empty(), "对话后历史非空");
        svc.clear();
        assert!(svc.history().is_empty(), "clear 后历史为空");
    }

    /// 对话失败（LLM 请求异常）→ 历史回滚到调用前，不残留脏上下文。
    #[tokio::test(flavor = "current_thread")]
    async fn failed_conversation_rolls_back_history() {
        // 空脚本：chat_completion_stream 立即 Err
        let svc = make_service(vec![]);
        let mut events = collect_events(&svc);

        let ticket = svc.send_text("问一下").expect("send 成功");
        let err = ticket.wait().await.expect_err("应返回 Err");
        assert!(
            err.to_string().contains("脚本化 client 响应耗尽"),
            "错误信息: {}",
            err
        );

        let mut got_error = false;
        while let Ok(ev) = events.try_recv() {
            if matches!(ev, V2ChatEvent::Error(_)) {
                got_error = true;
            }
        }
        assert!(got_error, "应收到 Error 事件");
        assert!(svc.history().is_empty(), "失败后历史应回滚为空");
    }

    /// 方案 B 核心：不重建 service，热切换模板 + 会话重置后，
    /// 下次 send 注入**新**模板、且旧历史被清空。
    #[tokio::test(flavor = "current_thread")]
    async fn set_template_and_reset_session_reinjects_new_system_prompt() {
        let svc = make_service(vec![
            ScriptedAiClient::text_chunk("第一轮"),
            ScriptedAiClient::text_chunk("第二轮"),
        ]);

        // 第一轮：注入默认模板
        svc.send_text("你好")
            .expect("send 成功")
            .wait()
            .await
            .expect("完成");
        let h0 = svc.history();
        assert!(matches!(h0[0].role, MessageRole::System));
        let sys0 = match &h0[0].content {
            Some(MessageContent::Text { text }) => text.as_str(),
            _ => "",
        };
        assert!(
            sys0.contains("thorough/thorough_system"),
            "默认模板应注入，实际: {}",
            sys0
        );

        // 热切换模板 + 入队重置（不重建 service）
        svc.set_system_prompt_template(Some("chat/other".to_string()));
        svc.reset_session().expect("reset 入队成功");

        // 第二轮：Reset 在队列中先于本次 send 执行 → 注入新模板、旧历史消失
        svc.send_text("再问")
            .expect("send 成功")
            .wait()
            .await
            .expect("完成");
        let h1 = svc.history();
        let roles: Vec<_> = h1.iter().map(|m| m.role.clone()).collect();
        assert!(
            matches!(
                roles.as_slice(),
                [MessageRole::System, MessageRole::User, MessageRole::Assistant]
            ),
            "角色序列不符: {:?}",
            roles
        );
        assert!(matches!(h1[0].role, MessageRole::System));
        let sys1 = match &h1[0].content {
            Some(MessageContent::Text { text }) => text.as_str(),
            _ => "",
        };
        assert!(
            sys1.contains("chat/other"),
            "热切换后应注入新模板，实际: {}",
            sys1
        );
        let user_texts: Vec<String> = h1
            .iter()
            .filter(|m| matches!(m.role, MessageRole::User))
            .filter_map(|m| match &m.content {
                Some(MessageContent::Text { text }) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(user_texts, vec!["再问"], "旧历史应被 Reset 清空");
    }

    /// Reset 在 awaiting 期间入队：等当前对话结束后才执行清空（串行安全）。
    #[tokio::test(flavor = "current_thread")]
    async fn reset_during_awaiting_is_queued_until_conversation_ends() {
        let svc = make_service(vec![ScriptedAiClient::tool_calls_chunk(&[(
            "c1",
            "request_user_action",
            request_user_action_args("请确认", &["ok"]),
        )])]);
        let mut events = collect_events(&svc);

        let ticket = svc.send_text("问一下").expect("send 成功");
        loop {
            let ev = events.recv().await.expect("收到事件");
            if matches!(ev, V2ChatEvent::Chat(ChatEvent::UIActionRequest { .. })) {
                break;
            }
        }

        // awaiting 期间 reset 入队（不阻塞、不立即清空）
        svc.reset_session().expect("reset 入队成功");
        assert!(
            !svc.history().is_empty(),
            "awaiting 期间 reset 不应立即清空历史"
        );

        // 取消当前对话 → 对话结束后 driver 执行 Reset
        svc.stop();
        ticket.wait().await.expect("取消后正常结束");

        // 等待 driver 消费队列中的 Reset（轮询 history 直到为空）
        for _ in 0..50 {
            if svc.history().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            svc.history().is_empty(),
            "对话结束后 Reset 应清空历史"
        );
    }

    // ── SubscriptionGuard：RAII 自动退订 ──────────────────────────────────

    /// Guard Drop 后不再收到事件：验证 handler 闭包 + 捕获上下文随 guard
    /// 一起被释放（不会因为忘了调 `unsubscribe` 而泄漏）。
    #[tokio::test(flavor = "current_thread")]
    async fn subscription_guard_unsubscribes_on_drop() {
        let svc = make_service(vec![
            ScriptedAiClient::text_chunk("hi"),
            ScriptedAiClient::text_chunk("hi"), // 第二次 send 用
        ]);
        let (tx, mut rx) = mpsc::unbounded_channel();

        // 模拟"页面作用域"持有 guard
        {
            let _guard: SubscriptionGuard = svc.on_chat_with_guard(move |ev| {
                let _ = tx.send(ev);
            });
            assert_eq!(_guard.id().0, 1, "首个订阅 id 应为 1");

            // guard 还活着：第一次 send 能收到事件
            svc.send_text("a")
                .expect("send")
                .wait()
                .await
                .expect("完成");
            let mut got_done = false;
            while let Ok(ev) = rx.try_recv() {
                if matches!(ev, V2ChatEvent::Done { .. }) {
                    got_done = true;
                }
            }
            assert!(got_done, "guard 存活时应收到 Done");
        } // ← guard 在此 drop，应自动退订

        // guard 已 drop：再 send 不应再有事件流到 rx
        let count_before = {
            let mut n = 0;
            while rx.try_recv().is_ok() {
                n += 1;
            }
            n
        };
        assert_eq!(count_before, 0, "drop 后 rx 不应残留事件");

        svc.send_text("b")
            .expect("send")
            .wait()
            .await
            .expect("完成");

        let mut got_after_drop = 0usize;
        while rx.try_recv().is_ok() {
            got_after_drop += 1;
        }
        assert_eq!(
            got_after_drop, 0,
            "guard drop 后 handler 不应再被调用（核心反泄漏断言）"
        );
    }

    /// `detach()` 主动退订后 Drop 幂等：验证同一 guard 多次 Drop / 退订均安全。
    #[tokio::test(flavor = "current_thread")]
    async fn subscription_guard_detach_is_idempotent() {
        let svc = make_service(vec![]);
        let guard: SubscriptionGuard = svc.on_chat_with_guard(move |_| {});
        let id = guard.id();

        // 第一次 detach：主动退订
        guard.detach();
        // 第二次 Drop（guard 已被 detach 消费）——实际上 detach 已经消费了 guard，
        // 不能再用同名变量。改成：另起一个 guard 测 Drop 幂等。
        let guard2: SubscriptionGuard = svc.on_chat_with_guard(move |_| {});
        drop(guard2); // 第一次 Drop

        // 用一个 guard 测"id() 不变"和"两次手动 unsubscribe 均安全"
        let g3: SubscriptionGuard = svc.on_chat_with_guard(move |_| {});
        let id3 = g3.id();
        svc.unsubscribe(id3); // 手动 unsubscribe 一次
        svc.unsubscribe(id3); // 重复 unsubscribe 必须安全（no-op）
        drop(g3); // guard Drop 再退订一次（也无害，因为列表里已无该 id）

        // 整个流程没有 panic 即视为通过。id 校验仅作 sanity check。
        assert_eq!(id.0, 1);
        assert_eq!(id3.0, 3, "id 单调递增");
    }

    /// Guard 不延长 service 寿命：service 全 drop 后 guard Drop 自动 no-op
    /// （不 panic、不泄漏）。
    #[tokio::test(flavor = "current_thread")]
    async fn subscription_guard_drop_is_noop_after_service_dropped() {
        let guard = {
            let svc = make_service(vec![]);
            let (tx, _rx) = mpsc::unbounded_channel();
            svc.on_chat_with_guard(move |ev| {
                let _ = tx.send(ev);
            })
            // svc 在此离开作用域 → Arc 计数归零 → driver 自动退出
        };
        // guard 还活着，但 inner_weak 已 upgrade 失败 → Drop 走 no-op 分支
        drop(guard); // 不应 panic
    }

    /// 多个 guard + service 重建：guard 各自独立退订；service 替换不影响旧 guard。
    #[tokio::test(flavor = "current_thread")]
    async fn multiple_guards_unsubscribe_independently() {
        let svc = make_service(vec![
            ScriptedAiClient::text_chunk("hi"),
            ScriptedAiClient::text_chunk("hi"), // 第二次 send 用
        ]);
        let (tx1, mut rx1) = mpsc::unbounded_channel();
        let (tx2, mut rx2) = mpsc::unbounded_channel();

        let g1: SubscriptionGuard = svc.on_chat_with_guard(move |ev| {
            let _ = tx1.send(ev);
        });
        let g2: SubscriptionGuard = svc.on_chat_with_guard(move |ev| {
            let _ = tx2.send(ev);
        });

        // 两个都活着：两个 channel 都收到
        svc.send_text("a").expect("send").wait().await.expect("完成");
        let mut r1_done = false;
        while let Ok(ev) = rx1.try_recv() {
            if matches!(ev, V2ChatEvent::Done { .. }) {
                r1_done = true;
            }
        }
        let mut r2_done = false;
        while let Ok(ev) = rx2.try_recv() {
            if matches!(ev, V2ChatEvent::Done { .. }) {
                r2_done = true;
            }
        }
        assert!(r1_done && r2_done, "两个 guard 都应收到 Done");

        // 只 drop g1：rx1 收不到，rx2 仍能收到
        drop(g1);
        svc.send_text("b").expect("send").wait().await.expect("完成");

        let mut r1_after = 0usize;
        while rx1.try_recv().is_ok() {
            r1_after += 1;
        }
        let mut r2_after = false;
        while let Ok(ev) = rx2.try_recv() {
            if matches!(ev, V2ChatEvent::Done { .. }) {
                r2_after = true;
            }
        }
        assert_eq!(r1_after, 0, "g1 drop 后 rx1 不应再收到");
        assert!(r2_after, "g2 仍存活，rx2 应继续收到");

        // g2 也 drop：彻底干净
        drop(g2);
    }
}