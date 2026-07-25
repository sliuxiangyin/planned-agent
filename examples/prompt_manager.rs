use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::json;

use planned_agent_core::prompt::{PromptManager, PromptContext};
use planned_agent_prompt_manager::{FilePromptManager, PromptManagerConfig};

#[derive(Debug, Deserialize, Serialize)]
struct ExtractedInfo {
    entities: Vec<String>,
    summary: String,
    sentiment: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    tracing_subscriber::fmt::init();
    
    // 创建Prompt管理器配置
    let config = PromptManagerConfig {
        prompt_dir: PathBuf::from("./prompts"),
        ..Default::default()
    };
    
    // 创建并初始化Prompt管理器
    let manager = FilePromptManager::new(config)?;
    manager.initialize().await?;
    
    // 列出所有可用的prompt
    println!("Available prompts:");
    let prompts = manager.list_prompts().await?;
    for prompt in &prompts {
        println!("  - {} (has schema: {})", prompt.name, prompt.has_output_schema);
    }
    
    // 示例1：渲染普通prompt
    println!("\n=== Example 1: Render simple prompt ===");
    let context = PromptContext::new()
        .with_variable("user_name", json!("张三"))
        .with_variable("context", json!("这是一个测试"));
    
    let rendered = manager.render("chat/system", &context).await?;
    println!("Rendered prompt:\n{}", rendered);
    
    // 示例2：渲染带变量的prompt
    println!("\n=== Example 2: Render prompt with variables ===");
    let context = PromptContext::new()
        .with_variable("question", json!("什么是Rust编程语言？"));
    
    let rendered = manager.render("chat/user", &context).await?;
    println!("Rendered prompt:\n{}", rendered);
    
    // 示例3：渲染带输出约束的prompt
    println!("\n=== Example 3: Render prompt with output schema ===");
    let context = PromptContext::new()
        .with_variable("text", json!("张三在北京的公司工作，他是一名软件工程师。"));
    
    let rendered = manager.render("analysis/extract_info", &context).await?;
    println!("Rendered prompt:\n{}", rendered);
    
    // 示例4：检查prompt是否存在
    println!("\n=== Example 4: Check if prompt exists ===");
    println!("chat/system exists: {}", manager.exists("chat/system").await?);
    println!("nonexistent/prompt exists: {}", manager.exists("nonexistent/prompt").await?);
    
    // 示例5：获取输出schema
    println!("\n=== Example 5: Get output schema ===");
    let schema = manager.get_output_schema("analysis/extract_info").await?;
    println!("Output schema: {:?}", schema);
    
    // 示例6：验证和解析LLM响应
    println!("\n=== Example 6: Validate and parse LLM response ===");
    let llm_response = r#"
    {
        "entities": ["张三", "北京", "公司"],
        "summary": "张三在北京的公司工作",
        "sentiment": "neutral"
    }
    "#;
    
    // 验证响应
    let is_valid = manager.validate_response("analysis/extract_info", llm_response).await?;
    println!("Response is valid: {}", is_valid);
    
    // 解析响应
    if is_valid {
        let extracted: ExtractedInfo = manager.parse_response("analysis/extract_info", llm_response).await?;
        println!("Extracted info: {:?}", extracted);
    }
    
    println!("\nAll examples completed successfully!");
    Ok(())
}
