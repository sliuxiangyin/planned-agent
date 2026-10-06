//! `executor` 的单元测试（从 `mod.rs` 外移，内容未改）。

    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crate::flexible::exec::executor::config::LOG_OUTPUT_MAX_CHARS;

    use async_trait::async_trait;
    use planned_agent_core::ai::types::{ChatCompletionRequest, ChatCompletionResponse};
    use planned_agent_core::ai::ChatCompletionStream;

    use crate::flexible::testing::{
        fake_tool, text_response, tool_response, FakeAiClient, FakeTool, RecordingSink,
    };
    use planned_agent_core::tool_registry::ToolCategory;
    use serde_json::json;
    use crate::flexible::{PlanInput, PlanStep};

    /// 两步模板：第二步依赖第一步的 `#E1`，且 `intent` 含 `${path}` 占位符。
    fn two_step_template() -> FlexiblePlanTemplate {
        FlexiblePlanTemplate {
            output_schema: None,
            task: "维护文件".to_string(),
            inputs: vec![PlanInput {
                name: "path".to_string(),
                default: Some(serde_json::json!("a.txt")),
                description: None,
            }],
            steps: vec![
                PlanStep {
                    result_reference: "#E1".to_string(),
                    intent: "读取 ${path}".to_string(),
                    expected_output: "文件内容".to_string(),
                    dependencies: vec![],
                },
                PlanStep {
                    result_reference: "#E2".to_string(),
                    intent: "基于 #E1 追加一行".to_string(),
                    expected_output: "追加完成".to_string(),
                    dependencies: vec!["#E1".to_string()],
                },
            ],
        }
    }

    /// 有契约（`bool`）→ 追加一个整理步，结果取它的输出；契约渲染进请求，且**不带工具**。
    #[tokio::test]
    async fn output_contract_appends_resolve_step_and_returns_result() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
            text_response("文件末尾已新增一行（index=8）", 30, 3),
        ]);
        let template = FlexiblePlanTemplate {
            output_schema: Some(serde_json::json!({
                "kind": "bool",
                "goal": "向 ${path} 末尾追加一行",
                "success": "文件末尾新增一行即视为成功"
            })),
            ..two_step_template()
        };
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(report.success, "整理步不参与任务成败");
        assert_eq!(report.steps.len(), 3, "两个模板步 + 一个整理步");
        let resolve = &report.steps[2];
        assert_eq!(resolve.result_reference, "#RESULT");
        assert_eq!(resolve.index, 3);
        assert_eq!(resolve.status, StepStatus::Done);
        assert_eq!(
            report.result.as_deref(),
            Some("文件末尾已新增一行（index=8）"),
            "最终结果 = 整理步的输出"
        );

        // 第三次请求是整理步：system 换专用 prompt、契约渲染成实际值、交付步输出带上了
        let requests = ai.requests();
        assert_eq!(requests.len(), 3);
        let messages = serde_json::to_string(&requests[2].messages).unwrap();
        assert!(
            messages.contains("结果整理助手"),
            "应换整理专用 system prompt: {messages}"
        );
        assert!(
            messages.contains("文件末尾新增一行即视为成功"),
            "契约应进请求: {messages}"
        );
        assert!(
            messages.contains("第二步产出"),
            "交付步输出应进请求: {messages}"
        );
        assert!(
            messages.contains("向 a.txt 末尾追加一行"),
            "${{path}} 应被渲染: {messages}"
        );
        assert!(requests[2].tools.is_none(), "整理步不应带工具");

        // 整理步也要有自己的事件，UI 的 pipeline 才能显示它
        let events = sink.events();
        assert!(events.iter().any(|event| matches!(
            event,
            PlanRunEvent::StepStarted { index: 3, intent } if intent.contains("整理")
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, PlanRunEvent::StepFinished { index: 3, .. })));
    }

    /// 没有契约（用户跳过输出定义）→ 不追加整理步，结果退化为交付步原文，也不多花一次 LLM 调用。
    #[tokio::test]
    async fn missing_contract_falls_back_to_deliverable_output() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
        ]);
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert_eq!(report.steps.len(), 2, "无契约不应追加整理步");
        assert_eq!(report.result.as_deref(), Some("第二步产出"));
        assert_eq!(ai.requests().len(), 2, "无契约不应多一次 LLM 调用");
    }

    /// `expected_output` 里的 `${name}` 必须展开后才进 prompt；
    /// 而执行记录里仍保留**模板原文**（UI 展示用）。
    #[tokio::test]
    async fn expands_expected_output_in_prompt_but_keeps_raw_in_record() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
        ]);
        let template = FlexiblePlanTemplate {
            steps: vec![
                PlanStep {
                    result_reference: "#E1".to_string(),
                    intent: "读取 ${path}".to_string(),
                    // 占位符写在 expected_output 里 —— 它必须展开后才能发给模型
                    expected_output: "${path} 的末尾新增一行".to_string(),
                    dependencies: vec![],
                },
                PlanStep {
                    result_reference: "#E2".to_string(),
                    intent: "收尾".to_string(),
                    expected_output: "完成".to_string(),
                    dependencies: vec!["#E1".to_string()],
                },
            ],
            ..two_step_template()
        };
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        let first = serde_json::to_string(&ai.requests()[0].messages).unwrap();
        assert!(
            first.contains("a.txt 的末尾新增一行"),
            "期望产出应展开为实际值: {first}"
        );
        assert!(
            !first.contains("${path}"),
            "不得把未展开的占位符发给模型: {first}"
        );

        assert_eq!(
            report.steps[0].expected_output, "${path} 的末尾新增一行",
            "执行记录仍应是模板原文"
        );
    }

    /// `expected_output` 的占位符缺值时该步失败（与 `intent` 同一路径），
    /// 且**在发起 LLM 调用之前**就失败。
    #[tokio::test]
    async fn missing_param_in_expected_output_fails_step_before_llm() {
        let ai = FakeAiClient::new(vec![]);
        let template = FlexiblePlanTemplate {
            output_schema: None,
            task: "维护文件".to_string(),
            inputs: vec![PlanInput {
                name: "no_default".to_string(),
                default: None,
                description: None,
            }],
            steps: vec![PlanStep {
                result_reference: "#E1".to_string(),
                intent: "做事".to_string(),
                expected_output: "写入 ${no_default}".to_string(),
                dependencies: vec![],
            }],
        };
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(!report.success);
        assert_eq!(report.steps[0].status, StepStatus::Failed);
        assert!(
            report.steps[0]
                .error
                .as_deref()
                .unwrap_or("")
                .contains("expected_output"),
            "失败原因应点名 expected_output: {:?}",
            report.steps[0].error
        );
        assert!(ai.requests().is_empty(), "展开失败不该发起 LLM 调用");
    }

    /// 依赖校验：引用必须**已在本步之前出现**，一条规则覆盖不存在 / 自依赖 / 依赖后续步。
    #[test]
    fn dependency_issues_cover_missing_self_and_forward_refs() {
        let template = FlexiblePlanTemplate {
            output_schema: None,
            task: "t".to_string(),
            inputs: vec![],
            steps: vec![
                PlanStep {
                    result_reference: "#E1".to_string(),
                    intent: "一".to_string(),
                    expected_output: "a".to_string(),
                    dependencies: vec!["#E2".to_string()], // 依赖后面的步骤
                },
                PlanStep {
                    result_reference: "#E2".to_string(),
                    intent: "二".to_string(),
                    expected_output: "b".to_string(),
                    dependencies: vec!["#E2".to_string()], // 自依赖
                },
                PlanStep {
                    result_reference: "#E3".to_string(),
                    intent: "三".to_string(),
                    expected_output: "c".to_string(),
                    dependencies: vec!["#E9".to_string()], // 引用不存在
                },
            ],
        };
        let issues = collect_dependency_issues(&template);
        assert_eq!(issues.len(), 3, "三处都该报警: {issues:?}");
        assert!(issues[0].contains("#E2"), "{}", issues[0]);
        assert!(issues[1].contains("#E2"), "{}", issues[1]);
        assert!(issues[2].contains("#E9"), "{}", issues[2]);
    }

    /// 只引用前面步骤的合法依赖不该报警。
    #[test]
    fn dependency_issues_accept_backward_refs() {
        // `two_step_template` 的 #E2 依赖 #E1（在其之前）
        assert!(collect_dependency_issues(&two_step_template()).is_empty());
    }

    /// 依赖写错只警告不阻断：执行照样跑完，只是该步拿不到 prior。
    #[tokio::test]
    async fn bad_dependency_warns_but_does_not_block() {
        let ai = FakeAiClient::new(vec![text_response("产出", 10, 1)]);
        let template = FlexiblePlanTemplate {
            output_schema: None,
            task: "t".to_string(),
            inputs: vec![],
            steps: vec![PlanStep {
                result_reference: "#E1".to_string(),
                intent: "一".to_string(),
                expected_output: "a".to_string(),
                dependencies: vec!["#E9".to_string()],
            }],
        };
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(report.success, "依赖写错不该阻断执行");
        assert_eq!(ai.requests().len(), 1, "仍应正常发起一次 LLM 调用");
    }

    /// 契约非法是数据问题：记一条 `Failed` 的整理步让原因可见，但**不**把任务判失败。
    #[tokio::test]
    async fn invalid_contract_records_failed_resolve_step_without_failing_run() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
        ]);
        let template = FlexiblePlanTemplate {
            output_schema: Some(serde_json::json!({ "kind": "success_only", "success": "s" })),
            ..two_step_template()
        };
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(report.success, "契约非法不该把任务判成失败");
        assert_eq!(report.steps.len(), 3);
        let resolve = &report.steps[2];
        assert_eq!(resolve.status, StepStatus::Failed);
        assert!(resolve
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("success_only"));
        assert!(report.result.is_none());
        assert_eq!(ai.requests().len(), 2, "契约非法时不该再调一次 LLM");
    }

    // ── C1：工具序列进报告 ──

    /// 把假工具注册进 registry。
    fn register(registry: &ToolRegistry, name: &str, tool: Arc<FakeTool>) {
        registry.register_custom_tool(
            planned_agent_core::mcp::types::Tool {
                name: name.to_string(),
                description: "测试工具".to_string(),
                input_schema: json!({"type": "object"}),
            },
            vec![ToolCategory::File],
            tool,
        );
    }

    /// C1：每步记下「调了哪些工具、什么入参、成功没有」。
    ///
    /// 在此之前报告里只有 `tool_calls`（**次数**）—— 事后想知道那几次是什么工具、
    /// 在哪一步、传了什么，只能去翻 UI 轨迹（而且那份入参已被截到 120 字符）。
    #[tokio::test]
    async fn report_records_tool_sequence_per_step() {
        let registry = Arc::new(ToolRegistry::new());
        register(
            &registry,
            "read_file",
            fake_tool("read_file", json!({"content": "甲"}), false),
        );
        register(
            &registry,
            "write_file",
            fake_tool("write_file", json!({"error": "磁盘满"}), true),
        );

        let ai = FakeAiClient::new(vec![
            // 第 1 步第 1 轮：要调两个工具（先成功、后工具层报错）
            tool_response("call-1", "read_file", json!({"path": "a.txt"}), 10, 1),
            tool_response(
                "call-2",
                "write_file",
                json!({"path": "b.txt", "content": "乙"}),
                11,
                2,
            ),
            // 第 1 步第 2 轮：给正文收敛
            text_response("第一步产出", 12, 3),
            // 第 2 步：不调工具
            text_response("第二步产出", 20, 4),
        ]);

        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let executor =
            FlexibleExecutor::new(ai as Arc<dyn AiClient>, registry, ExecutorConfig::default());

        let report = executor
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        // 第 1 步：两条记录，**按发生顺序**，工具名/入参/成败都对得上。
        let first = &report.steps[0];
        assert_eq!(first.tool_calls, 2);
        assert_eq!(first.tool_sequence.len(), 2);
        assert_eq!(first.tool_sequence[0].tool, "read_file");
        assert!(
            first.tool_sequence[0].args.contains("a.txt"),
            "入参应带路径：{:?}",
            first.tool_sequence[0].args
        );
        assert!(first.tool_sequence[0].ok);
        assert_eq!(first.tool_sequence[1].tool, "write_file");
        assert!(first.tool_sequence[1].args.contains("b.txt"));
        assert!(
            !first.tool_sequence[1].ok,
            "工具返回错误结果应记 ok=false"
        );

        // 第 2 步：没调工具 → 空序列（而不是缺字段/None）。
        assert_eq!(report.steps[1].tool_calls, 0);
        assert!(report.steps[1].tool_sequence.is_empty());
    }

    fn executor(ai: Arc<FakeAiClient>) -> FlexibleExecutor {
        FlexibleExecutor::new(
            ai as Arc<dyn AiClient>,
            Arc::new(ToolRegistry::new()),
            ExecutorConfig::default(),
        )
    }

    /// 可指定配置的执行器（落盘相关用例需要自定义 `cache_dir` / 阈值）。
    fn executor_with(ai: Arc<FakeAiClient>, cfg: ExecutorConfig) -> FlexibleExecutor {
        FlexibleExecutor::new(ai as Arc<dyn AiClient>, Arc::new(ToolRegistry::new()), cfg)
    }

    /// 任意 `AiClient` 的执行器（B1 的 fake 不是 `FakeAiClient`）。
    fn executor_any(ai: Arc<dyn AiClient>, cfg: ExecutorConfig) -> FlexibleExecutor {
        FlexibleExecutor::new(ai, Arc::new(ToolRegistry::new()), cfg)
    }

    /// 单步模板（B1 用例不需要步骤间的依赖关系）。
    fn one_step_template() -> FlexiblePlanTemplate {
        FlexiblePlanTemplate {
            output_schema: None,
            task: "单步任务".to_string(),
            inputs: vec![],
            steps: vec![PlanStep {
                result_reference: "#E1".to_string(),
                intent: "随便做点事".to_string(),
                expected_output: "做完".to_string(),
                dependencies: vec![],
            }],
        }
    }

    // ── B1：LLM 请求的超时、重试与取消 ──

    /// 永不返回的 AI：用来造「请求挂住」。
    struct HangingAi;

    #[async_trait]
    impl AiClient for HangingAi {
        async fn chat_completion(
            &self,
            _request: ChatCompletionRequest,
        ) -> anyhow::Result<ChatCompletionResponse> {
            std::future::pending::<()>().await;
            unreachable!("永不返回")
        }

        async fn chat_completion_stream(
            &self,
            _request: ChatCompletionRequest,
        ) -> anyhow::Result<ChatCompletionStream> {
            anyhow::bail!("不支持流式")
        }

        fn provider_name(&self) -> &str {
            "hanging"
        }

        fn model_name(&self) -> &str {
            "hanging-model"
        }

        fn default_config(&self) -> ChatCompletionRequest {
            ChatCompletionRequest {
                model: "hanging-model".to_string(),
                messages: vec![],
                tools: None,
                temperature: None,
                max_tokens: None,
                stream: false,
                extra: Default::default(),
            }
        }
    }

    /// 第一次调用永久挂起、之后正常：造「第一次超时、重试成功」。
    struct SlowThenOkAi {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl AiClient for SlowThenOkAi {
        async fn chat_completion(
            &self,
            _request: ChatCompletionRequest,
        ) -> anyhow::Result<ChatCompletionResponse> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                std::future::pending::<()>().await;
            }
            Ok(text_response("重试后的产出", 7, 3))
        }

        async fn chat_completion_stream(
            &self,
            _request: ChatCompletionRequest,
        ) -> anyhow::Result<ChatCompletionStream> {
            anyhow::bail!("不支持流式")
        }

        fn provider_name(&self) -> &str {
            "slow-then-ok"
        }

        fn model_name(&self) -> &str {
            "slow-then-ok-model"
        }

        fn default_config(&self) -> ChatCompletionRequest {
            ChatCompletionRequest {
                model: "slow-then-ok-model".to_string(),
                messages: vec![],
                tools: None,
                temperature: None,
                max_tokens: None,
                stream: false,
                extra: Default::default(),
            }
        }
    }

    /// B1a：LLM 请求**进行中**收到取消，必须立刻结束（而不是等请求自己返回）。
    #[tokio::test]
    async fn cancel_interrupts_in_flight_llm_request() {
        let template = one_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let (tx, rx) = watch::channel(false);

        let cancel_after_a_beat = async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = tx.send(true);
        };
        let executor = executor_any(Arc::new(HangingAi), ExecutorConfig::default());
        let run = executor.run(&template, &params, None, &sink, Some(rx));

        let (report, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(run, cancel_after_a_beat)
        })
        .await
        .expect("取消后应迅速结束 —— 请求进行中也必须能被中断（否则会话会卡死）");
        let report = report.expect("run 本身不失败（成败看 report）");

        assert!(!report.success);
        assert_eq!(report.steps[0].status, StepStatus::Failed);
        assert_eq!(
            report.steps[0].error.as_deref(),
            Some("用户取消"),
            "应记为取消而非其它失败"
        );
    }

    /// B1b：第一次请求超时 → 重试 → 成功。
    #[tokio::test]
    async fn llm_timeout_retries_and_then_succeeds() {
        let ai = Arc::new(SlowThenOkAi {
            calls: AtomicUsize::new(0),
        });
        let template = one_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let cfg = ExecutorConfig {
            llm_timeout: Some(Duration::from_millis(30)),
            llm_timeout_retries: 1,
            ..ExecutorConfig::default()
        };

        let report = executor_any(ai.clone(), cfg)
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(report.success, "第一次超时后重试应成功");
        assert_eq!(ai.calls.load(Ordering::SeqCst), 2, "应恰好两次尝试");
    }

    /// B1b：一直超时 → 用尽重试次数 → 该步失败（且**不会**永久挂住）。
    #[tokio::test]
    async fn llm_timeout_exhausts_retries_then_fails() {
        let template = one_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let cfg = ExecutorConfig {
            llm_timeout: Some(Duration::from_millis(20)),
            llm_timeout_retries: 1,
            ..ExecutorConfig::default()
        };

        let report = tokio::time::timeout(
            Duration::from_secs(5),
            executor_any(Arc::new(HangingAi), cfg).run(&template, &params, None, &sink, None),
        )
        .await
        .expect("配了超时就不该永久挂起")
        .expect("run 本身不失败（成败看 report）");

        assert!(!report.success);
        let error = report.steps[0].error.as_deref().unwrap_or_default();
        assert!(error.contains("超时"), "错误应说明超时: {error}");
        assert!(error.contains("已尝试 2 次"), "错误应体现重试次数: {error}");
    }

    /// 每个用例一个独立临时目录（并行用例互不干扰）。
    fn temp_cache_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "planned-agent-flexible-spill-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// 产出超过阈值 → 落盘；下游 `prior` 只拿到「文件说明 + 预览」，不含全文。
    #[tokio::test]
    async fn large_output_spills_to_file_and_prior_gives_path() {
        let long = "甲".repeat(9_000); // 超过默认阈值 8000
        let ai = FakeAiClient::new(vec![
            text_response(&long, 10, 1),
            text_response("第二步完成", 20, 2),
        ]);
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let cache_dir = temp_cache_dir("large");

        let report = executor_with(
            ai.clone(),
            ExecutorConfig {
                cache_dir: cache_dir.clone(),
                ..ExecutorConfig::default()
            },
        )
        .run(&template, &params, None, &sink, None)
        .await
        .expect("run 不应失败");

        assert!(report.success);
        let file = report.steps[0]
            .output_file
            .clone()
            .expect("超出阈值应落盘");
        let path = PathBuf::from(&file);
        assert!(path.starts_with(&cache_dir), "落盘应在 cache_dir 下：{file}");
        let written = std::fs::read_to_string(&path).expect("落盘文件应可读");
        assert!(
            written == long,
            "落盘内容应与产出逐字一致（写入 {} 字符，期望 {}）",
            written.chars().count(),
            long.chars().count()
        );

        let second = serde_json::to_string(&ai.requests()[1].messages).unwrap();
        assert!(second.contains("已存为临时文件"), "应给出文件说明: {second}");
        // JSON 里反斜杠会被转义，所以先按 JSON 片段转一次再比。
        let path_in_json = serde_json::to_string(&path.display().to_string())
            .unwrap()
            .trim_matches('"')
            .to_string();
        assert!(
            second.contains(&path_in_json),
            "应给出落盘路径: {second}"
        );
        assert!(!second.contains(&long), "不该把全文塞进下游 prompt");

        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    /// 落盘失败 → 该步 `Failed`（不静默降级）。
    ///
    /// 制造失败：`cache_dir` 被一个**同名文件**占住 → `create_dir_all` 必然失败。
    #[tokio::test]
    async fn spill_failure_marks_step_failed_and_skips_rest() {
        let long = "乙".repeat(9_000);
        let ai = FakeAiClient::new(vec![
            text_response(&long, 10, 1),
            text_response("第二步不该被调用", 20, 2),
        ]);
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let blocker = temp_cache_dir("blocked");
        std::fs::write(&blocker, b"x").expect("写占位文件");

        let report = executor_with(
            ai.clone(),
            ExecutorConfig {
                cache_dir: blocker.clone(),
                ..ExecutorConfig::default()
            },
        )
        .run(&template, &params, None, &sink, None)
        .await
        .expect("run 本身不失败（成败看 report）");

        assert!(!report.success);
        assert_eq!(report.steps[0].status, StepStatus::Failed);
        assert!(
            report.steps[0]
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("产出落盘失败"),
            "错误应说明落盘失败: {:?}",
            report.steps[0].error
        );
        assert_eq!(report.steps[1].status, StepStatus::Skipped);
        assert_eq!(ai.requests().len(), 1, "第一步失败后不该再调 LLM");

        let _ = std::fs::remove_file(&blocker);
    }

    /// 阈值边界是「含等号」的：等于阈值仍内联，多一个字符就落盘。
    #[tokio::test]
    async fn spill_threshold_boundary_is_inclusive() {
        let cache_dir = temp_cache_dir("boundary");
        let cfg = ExecutorConfig {
            cache_dir: cache_dir.clone(),
            spill_threshold_chars: 100,
            ..ExecutorConfig::default()
        };
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let exact = "丙".repeat(100);
        let ai = FakeAiClient::new(vec![text_response(&exact, 1, 1), text_response("完", 1, 1)]);
        let report = executor_with(ai, cfg.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");
        assert!(report.steps[0].output_file.is_none(), "等于阈值应内联");

        let over = "丙".repeat(101);
        let ai = FakeAiClient::new(vec![text_response(&over, 1, 1), text_response("完", 1, 1)]);
        let report = executor_with(ai, cfg)
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");
        assert!(report.steps[0].output_file.is_some(), "超过阈值应落盘");

        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    /// 交付产出落盘后，**整理步**收到的同样只是「文件说明 + 读取方式」，不是全文。
    #[tokio::test]
    async fn resolve_step_prior_points_at_spilled_file() {
        let long = "丁".repeat(9_000);
        let ai = FakeAiClient::new(vec![
            text_response(&long, 1, 1),
            text_response("整理完成", 1, 1),
        ]);
        let template = FlexiblePlanTemplate {
            output_schema: Some(serde_json::json!({
                "kind": "bool",
                "goal": "整理",
                "success": "完成"
            })),
            task: "单步".to_string(),
            inputs: vec![],
            steps: vec![PlanStep {
                result_reference: "#E1".to_string(),
                intent: "产出大结果".to_string(),
                expected_output: "结果".to_string(),
                dependencies: vec![],
            }],
        };
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let cache_dir = temp_cache_dir("resolve");

        let report = executor_with(
            ai.clone(),
            ExecutorConfig {
                cache_dir: cache_dir.clone(),
                ..ExecutorConfig::default()
            },
        )
        .run(&template, &params, None, &sink, None)
        .await
        .expect("run 不应失败");

        assert!(report.success);
        assert_eq!(ai.requests().len(), 2, "一次模板步 + 一次整理步");
        let resolve_req = serde_json::to_string(&ai.requests()[1].messages).unwrap();
        assert!(
            resolve_req.contains("已存为临时文件"),
            "整理步应收到文件说明: {resolve_req}"
        );
        assert!(
            resolve_req.contains("builtin_read_file_lines"),
            "文件说明应给出读取方式: {resolve_req}"
        );
        // 落盘文案必须同时列出两个读回工具（按需二选一，不是固定两步）
        // —— 它与 prompt.rs 第 3 条是同一套读法的两处落点，改一处必须同步另一处。
        assert!(
            resolve_req.contains("builtin_grep_file"),
            "文件说明应给出搜索定位方式: {resolve_req}"
        );
        assert!(!resolve_req.contains(&long), "整理步不该收到全文");

        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    /// 日志用的产出渲染：多行压单行、超长封顶并标出原长。
    #[test]
    fn log_output_flattens_and_caps() {
        assert_eq!(log_output("  a\n\nb\t c  "), "a b c");
        assert_eq!(log_output(""), "");

        let long = "x".repeat(LOG_OUTPUT_MAX_CHARS + 5);
        let rendered = log_output(&long);
        assert!(rendered.starts_with(&"x".repeat(LOG_OUTPUT_MAX_CHARS)));
        assert!(
            rendered.contains(&format!("共 {} 字符", LOG_OUTPUT_MAX_CHARS + 5)),
            "截断时应标出原长: {rendered}"
        );

        let exact = "y".repeat(LOG_OUTPUT_MAX_CHARS);
        assert_eq!(log_output(&exact), exact, "恰好等于上限不截断");
    }

    #[tokio::test]
    async fn passes_prior_output_and_expands_params() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
        ]);
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(report.success);
        assert_eq!(report.steps_done(), 2);
        assert_eq!(report.prompt_tokens, 30);
        assert_eq!(report.completion_tokens, 3);
        assert_eq!(report.tool_calls, 0);

        // 两个请求：第一个含展开后的参数（${path}），第二个含 #E1 的实际产出
        let requests = ai.requests();
        assert_eq!(requests.len(), 2);
        let first = serde_json::to_string(&requests[0].messages).unwrap();
        assert!(
            first.contains("a.txt"),
            "第一步 intent 应展开 ${{path}}: {first}"
        );
        let second = serde_json::to_string(&requests[1].messages).unwrap();
        assert!(
            second.contains("第一步产出"),
            "第二步应收到 #E1 的产出: {second}"
        );

        let events = sink.events();
        assert!(events
            .iter()
            .any(|event| matches!(event, PlanRunEvent::RunStarted { total_steps: 2 })));
        assert!(events
            .iter()
            .any(|event| matches!(event, PlanRunEvent::RunFinished { .. })));
    }

    #[tokio::test]
    async fn failure_marks_downstream_steps_skipped() {
        // 只给一个响应：第二步需要的响应缺失 → 第一步成功、但整体因缺响应而失败
        let ai = FakeAiClient::new(vec![text_response("只有第一步", 10, 1)]);
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(!report.success, "第二步 LLM 失败应使整体失败");
        assert_eq!(report.steps[0].status, StepStatus::Done);
        assert_eq!(report.steps[1].status, StepStatus::Failed);
        assert_eq!(ai.requests().len(), 2);
    }

    #[tokio::test]
    async fn missing_param_marks_step_failed_and_skips_rest() {
        let template = two_step_template();
        // 空参数表：${path} 无值 → 第一步展开失败
        let params = PlanRunParams::new();
        let sink = RecordingSink::default();
        let ai = FakeAiClient::new(vec![text_response("不该被调用", 1, 1)]);

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(!report.success);
        assert_eq!(report.steps[0].status, StepStatus::Failed);
        assert_eq!(report.steps[1].status, StepStatus::Skipped);
        assert!(ai.requests().is_empty(), "展开失败时不应发起 LLM 请求");
    }

    #[tokio::test]
    async fn cancelled_before_start_skips_everything() {
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let ai = FakeAiClient::new(vec![text_response("不该被调用", 1, 1)]);

        let (tx, rx) = watch::channel(true);
        drop(tx);

        let report = executor(ai.clone())
            .run(&template, &params, None, &sink, Some(rx))
            .await
            .expect("run 不应失败");

        assert!(!report.success);
        assert!(report
            .steps
            .iter()
            .all(|step| step.status == StepStatus::Skipped));
        assert!(ai.requests().is_empty());
    }
