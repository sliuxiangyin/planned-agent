//! 子 agent 会话式流输出集成测试：注册 → 流式调用 → 事件转发 → 挂起/恢复。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_tool_manager::{
    CustomToolExecutor, OneShotSubAgentRunner, StreamKind, SubAgentRunOutcome, SubAgentSession,
    SubAgentSessionRunner, ToolCategory, ToolRegistry, ToolStreamSender,
};

/// mock 子 agent runner：按序发射 4 种过程事件后返回最终结果
struct MockRunner;

#[async_trait]
impl SubAgentSessionRunner for MockRunner {
    async fn start(
        &self,
        _arguments: Value,
        stream: ToolStreamSender,
    ) -> anyhow::Result<SubAgentRunOutcome> {
        stream.status("started").await?;
        stream.text("processing...").await?;
        stream.tool_call("inner_tool", &json!({"a": 1})).await?;
        stream.summary("done: 42").await?;
        Ok(SubAgentRunOutcome::Done(ToolResult {
            call_id: String::new(), // 执行器会覆写为 invocation_id
            content: json!({"answer": 42}),
            is_error: false,
        }))
    }
}

/// 会话 mock：start 时挂起一次，resume 后完成
struct SessionMock;

#[async_trait]
impl SubAgentSessionRunner for SessionMock {
    async fn start(
        &self,
        arguments: Value,
        stream: ToolStreamSender,
    ) -> anyhow::Result<SubAgentRunOutcome> {
        stream.status("started").await?;
        stream
            .text(format!("need confirm: {}", arguments["query"]))
            .await?;
        // 第一次调用挂起，等待用户输入
        Ok(SubAgentRunOutcome::AwaitingUserAction {
            session: Box::new(SessionMockSession {
                query: arguments["query"].as_str().unwrap_or("").to_string(),
            }),
            message: "请确认是否继续".to_string(),
            actions: json!([
                {"id": "confirm", "type": "confirm", "label": "继续"},
                {"id": "cancel", "type": "confirm", "label": "取消"}
            ]),
        })
    }
}

struct SessionMockSession {
    query: String,
}

#[async_trait]
impl SubAgentSession for SessionMockSession {
    async fn resume(
        &mut self,
        user_input: Value,
        stream: ToolStreamSender,
    ) -> anyhow::Result<SubAgentRunOutcome> {
        stream
            .text(format!("resumed with: {}", user_input))
            .await?;
        stream.summary("done").await?;
        Ok(SubAgentRunOutcome::Done(ToolResult {
            call_id: String::new(),
            content: json!({
                "query": self.query,
                "confirmed": user_input,
                "answer": "completed"
            }),
            is_error: false,
        }))
    }
}

fn sub_agent_tool() -> Tool {
    Tool {
        name: "research_agent".to_string(),
        description: "sub agent: research helper".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            }
        }),
    }
}

#[tokio::test]
async fn sub_agent_stream_forwards_events_and_links_call_id() {
    let registry = ToolRegistry::new();
    registry.register_sub_agent(
        sub_agent_tool(),
        vec![ToolCategory::Utility],
        Arc::new(MockRunner),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let stream = ToolStreamSender::new(tx, "research_agent", "inv-1");
    let outcome = registry
        .call_tool_streamed("research_agent", json!({"query": "q"}), "inv-1", stream)
        .await
        .expect("streamed call should succeed");

    // 收集全部事件
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }

    // 事件数量与顺序
    assert_eq!(events.len(), 4, "should receive 4 process events");
    assert_eq!(events[0].kind, StreamKind::Status);
    assert_eq!(events[1].kind, StreamKind::TextDelta);
    assert_eq!(events[2].kind, StreamKind::ToolCall);
    assert_eq!(events[3].kind, StreamKind::FinalSummary);

    // 工具名 / invocation_id / seq 关联
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(ev.tool_name, "research_agent");
        assert_eq!(ev.invocation_id, "inv-1");
        assert_eq!(ev.seq, i as u64, "seq should be monotonic from 0");
    }

    // 结果与过程流用同一 invocation_id 关联
    assert_eq!(outcome.result.call_id, "inv-1");
    assert_eq!(outcome.result.content, json!({"answer": 42}));
    assert!(!outcome.result.is_error);
}

#[tokio::test]
async fn sub_agent_call_tool_non_streamed_works() {
    let registry = ToolRegistry::new();
    registry.register_sub_agent(
        sub_agent_tool(),
        vec![ToolCategory::Utility],
        Arc::new(MockRunner),
    );

    // 非流式入口：行为与普通工具一致
    let outcome = registry
        .call_tool("research_agent", json!({"query": "q"}))
        .await
        .expect("non-streamed call should succeed");
    assert_eq!(outcome.result.content, json!({"answer": 42}));
}

#[tokio::test]
async fn non_sub_agent_tool_streamed_produces_no_events() {
    let registry = ToolRegistry::new();
    let executor = CustomToolExecutor::new("hello".to_string(), vec![], |_args| async move {
        Ok(ToolResult {
            call_id: "x".to_string(),
            content: json!("hi"),
            is_error: false,
        })
    });
    registry.register_custom_tool(
        Tool {
            name: "hello".to_string(),
            description: "plain tool".to_string(),
            input_schema: json!({"type": "object"}),
        },
        vec![ToolCategory::Utility],
        Arc::new(executor),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let stream = ToolStreamSender::new(tx, "hello", "inv-2");
    let outcome = registry
        .call_tool_streamed("hello", json!({}), "inv-2", stream)
        .await
        .expect("non-sub-agent streamed call should succeed");

    // 结果与 call_tool 一致
    assert_eq!(outcome.result.content, json!("hi"));
    // 不产生任何流事件
    assert!(rx.try_recv().is_err(), "non-sub-agent tool should produce no events");
}

#[tokio::test]
async fn sub_agent_unregister_cleans_executor() {
    let registry = ToolRegistry::new();
    registry.register_sub_agent(
        sub_agent_tool(),
        vec![ToolCategory::Utility],
        Arc::new(MockRunner),
    );

    registry
        .unregister_tool("research_agent")
        .expect("unregister should succeed");
    assert!(registry.get_tool("research_agent").is_none());
    // 卸载后调用应报"工具不存在"
    let err = registry.call_tool("research_agent", json!({})).await;
    assert!(err.is_err());
}

#[tokio::test]
async fn sub_agent_awaiting_user_action_then_resume() {
    let registry = ToolRegistry::new();
    registry.register_sub_agent(
        sub_agent_tool(),
        vec![ToolCategory::Utility],
        Arc::new(SessionMock),
    );

    // ── 第一次调用：挂起等待用户输入 ──
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let stream = ToolStreamSender::new(tx, "research_agent", "inv-1");
    let outcome = registry
        .call_tool_streamed("research_agent", json!({"query": "q"}), "inv-1", stream)
        .await
        .expect("first call ok");
    assert_eq!(outcome.result.call_id, "inv-1");

    let content = outcome.result.content;
    assert_eq!(
        content["status"].as_str(),
        Some("awaiting_user_action"),
        "挂起时应返回结构化标记"
    );
    let sid = content["session_id"].as_str().expect("session_id present").to_string();
    assert!(content["message"].as_str().is_some());
    assert!(content["actions"].is_array());
    assert_eq!(registry.sub_agent_session_count(), 1, "挂起会话应入存储");

    // 挂起流事件（status + text）
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, StreamKind::Status);
    assert_eq!(events[1].kind, StreamKind::TextDelta);

    // ── 第二次调用：带 session_id resume ──
    let (tx2, mut rx2) = tokio::sync::mpsc::channel(16);
    let stream2 = ToolStreamSender::new(tx2, "research_agent", "inv-2");
    let outcome2 = registry
        .call_tool_streamed(
            "research_agent",
            json!({"session_id": sid, "user_input": "confirm"}),
            "inv-2",
            stream2,
        )
        .await
        .expect("resume call ok");

    // 完成：会话清理
    assert_eq!(registry.sub_agent_session_count(), 0, "完成后会话应被清理");
    assert_eq!(outcome2.result.call_id, "inv-2");
    assert_eq!(outcome2.result.content, json!({
        "query": "q",
        "confirmed": "confirm",
        "answer": "completed"
    }));

    let mut events2 = Vec::new();
    while let Ok(ev) = rx2.try_recv() {
        events2.push(ev);
    }
    assert_eq!(events2.len(), 2);
    assert_eq!(events2[0].kind, StreamKind::TextDelta);
    assert_eq!(events2[1].kind, StreamKind::FinalSummary);
}

#[tokio::test]
async fn resume_with_unknown_session_id_errors() {
    let registry = ToolRegistry::new();
    registry.register_sub_agent(
        sub_agent_tool(),
        vec![ToolCategory::Utility],
        Arc::new(SessionMock),
    );

    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let stream = ToolStreamSender::new(tx, "research_agent", "inv-x");
    let err = registry
        .call_tool_streamed(
            "research_agent",
            json!({"session_id": "no-such-session", "user_input": "x"}),
            "inv-x",
            stream,
        )
        .await
        .expect_err("unknown session should error");
    assert!(err.to_string().contains("not found or expired"));
}

#[tokio::test]
async fn one_shot_runner_wrapper_works() {
    let registry = ToolRegistry::new();
    let runner = OneShotSubAgentRunner::new(|_args, _stream| async move {
        Ok(ToolResult {
            call_id: String::new(),
            content: json!("one-shot result"),
            is_error: false,
        })
    });
    registry.register_sub_agent(
        sub_agent_tool(),
        vec![ToolCategory::Utility],
        Arc::new(runner),
    );

    let outcome = registry
        .call_tool("research_agent", json!({}))
        .await
        .expect("one-shot call ok");
    assert_eq!(outcome.result.content, json!("one-shot result"));
    // 一次性 runner 永不挂起
    assert_eq!(registry.sub_agent_session_count(), 0);
}

/// 结构化 ChatEvent 旁路直传：`emit_event` 携带类型化事件原样到达，
/// `kind` 为兼容占位值，`seq` 保序。
#[tokio::test]
async fn emit_event_carries_structured_chat_event() {
    use planned_agent_core::events::ChatEvent;

    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let stream = ToolStreamSender::new(tx, "research_agent", "inv-ev");
    stream
        .emit_event(ChatEvent::ReasoningDelta("思考".to_string()))
        .await
        .expect("emit_event ok");

    let ev = rx.recv().await.expect("receive event");
    assert!(matches!(
        ev.event,
        Some(ChatEvent::ReasoningDelta(ref t)) if t == "思考"
    ), "结构化事件应原样到达");
    assert_eq!(ev.kind, StreamKind::TextDelta, "kind 为兼容占位值");
    assert_eq!(ev.tool_name, "research_agent");
    assert_eq!(ev.invocation_id, "inv-ev");
    assert_eq!(ev.seq, 0, "seq 从 0 开始");
}
