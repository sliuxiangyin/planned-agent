use futures::StreamExt;
use planned_agent_core::ai::types::ChatCompletionChunk;
use anyhow::Result;

/// 流式响应处理器
pub struct StreamHandler;

/// 流式响应结果
#[derive(Debug, Clone)]
pub struct StreamResult {
    /// 最终回答内容
    pub content: String,
    /// 思考内容（思考模式下的思维链）
    pub reasoning_content: String,
}

impl StreamHandler {
    /// 处理流式响应并收集所有内容（包括思考内容）
    pub async fn collect_stream(
        mut stream: Box<dyn futures::Stream<Item = Result<ChatCompletionChunk>> + Send + Unpin>,
    ) -> Result<StreamResult> {
        let mut content = String::new();
        let mut reasoning_content = String::new();
        
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => {
                    for choice in &chunk.choices {
                        // 处理思考内容
                        if let Some(reasoning) = &choice.delta.reasoning_content {
                            reasoning_content.push_str(reasoning);
                            // 思考内容可以特殊显示，例如用灰色或斜体
                            print!("\x1b[90m{}\x1b[0m", reasoning);
                        }
                        // 处理最终回答内容
                        if let Some(chunk_content) = &choice.delta.content {
                            content.push_str(chunk_content);
                            print!("{}", chunk_content);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Stream error: {}", e);
                    break;
                }
            }
        }
        
        println!(); // 换行
        Ok(StreamResult {
            content,
            reasoning_content,
        })
    }
    
    /// 处理流式响应并实时回调
    pub async fn process_stream_with_callback<F>(
        mut stream: Box<dyn futures::Stream<Item = Result<ChatCompletionChunk>> + Send + Unpin>,
        mut callback: F,
    ) -> Result<StreamResult>
    where
        F: FnMut(&str, bool) -> Result<()>, // (content, is_reasoning)
    {
        let mut content = String::new();
        let mut reasoning_content = String::new();
        
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => {
                    for choice in &chunk.choices {
                        // 处理思考内容
                        if let Some(reasoning) = &choice.delta.reasoning_content {
                            reasoning_content.push_str(reasoning);
                            callback(reasoning, true)?;
                        }
                        // 处理最终回答内容
                        if let Some(chunk_content) = &choice.delta.content {
                            content.push_str(chunk_content);
                            callback(chunk_content, false)?;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Stream error: {}", e);
                    break;
                }
            }
        }
        
        Ok(StreamResult {
            content,
            reasoning_content,
        })
    }
}
