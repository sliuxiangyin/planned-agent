mod agent;
mod cli;
mod config;
mod planner;
use agent::Agent;
use anyhow::Result;
use cli::Cli;
use config::AppConfig;
use planned_agent_core::prompt::{PromptContext, PromptManager};
use planned_agent_core::types::PlanContext;
use planned_agent_core::planner::coarse::CoarsePlanner;
use planner::coarse::LlmCoarsePlanner;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{error, info};
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
        .with_env_filter(
            // 默认 info 级；显式屏蔽 scraper/selectors（否则 chunk_html 会刷屏 DEBUG）
            // 想看全部 debug 时：RUST_LOG=debug cargo run ...
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,selectors=warn,scraper=warn")),
        )
        .with_writer(std::io::stdout.and(file_writer))
        .init();

    // 解析命令行参数
    let cli = Cli::parse();

    // 加载配置
    let config = match AppConfig::load() {
        Ok(config) => cli.merge_with_config(config),
        Err(_) => {
            info!("Using default configuration");
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
                run_test_execute(&agent, &input).await?;
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
async fn run_test_execute(agent: &Agent, input: &str) -> Result<()> {
    use planner::coarse::LlmCoarsePlanner;
    use planner::react::DefaultReActAgent;
    use planned_agent_core::planner::coarse::CoarsePlanner;
    use planned_agent_core::planner::react::{ReActAgent, ReActAgentConfig};
    
    println!("=== Plan-and-Execute 测试 ===\n");
    println!("用户输入: {}\n", input);
    
    // 获取工具注册表
    let tool_registry = agent.get_tool_registry();
    
    // 1. 创建粗粒度计划器
    println!("--- 步骤1: 粗粒度计划器 ---");
    let ai_manager = agent.get_ai_manager()
        .ok_or_else(|| anyhow::anyhow!("AI管理器未初始化"))?;
    let ai_client = ai_manager.default()?;
    let prompt_manager = agent.get_prompt_manager_arc()
        .ok_or_else(|| anyhow::anyhow!("提示管理器未初始化"))?;
    
    let coarse_planner = LlmCoarsePlanner::new(
        ai_client.clone(),
        prompt_manager.clone(),
    );
    
    // 生成粗粒度计划
    let mut plan_context = PlanContext {
        user_id: None,
        session_id: None,
        history: Vec::new(),
        metadata: std::collections::HashMap::new(),
    };
    plan_context.metadata.insert("user_goal".to_string(), serde_json::json!(input));
    plan_context.metadata.insert("max_steps".to_string(), serde_json::json!(5));
    plan_context.metadata.insert("available_tools".to_string(), 
        serde_json::json!(tool_registry.get_all_tools().iter().map(|t| t.name.clone()).collect::<Vec<_>>()));
    
    let coarse_plan = match coarse_planner.generate_coarse_plan(input, &plan_context).await {
        Ok(plan) => {
            println!("生成粗粒度计划:");
            println!("  计划ID: {}", plan.id);
            println!("  步骤数量: {}", plan.steps.len());
            for (i, step) in plan.steps.iter().enumerate() {
                let categories = step.recommended_tool_categories.as_ref()
                    .map(|cats| cats.iter().map(|c| format!("{:?}", c)).collect::<Vec<_>>().join(", "))
                    .unwrap_or_else(|| "未知".to_string());
                println!("  步骤{}: {} (工具分类: {})", i + 1, step.intent, categories);
            }
            println!();
            plan
        }
        Err(e) => {
            error!("粗粒度计划生成失败: {}", e);
            return Err(e);
        }
    };
    
    // 2. 创建 ReAct Agent 并执行每个步骤
    println!("--- 步骤2: ReAct Agent 执行 ---");
    let react_config = ReActAgentConfig {
        max_iterations: 5,
        step_timeout_ms: 30000,
        enable_chain_of_thought: true,
        max_retries: 3,
        retry_delay_ms: 1000,
    };
    
    let react_agent = DefaultReActAgent::new(
        ai_client.clone(),
        prompt_manager.clone(),
        tool_registry.clone(),
        react_config,
    );
    
    println!("ReAct Agent 已创建: {}", react_agent.name());
    println!("开始执行 {} 个步骤...\n", coarse_plan.steps.len());
    
    // 存储前序步骤的结果，用于传递给后续步骤
    // 存储前序步骤的原始工具输出，用于传递给后续步骤
    let mut previous_outputs: Vec<serde_json::Value> = Vec::new();
    
    // 执行每个粗粒度步骤
    for (i, step) in coarse_plan.steps.iter().enumerate() {
        println!("--- 执行步骤 {}/{}: {} ---", i + 1, coarse_plan.steps.len(), step.intent);
        
        // 将前序步骤的原始输出添加到上下文中
        let mut step_context = plan_context.clone();
        if !previous_outputs.is_empty() {
            step_context.metadata.insert(
                "previous_outputs".to_string(),
                serde_json::json!(previous_outputs.iter().enumerate().map(|(idx, output)| {
                    serde_json::json!({
                        "step": idx + 1,
                        "output": output
                    })
                }).collect::<Vec<_>>())
            );
        }
        
        // 将后续步骤信息添加到上下文中
        let remaining_steps: Vec<_> = coarse_plan.steps.iter().skip(i + 1).collect();
        step_context.metadata.insert(
            "remaining_steps".to_string(),
            serde_json::json!(remaining_steps)
        );
        
        match react_agent.execute_coarse_step(step, &step_context).await {
            Ok(result) => {
                if result.success {
                    println!("  ✓ 成功 (迭代次数: {}, 耗时: {}ms)", result.iterations, result.total_duration_ms);
                    println!("  输出: {}", serde_json::to_string_pretty(&result.output).unwrap_or_default());
                    
                    // 保存原始输出，供后续步骤使用
                    previous_outputs.push(result.output.clone());
                    
                    // 打印执行历史
                    if !result.history.is_empty() {
                        println!("  执行历史:");
                        for (j, step) in result.history.iter().enumerate() {
                            println!("    第{}轮:", j + 1);
                            println!("      思考: {}", step.thought.reasoning);
                            println!("      行动: {}({})", step.action.tool_name, step.action.parameters);
                            println!("      观察: {}", step.observation.output);
                        }
                    }
                } else {
                    println!("  ✗ 失败: {}", result.error.unwrap_or_else(|| "未知错误".to_string()));
                }
            }
            Err(e) => {
                println!("  ✗ 执行错误: {}", e);
            }
        }
        println!();
    }
    
    println!("=== 测试完成 ===");
    
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
                            
                            // 2. 获取页面快照
                            println!("--- Step 2: Take Snapshot ---");
                            let snap_args = serde_json::json!({
                                "verbose": verbose
                            });
                            
                            match tool_registry.call_tool("browser_snapshot", snap_args).await {
                                Ok(snap_result) => {
                                    println!("Snapshot result:");
                                    println!("  is_error: {}", snap_result.result.is_error);
                                    println!("  call_id: {}", snap_result.result.call_id);
                                    println!("  content: {}", serde_json::to_string_pretty(&snap_result.result.content).unwrap_or_else(|_| snap_result.result.content.to_string()));
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
