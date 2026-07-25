use planned_agent_core::ai::AiClient;
use planned_agent_core::types::{AiRequest, AiConfig};
use planned_agent_ai_openai::OpenAiClient;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // 创建配置
    let config = AiConfig {
        provider: "openai".to_string(),
        api_key: std::env::var("OPENAI_API_KEY")?,
        model: "gpt-4".to_string(),
        max_tokens: Some(1000),
        temperature: Some(0.7),
        base_url: None,
    };
    
    // 创建客户端
    let client = OpenAiClient::new(config);
    
    // 创建请求
    let request = AiRequest {
        content: "Tell me a short story about a robot learning to code.".to_string(),
        tools: None,
        context: None,
    };
    
    println!("Sending streaming request...");
    
    // 发送流式请求
    let stream_response = client.stream(request).await?;
    
    // 处理流式响应
    let content = planned_agent_ai_openai::StreamHandler::collect_stream(stream_response.stream).await?;
    
    println!("\n\nFull response:");
    println!("{}", content);
    
    Ok(())
}
