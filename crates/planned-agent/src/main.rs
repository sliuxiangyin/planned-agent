mod agent;
mod cli;
mod config;
use agent::Agent;
use anyhow::Result;
use cli::Cli;
use config::AppConfig;
use planned_agent::planner::{
    coarse::LlmCoarsePlanner,
    react::{PlanAndExecuteAgent, PlanAndExecuteConfig},
    trace::{recorder::TraceRecorderConfig, TraceRecorder},
};
use planned_agent_core::prompt::{PromptContext, PromptManager};
use planned_agent_core::types::PlanContext;
use planned_agent_core::planner::coarse::CoarsePlanner;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, error, info};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志：同时输出到 stdout 和文件（logs/agent.log.YYYY-MM-DD，按天轮转）
    let log_dir = "logs";
    let _ = std::fs::create_dir_all(log_dir);
    let file_appender = tracing_appender::rolling::daily(log_dir, "agent.log");
    let (file_writer, _file_guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_env_filter({
            // 默认 debug 级别；强制屏蔽 scraper/html5ever 的 DEBUG 刷屏
            let mut filter = EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("debug"));
            filter = filter
                .add_directive("html5ever=warn".parse().unwrap())
                .add_directive("markup5ever=warn".parse().unwrap())
                .add_directive("scraper=warn".parse().unwrap())
                .add_directive("selectors=warn".parse().unwrap());
            filter
        })
        .with_writer(std::io::stdout.and(file_writer))
        .init();

    // 解析命令行参数
    let cli = Cli::parse();

    // 加载配置
    let config = match AppConfig::load(String::from("/home/code/planned-agent/crates/agent-gui/config.toml")) {
        Ok(config) => {
            info!("已从 {} 加载配置", cli.config);
            cli.merge_with_config(config)
        }
        Err(e) => {
            info!(
                "无法从 {} 加载配置 ({}), 使用默认配置",
                cli.config, e
            );
            cli.merge_with_config(AppConfig::default_config())
        }
    };

    // 创建代理
    let mut agent = Agent::new();

    // 初始化 AI 客户端
    if let Err(e) = agent.init_ai_clients(config.ai_providers.clone()) {
        error!("Failed to initialize AI clients: {}", e);
        return Err(e);
    }

    // 初始化 Prompt 管理器
    if let Err(e) = agent
        .init_prompt_manager(config.prompt_manager.clone())
        .await
    {
        error!("Failed to initialize prompt manager: {}", e);
        // 继续运行，但不使用prompt管理器
    }

    // 连接到 MCP 服务器
    info!("Connecting to MCP servers...");
    if let Err(e) = agent.connect_mcp_servers(config.mcp_servers.clone()).await {
        error!("Failed to connect to MCP servers: {}", e);
        // 继续运行，但不使用工具
    }

    // // 显示连接状态
    // info!("AI providers: {:?}", agent.get_ai_provider_names());
    // info!("MCP servers: {:?}", agent.get_mcp_server_names());
    // info!("Tool Registry status:\n{}", agent.get_tool_registry_status());
    // info!("Available tools: {:?}", agent.get_available_tools());

    // 处理子命令
    if let Some(command) = cli.command {
        match command {
            cli::Commands::TestExecute { input } => {
                // 运行完整的 Plan-and-Execute 测试流程
                run_test_execute(&agent, &config, &input).await?;
            }
        }
    } else if cli.interactive {
        // 交互模式
        run_interactive_mode(&mut agent, cli.stream).await?;
    } else {
        // 单次查询模式
        let input = "Hello, how can you help me?";
        if cli.stream {
            let stream_result = agent.process_input_stream(input).await?;
            println!("Response: {}", stream_result.content);
        } else {
            let response = agent.process_input(input).await?;
            println!("Response: {}", response);
        };
    }

    Ok(())
}

/// 运行完整的 Plan-and-Execute 测试流程
async fn run_test_execute(agent: &Agent, app_config: &AppConfig, input: &str) -> Result<()> {
    use std::sync::Arc;
    use planned_agent_core::ai::AiClient;
    use planned_agent_core::planner::react::ReActAgentConfig;
    
    println!("=== Plan-and-Execute 测试 ===\n");
    println!("用户输入: {}\n", input);
    
    // 获取工具注册表
    let tool_registry = agent.get_tool_registry();
    let exec_ctx = agent.get_exec_ctx();
    
    // 获取 AI 管理器 和 Prompt 管理器
    let ai_manager = agent.get_ai_manager()
        .ok_or_else(|| anyhow::anyhow!("AI管理器未初始化"))?;
    let ai_client: Arc<dyn AiClient> = ai_manager.default()?;
    let prompt_manager = agent.get_prompt_manager_arc()
        .ok_or_else(|| anyhow::anyhow!("提示管理器未初始化"))?;
    
    // 构建计划上下文
    let mut plan_context = PlanContext {
        user_id: None,
        session_id: None,
        history: Vec::new(),
        metadata: std::collections::HashMap::new(),
    };
    plan_context.metadata.insert("max_steps".to_string(), serde_json::json!(5));
    plan_context.metadata.insert("available_tools".to_string(), 
        serde_json::json!(tool_registry.get_all_tools().iter().map(|t| t.name.clone()).collect::<Vec<_>>()));
    
    
    // 配置 Plan-And-Execute Agent
    let config = PlanAndExecuteConfig {
        react_config: ReActAgentConfig {
            max_iterations: 15,
            step_timeout_ms: 130000,
            max_retries: 3,
            retry_delay_ms: 1000,
        },
    };
    
    // 创建 TraceRecorder（轨迹泛化 + JSON 存储）
    let trace_model = if app_config.trace.generalization_model.is_empty() {
        None
    } else {
        Some(app_config.trace.generalization_model.clone())
    };
    let trace_config = TraceRecorderConfig {
        enabled: app_config.trace.enabled,
        storage_dir: std::path::PathBuf::from(&app_config.trace.storage_dir),
        max_iterations_for_record: app_config.trace.max_iterations_for_record,
        use_llm_generalization: app_config.trace.use_llm_generalization,
        trace_model,
    };
    let trace_recorder = TraceRecorder::new(
        trace_config,
        Some(Arc::new(ai_manager.clone())),
        Some(prompt_manager.clone()),
    );

    // 创建并执行
    let mut pae_agent = PlanAndExecuteAgent::new(
        ai_client.clone(),
        prompt_manager.clone(),
        tool_registry.clone(),
        exec_ctx.clone(),
        config,
        trace_recorder,
    );
    
    let result = pae_agent.execute(input, &plan_context).await?;
    
    // 打印计划
    println!("\n{} 阶段1: 粗粒度计划 {}", "─".repeat(4), "─".repeat(40));
    println!("计划: {} | {} 步骤 | 复杂度: {:?}",
        result.coarse_plan.title,
        result.coarse_plan.steps.len(),
        result.coarse_plan.complexity);
    if !result.coarse_plan.description.is_empty() {
        println!("描述: {}", result.coarse_plan.description);
    }
    println!();
    for step in &result.coarse_plan.steps {
        let cats = step.recommended_tool_categories.as_ref()
            .map(|c| c.iter().map(|x| format!("{:?}", x)).collect::<Vec<_>>().join(","))
            .unwrap_or_else(|| "—".to_string());
        let deps = if step.dependencies.is_empty() {
            String::new()
        } else {
            format!("  \u{21B3} {}", step.dependencies.join(", "))
        };
        println!("  {:>3}  {:<40} \u{2192} {}  [{}]{}",
            format!("#{}", step.order), step.intent, step.result_reference, cats, deps);
    }

    // 打印执行结果
    println!("\n{} 阶段2: 逐步执行 {}", "─".repeat(4), "─".repeat(40));
    let mut success_count = 0u32;
    let mut fail_count = 0u32;

    for step in &result.coarse_plan.steps {
        let ref_id = &step.result_reference;
        let is_last = Some(step.id.as_str()) == result.coarse_plan.steps.last().map(|s| s.id.as_str());
        println!();
        println!("  {}  {}", ref_id, step.intent);

        match result.step_results.get(ref_id) {
            Some(sr) if sr.success => {
                success_count += 1;
                // 提取工具名
                let tools: Vec<&str> = sr.history.iter()
                    .map(|h| h.action.tool_name.as_str())
                    .collect();
                let tool_str = if tools.is_empty() {
                    "(无工具调用)".to_string()
                } else {
                    tools.join(" \u{2192} ")
                };
                println!("  \u{2705} {}次迭代 {}ms", sr.iterations, sr.duration_ms);
                println!("  工具: {}", tool_str);
                let out = serde_json::to_string(&sr.output).unwrap_or_default();
                if is_last {
                    println!("  输出({}B):\n{}", out.len(), out);
                } else {
                    let preview: String = out.chars().take(200).collect();
                    let suffix = if out.chars().count() > 200 { "..." } else { "" };
                    println!("  输出({}B): {}{}", out.len(), preview, suffix);
                }
            }
            Some(sr) => {
                fail_count += 1;
                let err = sr.error.as_deref().unwrap_or("未知错误");
                println!("  \u{274C} {}次迭代 {}ms  错误: {}", sr.iterations, sr.duration_ms, err);
                if !sr.history.is_empty() {
                    println!("  执行历史:");
                    for (i, h) in sr.history.iter().enumerate() {
                        // 简化参数显示
                        let params_str = serde_json::to_string(&h.action.parameters).unwrap_or_default();
                        let params_short = if params_str.len() > 60 {
                            format!("{}...", &params_str[..57])
                        } else {
                            params_str
                        };
                        // 输出大小
                        let out_str = serde_json::to_string(&h.observation.output).unwrap_or_default();
                        let size = out_str.len();
                        let err_mark = if h.observation.error.is_some() { " \u{26A0}" } else { "" };
                        println!("    {}\u{20E3} {}({})\u{2192}{}B{}",
                            i + 1, h.action.tool_name, params_short, size, err_mark);
                    }
                    // 最后一条的 observe 结论
                    if let Some(last) = sr.history.last() {
                        if !last.thought.reasoning.is_empty() {
                            println!("    \u{1F4AD} observe: {}", last.thought.reasoning);
                        }
                    }
                }
            }
            None => {
                fail_count += 1;
                println!("  \u{2753} 未找到结果");
            }
        }
    }

    // 总结
    println!();
    println!("{}", "─".repeat(56));
    let status = if result.success { "\u{2705} 成功" } else { "\u{274C} 失败" };
    println!("  {} | \u{2705} {} / \u{274C} {} | {}ms",
        status, success_count, fail_count, result.total_duration_ms);
    println!("{}", "─".repeat(56));
    
    Ok(())
}

/// 从LLM响应中提取的结构化信息
#[derive(Debug, Deserialize, Serialize)]
struct ExtractedInfo {
    entities: Vec<String>,
    summary: String,
    sentiment: String,
}

/// 交互模式
async fn run_interactive_mode(agent: &mut Agent, use_stream: bool) -> Result<()> {
    println!("Planned Agent Interactive Mode");
    println!("Type 'quit' or 'exit' to exit");
    println!("Type 'status' to see connection status");
    println!("Type 'tools' to see available tools");
    println!("Type 'providers' to see AI providers");
    println!("Type 'servers' to see MCP servers");
    println!("Type 'test-prompt' to test prompt extraction");
    println!("Type 'prompts' to list available prompts");
    println!("Type 'test-coarse' 测试颗粒度计划");
    println!("Type 'browse <url>' or 'browse <url> --verbose' 打开网页并获取快照");
    println!("Type 'snapshot' 查看浏览命令帮助");
    println!("Type 'clean-snapshot <yaml_text>' 清洗 YAML 快照");
    println!("----------------------------------------");

    loop {
        print!("> ");
        use std::io::{self, Write};
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        match input.to_lowercase().as_str() {
            "quit" | "exit" => {
                println!("Goodbye!");
                break;
            }
            "status" => {
                println!(
                    "Tool Registry status:\n{}",
                    agent.get_tool_registry_status()
                );
            }
            "tools" => {
                let tools = agent.get_available_tools_with_categories();
                if tools.is_empty() {
                    println!("No tools available");
                } else {
                    println!("Available tools:");
                    for (name, categories) in tools {
                        if categories.is_empty() {
                            println!("  - {}", name);
                        } else {
                            println!("  - {} [{}]", name, categories.join(", "));
                        }
                    }
                }
            }
            "providers" => {
                let providers = agent.get_ai_provider_names();
                if providers.is_empty() {
                    println!("No AI providers configured");
                } else {
                    println!("AI providers:");
                    for provider in providers {
                        println!("  - {}", provider);
                    }
                }
            }
            "servers" => {
                let servers = agent.get_mcp_server_names();
                if servers.is_empty() {
                    println!("No MCP servers configured");
                } else {
                    println!("MCP servers:");
                    for server in servers {
                        println!("  - {}", server);
                    }
                }
            }
            "prompts" => {
                if let Some(prompt_manager) = agent.get_prompt_manager() {
                    match prompt_manager.list_prompts().await {
                        Ok(prompts) => {
                            if prompts.is_empty() {
                                println!("No prompts available");
                            } else {
                                println!("Available prompts:");
                                for prompt in &prompts {
                                    println!(
                                        "  - {} (has schema: {})",
                                        prompt.name, prompt.has_output_schema
                                    );
                                }
                            }
                        }
                        Err(e) => println!("Error listing prompts: {}", e),
                    }
                } else {
                    println!("Prompt manager not initialized");
                }
            }
            "test-prompt" => {
                println!("\n=== Testing Prompt: analysis/extract_info ===");
                
                // 写死测试文本
                let test_text = "张三在北京的公司工作，他是一名软件工程师。";
                println!("Test text: {}", test_text);
                
                // 1. 渲染prompt模板
                let rendered_prompt = if let Some(prompt_manager) = agent.get_prompt_manager() {
                    let context = PromptContext::new()
                        .with_variable("text", json!(test_text));
                    
                    match prompt_manager.render("analysis/extract_info", &context).await {
                        Ok(prompt) => {
                            println!("\n=== Rendered Prompt ===");
                            println!("{}", prompt);
                            println!("=======================\n");
                            Some(prompt)
                        }
                        Err(e) => {
                            println!("Failed to render prompt: {}", e);
                            None
                        }
                    }
                } else {
                    println!("Prompt manager not initialized");
                    None
                };
                
                if let Some(prompt) = rendered_prompt {
                    // 2. 调用LLM
                    println!("Calling LLM...");
                    match agent.process_input(&prompt).await {
                        Ok(response) => {
                            println!("\n=== LLM Response ===");
                            println!("{}", response);
                            println!("====================\n");
                            
                            // 3. 验证响应
                            if let Some(prompt_manager) = agent.get_prompt_manager() {
                                match prompt_manager.validate_response("analysis/extract_info", &response).await {
                                    Ok(is_valid) => {
                                        println!("Response valid: {}", is_valid);
                                        
                                        if is_valid {
                                            // 4. 解析为结构化类型
                                            match prompt_manager.parse_response::<ExtractedInfo>("analysis/extract_info", &response).await {
                                                Ok(extracted) => {
                                                    println!("\n=== Extracted Info (Rust Type) ===");
                                                    println!("Entities: {:?}", extracted.entities);
                                                    println!("Summary: {}", extracted.summary);
                                                    println!("Sentiment: {}", extracted.sentiment);
                                                    println!("==================================\n");
                                                }
                                                Err(e) => println!("Failed to parse response: {}", e),
                                            }
                                        }
                                    }
                                    Err(e) => println!("Failed to validate response: {}", e),
                                }
                            }
                        }
                        Err(e) => println!("LLM call failed: {}", e),
                    }
                }
            }
            "test-coarse"=>{
                println!("\n=== Testing Coarse Plan Generation ===");

                // 获取 AI 管理器和 Prompt Manager
                let ai_manager = match agent.get_ai_manager() {
                    Some(manager) => manager,
                    None => {
                        println!("Error: AI manager not initialized");
                        continue;
                    }
                };

                let ai_client = match ai_manager.default() {
                    Ok(client) => client,
                    Err(e) => {
                        println!("Error: {}", e);
                        continue;
                    }
                };

                let prompt_manager = match agent.get_prompt_manager() {
                    Some(pm) => pm.clone(),
                    None => {
                        println!("Error: Prompt manager not initialized");
                        continue;
                    }
                };

                // 创建粗粒度计划器
                let planner = LlmCoarsePlanner::new(
                    ai_client,
                    std::sync::Arc::new(prompt_manager),
                );

                // 构建计划上下文
                let context = PlanContext {
                    user_id: Some("interactive-user".to_string()),
                    session_id: Some("test-session".to_string()),
                    history: vec![],
                    metadata: std::collections::HashMap::new(),
                };

                // 测试输入
                let test_input = "读取 /home/code/planned-agent目录下的所有文件并按大小排序提取前三个";
                println!("Input: {}", test_input);

                // 生成粗粒度计划
                match planner.generate_coarse_plan(test_input, &context).await {
                    Ok(plan) => {
                        println!("\n=== Generated Coarse Plan ===");
                        // 打印完整的 CoarseGrainedPlan（JSON 格式）
                        match serde_json::to_string_pretty(&plan) {
                            Ok(json) => println!("{}", json),
                            Err(e) => println!("Error serializing plan: {}", e),
                        }
                        println!("===============================\n");
                    }
                    Err(e) => {
                        println!("Error generating coarse plan: {}", e);
                    }
                }
            }
            "browse" | "snapshot" => {
                println!("\n=== Browser Snapshot Mode ===");
                println!("Usage: browse <url> or browse <url> --verbose");
                println!("Example: browse https://www.example.com");
                println!("         browse https://www.example.com --verbose");
                println!("================================\n");
            }
            _ => {
                // 检查是否是 browse 命令（支持带参数的形式）
                let input_lower = input.to_lowercase();
                if input_lower.starts_with("browse ") || input_lower.starts_with("snapshot ") {
                    // 解析命令参数
                    let parts: Vec<&str> = input.split_whitespace().collect();
                    if parts.len() < 2 {
                        println!("Error: Please provide a URL");
                        println!("Usage: browse <url> [--verbose]");
                        continue;
                    }
                    
                    let url = parts[1];
                    let verbose = parts.iter().any(|&p| p == "--verbose" || p == "-v");
                    
                    println!("Opening URL: {}", url);
                    println!("Verbose mode: {}", verbose);
                    println!();
                    
                    // 获取工具注册表
                    let tool_registry = agent.get_tool_registry();
                    
                    // 1. 先导航到指定 URL
                    println!("--- Step 1: Navigate to URL ---");
                    let nav_args = serde_json::json!({
                        "type": "url",
                        "url": url
                    });
                    
                    match tool_registry.call_tool("browser_navigate", nav_args).await {
                        Ok(nav_result) => {
                            println!("Navigate result:");
                            println!("  is_error: {}", nav_result.result.is_error);
                            println!("  call_id: {}", nav_result.result.call_id);
                            println!("  content: {}", serde_json::to_string_pretty(&nav_result.result.content).unwrap_or_else(|_| nav_result.result.content.to_string()));
                            println!("  categories: {:?}", nav_result.categories);
                            println!();
                            
                            // 等待页面加载完成 (使用 browser_wait_for 工具)
                            println!("--- Waiting for page to load ---");
                            let wait_args = serde_json::json!({
                                "text": "body"
                            });
                            if let Err(e) = tool_registry.call_tool("browser_wait_for", wait_args).await {
                                println!("Warning: wait_for failed (non-critical): {}", e);
                            } else {
                                println!("Page loaded successfully");
                            }
                            
                            // 2. 通过 JS 获取页面 HTML
                            println!("--- Step 2: Get Page HTML via JS ---");
                            let html_args = serde_json::json!({
                                "function": "() => document.documentElement.outerHTML"
                            });
                            
                            match tool_registry.call_tool("browser_evaluate", html_args).await {
                                Ok(snap_result) => {
                                    println!("Snapshot result:");
                                    println!("  is_error: {}", snap_result.result.is_error);
                                    println!("  call_id: {}", snap_result.result.call_id);
                                    info!("  content: {:?}", serde_json::to_string_pretty(&snap_result.result.content).unwrap_or_else(|_| snap_result.result.content.to_string()));
                                    println!("  categories: {:?}", snap_result.categories);
                                    println!();
                                    
                                    // 打印完整结果（格式化输出）
                                    println!("=== Full Results ===");
                                    println!("\n--- Navigate Result (JSON) ---");
                                    println!("{}", serde_json::to_string_pretty(&nav_result).unwrap_or_default());
                                    
                                    println!("\n--- Snapshot Result (JSON) ---");
                                    println!("{}", serde_json::to_string_pretty(&snap_result).unwrap_or_default());
                                    println!("===================\n");
                                }
                                Err(e) => {
                                    println!("Error taking snapshot: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            println!("Error navigating to URL: {}", e);
                        }
                    }
                } else if use_stream {
                    let stream_result = agent.process_input_stream(input).await?;
                    println!("Response: {}", stream_result.content);
                } else {
                    let response = agent.process_input(input).await?;
                    println!("Response: {}", response);
                };
            }
        }
    }

    Ok(())
}
