//! `step` 的单元测试（从 `mod.rs` 外移，内容未改）。

    use super::*;
    use crate::flexible::testing::{
        fake_tool, text_response, tool_response, FakeAiClient, RecordingSink,
    };
    use crate::flexible::{ExecutorConfig, StepStatus};
    use planned_agent_core::tool_registry::ToolCategory;
    use serde_json::json;

    const MAX_ROUNDS: usize = 5;

    fn cfg(max_rounds_per_step: usize) -> ExecutorConfig {
        ExecutorConfig {
            max_rounds_per_step,
            ..Default::default()
        }
    }

    /// 造一个只含单个步骤的输入，避免每条测试都写全字段。
    fn step(reference: &str) -> crate::flexible::PlanStep {
        crate::flexible::PlanStep {
            result_reference: reference.to_string(),
            intent: "做事".to_string(),
            expected_output: "做完".to_string(),
            dependencies: vec![],
        }
    }

    /// 注册一个假工具并返回 (registry, 调用记录)。
    fn registry_with(
        name: &str,
        output: Value,
        is_error: bool,
    ) -> (Arc<ToolRegistry>, Arc<super::super::super::testing::FakeTool>) {
        let registry = Arc::new(ToolRegistry::new());
        let tool = fake_tool(name, output, is_error);
        registry.register_custom_tool(
            planned_agent_core::mcp::types::Tool {
                name: name.to_string(),
                description: "测试工具".to_string(),
                input_schema: json!({"type": "object"}),
            },
            vec![ToolCategory::File],
            tool.clone(),
        );
        (registry, tool)
    }

    /// 一段可断言的日志缓冲区（配合 `tracing` 的线程局部订阅者使用）。
    #[derive(Clone, Default)]
    struct CapturedLog(Arc<std::sync::Mutex<Vec<u8>>>);

    impl CapturedLog {
        /// 在当前线程装上订阅者；返回的 guard 析构即卸载。
        fn install(&self) -> tracing::subscriber::DefaultGuard {
            let subscriber = tracing_subscriber::fmt()
                .with_writer(self.clone())
                .with_max_level(tracing::Level::DEBUG)
                .with_ansi(false)
                .without_time()
                .finish();
            tracing::subscriber::set_default(subscriber)
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("日志缓冲区锁被毒化")).to_string()
        }
    }

    impl std::io::Write for CapturedLog {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("日志缓冲区锁被毒化")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLog {
        type Writer = CapturedLog;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// 工具执行失败时，日志必须带上 **LLM 实际传的入参**。
    ///
    /// 这是失败现场唯一的证据：错误信息只会说「找不到指定的路径」/「program not found」，
    /// 而「它到底传了哪条路径、哪条命令」只在入参里 —— 少了它就只能靠错误码反推。
    #[tokio::test]
    async fn failed_tool_call_logs_its_arguments() {
        let captured = CapturedLog::default();
        let _guard = captured.install();

        // 故意调一个没注册的工具：`call_tool` 走 Err 分支（与 builtin_execute_command
        // 在 Windows 上拿不到可执行文件时是同一条路径）。
        let ai = FakeAiClient::new(vec![tool_response(
            "call-1",
            "builtin_execute_command",
            json!({"command": "echo 1 >> text.txt", "args": []}),
            10,
            5,
        )]);
        let (registry, _tool) = registry_with("noop", json!("x"), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();

        let result = run_step(
            prompt::STEP_SYSTEM_PROMPT,
            StepInput {
                step: &step_def,
                intent: "做事",
                expected_output: "做完",
                prior: &[],
                tools: &[],
                index: 1,
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &cfg(MAX_ROUNDS),
            &sink,
            None,
        )
        .await;

        assert!(result.record.tool_calls > 0, "应当发生过工具调用");
        let log = captured.text();
        // 精确到「失败那一行」：缓冲区里另有一条 debug 级「工具调用入参」，
        // 若只断言整段日志含入参，就分不清失败行自己到底带没带。
        let failed_line = log
            .lines()
            .find(|line| line.contains("工具执行失败"))
            .unwrap_or_else(|| panic!("没有记下「工具执行失败」这一行：{log}"));
        assert!(
            failed_line.contains("echo 1 >> text.txt"),
            "失败行必须带上 LLM 实际传的入参，否则无从分析：{failed_line}"
        );
    }

    #[tokio::test]
    async fn converges_without_tool_calls() {
        let ai = FakeAiClient::new(vec![text_response("完成", 10, 5)]);
        let (registry, _tool) = registry_with("noop", json!("x"), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();

        let result = run_step(
            prompt::STEP_SYSTEM_PROMPT,
            StepInput {
                step: &step_def,
                intent: "做事",
                expected_output: "做完",
                prior: &[],
                tools: &[],
                index: 1,
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &cfg(MAX_ROUNDS),
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Done);
        assert_eq!(result.output.as_deref(), Some("完成"));
        assert_eq!(result.record.rounds, 1);
        assert_eq!(result.record.tool_calls, 0);
        // ⑤ 单次明细条数 == 轮数
        assert_eq!(result.record.call_usages.len(), result.record.rounds);
        // ⑥ 步骤级 token == 各轮之和
        assert_eq!(result.record.prompt_tokens, 10);
        assert_eq!(result.record.completion_tokens, 5);
    }

    #[tokio::test]
    async fn converges_after_one_tool_call() {
        let ai = FakeAiClient::new(vec![
            tool_response("call-1", "read", json!({"path": "a.txt"}), 100, 20),
            text_response("已读取", 150, 10),
        ]);
        let (registry, tool) = registry_with("read", json!("file body"), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();

        let result = run_step(
            prompt::STEP_SYSTEM_PROMPT,
            StepInput {
                step: &step_def,
                intent: "读文件",
                expected_output: "做完",
                prior: &[],
                tools: &[],
                index: 1,
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &cfg(MAX_ROUNDS),
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Done);
        assert_eq!(result.output.as_deref(), Some("已读取"));
        assert_eq!(result.record.rounds, 2);
        assert_eq!(result.record.tool_calls, 1);
        assert_eq!(result.record.call_usages.len(), 2);
        // ⑥ 累加（prompt: 100 + 150）
        assert_eq!(result.record.prompt_tokens, 250);
        assert_eq!(result.record.completion_tokens, 30);

        // 工具确实被调用，且参数透传
        let calls = tool.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, json!({"path": "a.txt"}));

        // 事件流：StepToolCall(ok=true) 出现，且带着 `$` 行要显示的关键入参
        let events = sink.events();
        assert!(events.iter().any(|event| matches!(
            event,
            PlanRunEvent::StepToolCall { tool, ok: true, .. } if tool == "read"
        )));
        assert!(
            events.iter().any(|event| matches!(
                event,
                PlanRunEvent::StepToolCall { args, .. } if args == "a.txt"
            )),
            "事件要带上渲染好的关键入参（单个 path 字段直接给值）：{events:?}"
        );
    }

    #[tokio::test]
    async fn fails_when_exceeding_max_rounds() {
        // 每轮都要调工具 → 上限 2 时必然失败
        let ai = FakeAiClient::new(vec![
            tool_response("call-1", "read", json!({}), 10, 1),
            tool_response("call-2", "read", json!({}), 10, 1),
        ]);
        let (registry, _tool) = registry_with("read", json!("body"), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();

        let result = run_step(
            prompt::STEP_SYSTEM_PROMPT,
            StepInput {
                step: &step_def,
                intent: "读文件",
                expected_output: "做完",
                prior: &[],
                tools: &[],
                index: 1,
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &cfg(2),
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Failed);
        assert!(result.output.is_none());
        assert!(result
            .record
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("轮数上限"));
    }

    #[tokio::test]
    async fn error_tool_result_is_still_fed_back() {
        let ai = FakeAiClient::new(vec![
            tool_response("call-1", "read", json!({}), 10, 1),
            text_response("换个办法", 10, 1),
        ]);
        let (registry, _tool) = registry_with("read", json!("boom"), true);
        let step_def = step("#E1");
        let sink = RecordingSink::default();

        let result = run_step(
            prompt::STEP_SYSTEM_PROMPT,
            StepInput {
                step: &step_def,
                intent: "读文件",
                expected_output: "做完",
                prior: &[],
                tools: &[],
                index: 1,
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &cfg(MAX_ROUNDS),
            &sink,
            None,
        )
        .await;

        // 工具报错不终止本步：照常回灌后仍可收敛
        assert_eq!(result.record.status, StepStatus::Done);
        assert_eq!(result.record.tool_calls, 1);
        let events = sink.events();
        assert!(events.iter().any(|event| matches!(
            event,
            PlanRunEvent::StepToolCall { ok: false, .. }
        )));
    }

    #[tokio::test]
    async fn llm_error_marks_step_failed() {
        // 脚本为空 → FakeAiClient 返回 Err
        let ai = FakeAiClient::new(vec![]);
        let (registry, _tool) = registry_with("read", json!("body"), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();

        let result = run_step(
            prompt::STEP_SYSTEM_PROMPT,
            StepInput {
                step: &step_def,
                intent: "读文件",
                expected_output: "做完",
                prior: &[],
                tools: &[],
                index: 1,
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &cfg(MAX_ROUNDS),
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Failed);
        assert!(result
            .record
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("LLM 调用失败"));
    }

    /// 空产出（既无正文也无思考）不能被当成功 —— 否则空串会进 store 传给下游。
    #[tokio::test]
    async fn empty_answer_marks_step_failed() {
        let ai = FakeAiClient::new(vec![text_response("", 10, 5)]);
        let (registry, _tool) = registry_with("noop", json!("x"), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();

        let result = run_step(
            prompt::STEP_SYSTEM_PROMPT,
            StepInput {
                step: &step_def,
                intent: "做事",
                expected_output: "做完",
                prior: &[],
                tools: &[],
                index: 1,
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &cfg(MAX_ROUNDS),
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Failed);
        assert!(
            result.output.is_none(),
            "空产出不该进 store（否则会传给下游 prior）"
        );
        assert!(result
            .record
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("未产出内容"));
    }

    /// 取消应在下一次检查点终止本步。
    #[tokio::test]
    async fn cancel_stops_before_first_call() {
        let ai = FakeAiClient::new(vec![text_response("不该被调用", 1, 1)]);
        let (registry, _tool) = registry_with("read", json!("body"), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();
        let (tx, rx) = watch::channel(true); // 一开始就是取消状态
        drop(tx);

        let result = run_step(
            prompt::STEP_SYSTEM_PROMPT,
            StepInput {
                step: &step_def,
                intent: "读文件",
                expected_output: "做完",
                prior: &[],
                tools: &[],
                index: 1,
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &cfg(MAX_ROUNDS),
            &sink,
            Some(&rx),
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Failed);
        assert_eq!(result.record.rounds, 0);
        assert_eq!(result.record.call_usages.len(), 0);
    }

    /// `$` 行的渲染规则：要像一条命令行，且不能把终端撑爆。
    #[test]
    fn tool_args_render_like_a_command_line() {
        // 「程序 + 参数」拼成命令行
        assert_eq!(
            describe_tool_args(&json!({"command": "ls", "args": ["C:/x"]})),
            "ls C:/x"
        );
        // 单字段直接给值（不带 JSON 括号）—— `{"path": ...}` 是最常见的形态
        assert_eq!(
            describe_tool_args(&json!({"path": "C:/Users/x/a.txt"})),
            "C:/Users/x/a.txt"
        );
        assert_eq!(describe_tool_args(&json!({"command": "dir"})), "dir");
        // 字段多且不认识 → 退回紧凑 JSON（字段名本身有信息量，不该丢）
        assert_eq!(
            describe_tool_args(&json!({"a": 1, "b": 2})),
            "{\"a\":1,\"b\":2}"
        );
        // 超长截断
        let rendered = describe_tool_args(&json!({ "path": "x".repeat(400) }));
        assert!(
            rendered.chars().count() <= 121,
            "超长入参应截断，实际 {} 字符",
            rendered.chars().count()
        );
    }
