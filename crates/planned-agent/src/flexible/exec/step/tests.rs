//! `step` 的单元测试（从 `mod.rs` 外移，内容未改）。

    use super::*;
    use crate::flexible::testing::{
        fake_tool, text_response, tool_response, FakeAiClient, RecordingSink,
    };
    use crate::flexible::{ExecutorConfig, StepStatus};
    use async_trait::async_trait;
    use planned_agent_core::mcp::types::ToolResult;
    use planned_agent_core::tool_registry::{ToolCategory, ToolExecutor};
    use serde_json::{json, Value};
    use std::sync::Mutex;

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

    /// 第一次固定报「路径类错误」、之后成功的假工具 —— 用于验证回退只发生一次。
    struct FailOnceTool {
        calls: Mutex<Vec<Value>>,
    }

    #[async_trait]
    impl ToolExecutor for FailOnceTool {
        async fn execute(&self, _tool_name: &str, arguments: Value) -> anyhow::Result<ToolResult> {
            let mut calls = self.calls.lock().expect("calls 锁");
            calls.push(arguments);
            // 第一次（补全后的参数）报 ENOENT，第二次（原参数）成功。
            let first = calls.len() == 1;
            Ok(ToolResult {
                call_id: String::new(),
                content: json!(if first {
                    "Error: ENOENT: no such file or directory"
                } else {
                    "ok"
                }),
                is_error: first,
            })
        }

        fn name(&self) -> &str {
            "needs_path"
        }

        fn description(&self) -> &str {
            "测试用假工具"
        }

        fn supported_tools(&self) -> Vec<String> {
            vec!["needs_path".to_string()]
        }

        fn supports_tool(&self, name: &str) -> bool {
            name == "needs_path"
        }
    }

    /// 注册 `FailOnceTool`，其 schema 里有一个路径类参数。
    fn registry_with_fail_once() -> (Arc<ToolRegistry>, Arc<FailOnceTool>) {
        let registry = Arc::new(ToolRegistry::new());
        let tool = Arc::new(FailOnceTool {
            calls: Mutex::new(Vec::new()),
        });
        registry.register_custom_tool(
            planned_agent_core::mcp::types::Tool {
                name: "needs_path".to_string(),
                description: "测试工具".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {"filename": {"type": "string"}}
                }),
            },
            vec![ToolCategory::File],
            tool.clone(),
        );
        (registry, tool)
    }

    fn one_rewrite() -> Vec<call_arguments::PathRewrite> {
        vec![call_arguments::PathRewrite {
            key: "filename".to_string(),
            from: "a.png".to_string(),
            to: "D:/out/a.png".to_string(),
        }]
    }

    /// 补全后的参数报「路径找不到」→ 用模型给的原参数重试一次，并如实报告用了哪一份。
    #[tokio::test]
    async fn fallback_retries_once_with_original_arguments_on_path_error() {
        let (registry, tool) = registry_with_fail_once();
        let (result, used_rewritten) = call_tool_with_path_fallback(
            &registry,
            "needs_path",
            json!({"filename": "D:/out/a.png"}),
            json!({"filename": "a.png"}),
            &one_rewrite(),
        )
        .await;

        let outcome = result.expect("回退应当成功");
        assert!(!outcome.result.is_error);
        // 结果来自**原参数** → 回灌不能宣称「已补全」。
        assert!(!used_rewritten);

        let calls = tool.calls.lock().expect("calls 锁").clone();
        assert_eq!(calls.len(), 2, "只应重试一次");
        assert_eq!(calls[0], json!({"filename": "D:/out/a.png"}));
        assert_eq!(calls[1], json!({"filename": "a.png"}));
    }

    /// 失败与路径无关 → **不**重试（重试等于把带副作用的工具白跑一次）。
    #[tokio::test]
    async fn no_fallback_when_error_is_unrelated_to_paths() {
        let (registry, tool) = registry_with(
            "noop",
            json!("Cannot type text into input[type=number]"),
            true,
        );
        let (result, used_rewritten) = call_tool_with_path_fallback(
            &registry,
            "noop",
            json!({"filename": "D:/out/a.png"}),
            json!({"filename": "a.png"}),
            &one_rewrite(),
        )
        .await;

        assert!(result
            .expect("结果是 Ok，失败由 is_error 表达")
            .result
            .is_error);
        assert!(used_rewritten, "结果来自补全参数");
        assert_eq!(
            tool.calls.lock().expect("calls 锁").len(),
            1,
            "与路径无关的失败不该触发重试"
        );
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
                run_dir: "",
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

    /// 整步链路：模型给的**裸文件名** → 调用前补成产出目录下的完整路径 → 回灌告知模型。
    ///
    /// 单测只证明了 `resolve` 纯函数正确；这条证明**它真的接在调用链上**，
    /// 而且判定过程写进了日志（否则线上「为什么没生效」无法诊断）。
    #[tokio::test]
    async fn bare_filename_is_rewritten_into_run_dir_across_the_step() {
        let captured = CapturedLog::default();
        let _guard = captured.install();

        let ai = FakeAiClient::new(vec![
            tool_response("call-1", "shot", json!({"filename": "captcha-1.png"}), 10, 5),
            text_response("已截图", 20, 5),
        ]);
        // schema 里**有**路径参数，才可能被补全（对照 `registry_with`：它没有 properties）。
        let registry = Arc::new(ToolRegistry::new());
        let tool = fake_tool("shot", json!("saved"), false);
        registry.register_custom_tool(
            planned_agent_core::mcp::types::Tool {
                name: "shot".to_string(),
                description: "测试工具".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {"filename": {"type": "string"}}
                }),
            },
            vec![ToolCategory::File],
            tool.clone(),
        );
        let cache_dir = std::env::temp_dir().join("pa-step-arg-rewrite");
        let step_def = step("#E1");
        let sink = RecordingSink::default();

        let result = run_step(
            prompt::STEP_SYSTEM_PROMPT,
            StepInput {
                step: &step_def,
                intent: "截图",
                expected_output: "存下来",
                prior: &[],
                tools: &[],
                index: 1,
                run_dir: "run-1",
            },
            // `clone` 而非 `ai` 本身：`as` cast 会 move，后面还要用 `ai.requests()` 断言回灌。
            &(ai.clone() as Arc<dyn AiClient>),
            &registry,
            &ExecutorConfig {
                max_rounds_per_step: MAX_ROUNDS,
                cache_dir: cache_dir.clone(),
                ..Default::default()
            },
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Done);

        // ① 工具**实际收到**的是产出目录下的完整路径，不是裸名。
        let expected = cache_dir
            .join("run-1")
            .join("captcha-1.png")
            .to_string_lossy()
            .into_owned();
        let calls = tool.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1["filename"].as_str(), Some(expected.as_str()));

        // ② 回灌：模型在下一轮看得到「参数被补过」。
        let requests = ai.requests();
        let follow_up = format!("{:?}", requests[1].messages);
        assert!(
            follow_up.contains("已按本次执行的产出目录补全"),
            "回灌文案应进 messages：{follow_up}"
        );

        // ③ 日志：判定过程可观测（带工具名与识别到的路径参数名）。
        let log = captured.text();
        let decision = log
            .lines()
            .find(|line| line.contains("工具入参的路径判定"))
            .unwrap_or_else(|| panic!("没有记下「路径判定」日志：{log}"));
        assert!(
            decision.contains("filename"),
            "日志要带识别到的路径参数名：{decision}"
        );
        assert!(decision.contains("shot"), "日志要带工具名：{decision}");
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
                run_dir: "",
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
        // ⑤ 单次明细条数 == 轮数 + 空回答重发次数（本例无重发）
        assert_eq!(
            result.record.call_usages.len(),
            result.record.rounds + result.record.llm_retries
        );
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
                run_dir: "",
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
                run_dir: "",
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
                run_dir: "",
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
                run_dir: "",
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
    ///
    /// A6：空回答先按**同一条请求**重发 `llm_empty_retries` 次（默认 1），仍空才判失败。
    #[tokio::test]
    async fn empty_answer_marks_step_failed() {
        // 首发 + 1 次重发都用尽，仍然空
        let ai = FakeAiClient::new(vec![text_response("", 10, 5), text_response("", 10, 5)]);
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
                run_dir: "",
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
        // 重发**不占**轮数：仍然是第 1 轮，只是多发了 1 次请求
        assert_eq!(result.record.rounds, 1);
        assert_eq!(result.record.llm_retries, 1);
        assert_eq!(result.record.call_usages.len(), 2);
        let error = result.record.error.as_deref().unwrap_or_default();
        assert!(error.contains("未产出内容"), "错误应说明空产出：{error}");
        assert!(error.contains("已重发 1 次"), "错误应体现重发次数：{error}");
    }

    /// A6：provider 偶发空响应不该让整步失败 —— 同一轮重发**同一条请求**即可自愈。
    #[tokio::test]
    async fn empty_answer_retried_then_succeeds() {
        let ai = FakeAiClient::new(vec![
            text_response("", 10, 5),
            text_response("重发后的产出", 60, 7),
        ]);
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
                run_dir: "",
            },
            &(ai.clone() as Arc<dyn AiClient>),
            &registry,
            &cfg(MAX_ROUNDS),
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Done);
        assert_eq!(result.output.as_deref(), Some("重发后的产出"));
        assert_eq!(result.record.rounds, 1, "重发不占轮数");
        assert_eq!(result.record.llm_retries, 1);
        // 每次请求各记一条：1 次首发 + 1 次重发
        assert_eq!(
            result.record.call_usages.len(),
            result.record.rounds + result.record.llm_retries
        );
        assert_eq!(result.record.prompt_tokens, 70);
        assert_eq!(result.record.completion_tokens, 12);
        // 关键：重发的确实是**同一条请求**（`messages` 一字未动）—— 否则工具可能被重复触发。
        // `Message` 未必实现 `PartialEq`，所以比对 Debug 渲染。
        let requests = ai.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            format!("{:?}", requests[0].messages),
            format!("{:?}", requests[1].messages),
            "重发必须复用同一份上下文"
        );
    }

    /// A6：关闭重发（`llm_empty_retries = 0`）时与改造前逐字等价。
    #[tokio::test]
    async fn empty_retries_disabled_fails_immediately() {
        let ai = FakeAiClient::new(vec![text_response("", 10, 5)]);
        let (registry, _tool) = registry_with("noop", json!("x"), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();
        let config = ExecutorConfig {
            llm_empty_retries: 0,
            ..cfg(MAX_ROUNDS)
        };

        let result = run_step(
            prompt::STEP_SYSTEM_PROMPT,
            StepInput {
                step: &step_def,
                intent: "做事",
                expected_output: "做完",
                prior: &[],
                tools: &[],
                index: 1,
                run_dir: "",
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &config,
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Failed);
        assert_eq!(result.record.llm_retries, 0);
        assert_eq!(result.record.call_usages.len(), 1);
        // 文案保持改造前的原文（不带「已重发」）
        assert_eq!(
            result.record.error.as_deref(),
            Some("模型未产出内容（空回答）")
        );
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
                run_dir: "",
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

    // ─── 单步内工具输出落盘 ──────────────────────────────────────
    // 见 docs/planned-agent/flexible-step-tool-output-spill.md

    use planned_agent_core::ai::types::MessageContent;

    /// 每个用例一个独立临时目录（并行用例互不干扰）。
    fn temp_cache_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "planned-agent-flexible-tool-spill-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// 带落盘目录的配置。
    fn cfg_with_cache(max_rounds_per_step: usize, cache_dir: std::path::PathBuf) -> ExecutorConfig {
        ExecutorConfig {
            max_rounds_per_step,
            cache_dir,
            ..Default::default()
        }
    }

    /// 取第 `index` 条请求里那条 tool 消息的文本内容。
    fn tool_content_of_request(ai: &FakeAiClient, index: usize) -> String {
        let requests = ai.requests();
        let message = requests[index]
            .messages
            .iter()
            .find(|message| matches!(message.role, MessageRole::Tool))
            .unwrap_or_else(|| panic!("第 {index} 条请求里应有 tool 消息"));
        match &message.content {
            Some(MessageContent::ToolResult { content, .. }) => content.clone(),
            other => panic!("tool 消息应是 ToolResult，实为 {other:?}"),
        }
    }

    /// 工具输出超过阈值 → 落盘；**进 messages 的那一份**是引用（请求里看不到全文）。
    #[tokio::test]
    async fn oversized_tool_output_spills_and_request_carries_reference() {
        let huge = "网页正文".repeat(3_000); // 12_000 字符 > 默认阈值 8_000
        let ai = FakeAiClient::new(vec![
            tool_response("call-1", "read", json!({"path": "a.txt"}), 100, 20),
            text_response("已读取", 150, 10),
        ]);
        let (registry, _tool) = registry_with("read", json!(huge.clone()), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();
        let cache_dir = temp_cache_dir("oversized");

        let result = run_step(
            prompt::STEP_SYSTEM_PROMPT,
            StepInput {
                step: &step_def,
                intent: "读文件",
                expected_output: "做完",
                prior: &[],
                tools: &[],
                index: 1,
                run_dir: "run-test-0",
            },
            &(ai.clone() as Arc<dyn AiClient>),
            &registry,
            &cfg_with_cache(MAX_ROUNDS, cache_dir.clone()),
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Done);

        // ① 落盘文件存在，内容逐字等于工具原文
        let written = std::fs::read_to_string(cache_dir.join("run-test-0").join("tool-s1-r1-0.txt"))
            .expect("超阈值应已落盘");
        assert_eq!(written, huge);

        // ② 第二轮请求（带工具结果）里那条 tool 消息是引用，不是全文
        let content = tool_content_of_request(&ai, 1);
        assert!(content.contains("已存为临时文件"), "应是落盘引用：{content}");
        assert!(content.contains("content@"), "落盘路径须带 content@ 前缀：{content}");
        assert!(content.contains("tool-s1-r1-0.txt"), "应带文件路径：{content}");
        assert!(
            content.chars().count() < 2_000,
            "引用文案不该含全文，实际 {} 字符",
            content.chars().count()
        );

        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    /// 小输出不回归：原样内联，且不建目录、不写文件。
    #[tokio::test]
    async fn small_tool_output_stays_inline() {
        let ai = FakeAiClient::new(vec![
            tool_response("call-1", "read", json!({"path": "a.txt"}), 100, 20),
            text_response("已读取", 150, 10),
        ]);
        let (registry, _tool) = registry_with("read", json!("file body"), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();
        let cache_dir = temp_cache_dir("small");

        let result = run_step(
            prompt::STEP_SYSTEM_PROMPT,
            StepInput {
                step: &step_def,
                intent: "读文件",
                expected_output: "做完",
                prior: &[],
                tools: &[],
                index: 1,
                run_dir: "run-test-1",
            },
            &(ai.clone() as Arc<dyn AiClient>),
            &registry,
            &cfg_with_cache(MAX_ROUNDS, cache_dir.clone()),
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Done);
        assert_eq!(tool_content_of_request(&ai, 1), "file body");
        assert!(
            !cache_dir.join("run-test-1").exists(),
            "未超阈值就不该建目录或写文件"
        );

        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn image_notes_are_appended_to_tool_text() {
        // 无说明 → 原文一字不动（纯文本路径的行为不变）
        assert_eq!(join_text_and_image_notes("done", &[]), "done");

        let notes = vec!["- file@D:\\cache\\img-s1-r0-c0-0.png（image/png，8 字节）".to_string()];
        let rendered = join_text_and_image_notes("screenshot taken", &notes);
        assert!(rendered.starts_with("screenshot taken\n\n"), "{rendered}");
        // 只声明「这是已落盘的文件」，不指导用什么工具读它
        assert!(
            rendered.contains("- file@D:\\cache\\img-s1-r0-c0-0.png"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("用能读取本地图片的工具"),
            "不该指导怎么读文件：{rendered}"
        );

        // 文本为空时不该出现前导空行
        let only_image = join_text_and_image_notes("", &notes);
        assert!(only_image.starts_with("（工具返回了 1 张图片"), "{only_image}");
    }

    /// 含图片的工具结果：图片落盘、tool 消息里只有**绝对路径**。
    ///
    /// 覆盖两件事：① `mod.rs` 回灌点的类型分支确实认出了图片；② 脱敏不变量 ——
    /// base64 既不进上下文，也不进 tracing 日志（设计稿 §4.4-1，风险最高的一项）。
    #[tokio::test]
    async fn image_tool_result_is_spilled_without_base64_in_message_or_logs() {
        const PNG_BASE64: &str = "iVBORw0KGgo=";

        let captured = CapturedLog::default();
        let _guard = captured.install();

        let ai = FakeAiClient::new(vec![
            tool_response("call-1", "shot", json!({"full_page": true}), 100, 20),
            text_response("已截图", 150, 10),
        ]);
        let (registry, _tool) = registry_with(
            "shot",
            json!([
                "screenshot taken",
                { "type": "image", "mime_type": "image/png", "data": PNG_BASE64 }
            ]),
            false,
        );
        let step_def = step("#E1");
        let sink = RecordingSink::default();
        let cache_dir = temp_cache_dir("image");

        let result = run_step(
            prompt::STEP_SYSTEM_PROMPT,
            StepInput {
                step: &step_def,
                intent: "截图",
                expected_output: "做完",
                prior: &[],
                tools: &[],
                index: 1,
                run_dir: "run-image-1",
            },
            &(ai.clone() as Arc<dyn AiClient>),
            &registry,
            &cfg_with_cache(MAX_ROUNDS, cache_dir.clone()),
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Done);

        let tool_text = tool_content_of_request(&ai, 1);
        assert!(tool_text.contains("screenshot taken"), "{tool_text}");
        // 图片只以 file@ 路径示人（提示「这是文件，不是未全文注入的正文」）
        assert!(tool_text.contains("file@"), "{tool_text}");
        assert!(tool_text.contains("img-s1-r1-c0-0.png"), "{tool_text}");
        assert!(
            !tool_text.contains("builtin_recognize_image"),
            "回灌文案不点名工具（没注册 / 改名时会变幻觉源）：{tool_text}"
        );
        assert!(
            !tool_text.contains(PNG_BASE64),
            "base64 不得进上下文：{tool_text}"
        );

        // 图片落盘到 run 目录（文件名格式由 `image.rs` 的单测锁定，这里只验证存在）。
        let spilled_dir = cache_dir.join("run-image-1");
        assert!(spilled_dir.exists(), "应建目录 {}", spilled_dir.display());
        let pngs = std::fs::read_dir(&spilled_dir)
            .expect("读 run 目录")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "png")
            })
            .count();
        assert_eq!(pngs, 1, "应落盘 1 张 png");

        assert!(
            !captured.text().contains(PNG_BASE64),
            "base64 不得进日志：\n{}",
            captured.text()
        );

        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    /// 落盘失败 → 告警并回退内联全文，且**该步仍然成功**
    /// （与跨步产出「IO 失败 → 该步 Failed」刻意不同，见设计稿 §2.7）。
    #[tokio::test]
    async fn spill_failure_falls_back_to_inline_without_failing_step() {
        let huge = "网页正文".repeat(3_000);
        let ai = FakeAiClient::new(vec![
            tool_response("call-1", "read", json!({"path": "a.txt"}), 100, 20),
            text_response("已读取", 150, 10),
        ]);
        let (registry, _tool) = registry_with("read", json!(huge.clone()), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();

        // 把一个**文件**当 cache_dir：`create_dir_all` 必失败
        let blocker = std::env::temp_dir().join(format!(
            "planned-agent-tool-spill-blocker-{}",
            std::process::id()
        ));
        std::fs::write(&blocker, b"not a directory").expect("写 blocker 文件");

        let result = run_step(
            prompt::STEP_SYSTEM_PROMPT,
            StepInput {
                step: &step_def,
                intent: "读文件",
                expected_output: "做完",
                prior: &[],
                tools: &[],
                index: 1,
                run_dir: "run-test-2",
            },
            &(ai.clone() as Arc<dyn AiClient>),
            &registry,
            &cfg_with_cache(MAX_ROUNDS, blocker.clone()),
            &sink,
            None,
        )
        .await;

        assert_eq!(
            result.record.status,
            StepStatus::Done,
            "落盘失败不该判该步失败"
        );
        let content = tool_content_of_request(&ai, 1);
        assert!(
            !content.contains("已存为临时文件"),
            "落盘失败应回退内联，而不是引用"
        );
        assert_eq!(content, huge, "回退时应是全文");

        let _ = std::fs::remove_file(&blocker);
    }

    /// 回灌函数本身：阈值两侧的行为、文件名、文案（含 `limit` 说明的回归锁）。
    #[tokio::test]
    async fn tool_output_for_llm_switches_at_threshold() {
        let step_def = step("#E1");
        let cache_dir = temp_cache_dir("unit");
        let cfg = cfg_with_cache(MAX_ROUNDS, cache_dir.clone());
        let input = StepInput {
            step: &step_def,
            intent: "读文件",
            expected_output: "做完",
            prior: &[],
            tools: &[],
            index: 1,
            run_dir: "run-unit",
        };

        // 未超阈值：原样返回
        assert_eq!(
            super::tool_output_for_llm("小输出", &input, &cfg, 1, 0).await,
            "小输出"
        );

        // 超阈值：引用文案，带轮次与序号命名的文件
        let reference = super::tool_output_for_llm(&"甲".repeat(9_000), &input, &cfg, 2, 1).await;
        assert!(reference.contains("工具输出较大"), "{reference}");
        assert!(reference.contains("tool-s1-r2-1.txt"), "{reference}");
        assert!(reference.contains("不传 `limit` 会一次读到文件末尾"), "{reference}");

        // 同轮两个工具各写一份，互不覆盖
        super::tool_output_for_llm(&"乙".repeat(9_000), &input, &cfg, 3, 0).await;
        super::tool_output_for_llm(&"丙".repeat(9_000), &input, &cfg, 3, 1).await;
        let dir = cache_dir.join("run-unit");
        assert!(dir.join("tool-s1-r3-0.txt").exists());
        assert!(dir.join("tool-s1-r3-1.txt").exists());
        assert_ne!(
            std::fs::read_to_string(dir.join("tool-s1-r3-0.txt")).unwrap(),
            std::fs::read_to_string(dir.join("tool-s1-r3-1.txt")).unwrap()
        );

        // 跨步骤不覆盖：`round`/`nth` 是**步内**计数，而 `run_dir` 是 run 级 ——
        // 文件名必须带步骤序号，否则不同步骤的同轮次同序号会互相覆盖。
        let input_step2 = StepInput {
            step: &step_def,
            intent: "读文件",
            expected_output: "做完",
            prior: &[],
            tools: &[],
            index: 2,
            run_dir: "run-unit",
        };
        super::tool_output_for_llm(&"丁".repeat(9_000), &input_step2, &cfg, 3, 0).await;
        assert!(
            dir.join("tool-s2-r3-0.txt").exists(),
            "不同步骤的同轮次同序号必须各写一份"
        );
        assert_ne!(
            std::fs::read_to_string(dir.join("tool-s1-r3-0.txt")).unwrap(),
            std::fs::read_to_string(dir.join("tool-s2-r3-0.txt")).unwrap()
        );

        let _ = std::fs::remove_dir_all(&cache_dir);
    }
