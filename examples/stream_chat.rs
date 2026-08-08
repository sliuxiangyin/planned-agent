use planned_agent_core::ai::types::{
    ChatCompletionRequest, Message, MessageContent, MessageRole,
};
use planned_agent_core::ai::AiClient;
use planned_agent_ai_openai::{OpenAiClient, OpenAiClientConfig, StreamHandler};
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // 创建配置
    let config = OpenAiClientConfig {
        api_key: std::env::var("OPENAI_API_KEY")?,
        model: "gpt-4".to_string(),
        base_url: None,
        default_temperature: Some(0.7),
        default_max_tokens: Some(1000),
        organization: None,
        thinking_config: None,
    };

    // 创建客户端
    let client = OpenAiClient::new(config);

    // 创建流式请求
    let request = ChatCompletionRequest {
        model: "gpt-4".to_string(),
        messages: vec![Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text {
                text: "Tell me a short story about a robot learning to code.".to_string(),
            }),
            ..Default::default()
        }],
        tools: None,
        temperature: Some(0.7),
        max_tokens: Some(1000),
        stream: true,
        extra: Default::default(),
    };

    println!("Sending streaming request...");

    // 发送流式请求
    let stream_response = client.chat_completion_stream(request).await?;

    // 处理流式响应（边接收边打印）
    let result = StreamHandler::collect_stream(stream_response.stream).await?;

    println!("\n\nFull response:");
    println!("{}", result.content);

    Ok(())
}
