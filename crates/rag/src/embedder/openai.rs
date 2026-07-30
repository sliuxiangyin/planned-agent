//! OpenAI 兼容 Embedding API 实现
//!
//! 基于 async-openai 库（纯第三方，无业务耦合）。
//! 支持所有 OpenAI Embeddings API 兼容的提供商（OpenAI、SiliconFlow 等）。

use anyhow::{anyhow, Result};
use async_openai::config::OpenAIConfig;
use async_openai::types::CreateEmbeddingRequestArgs;
use async_openai::Client;
use async_trait::async_trait;
use tracing::{info, warn};

use super::Embedder;

/// OpenAI 兼容 Embedding 实现
pub struct OpenAiEmbedder {
    client: Client<OpenAIConfig>,
    model: String,
    /// 向量维度（首次调用后缓存）
    dim: std::sync::OnceLock<usize>,
}

impl OpenAiEmbedder {
    /// 创建新的 OpenAI Embedder
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Result<Self> {
        let config = OpenAIConfig::new()
            .with_api_key(api_key)
            .with_api_base(base_url);

        let client = Client::with_config(config);

        info!(
            "OpenAI Embedder 初始化: base_url={}, model={}",
            base_url, model
        );

        Ok(Self {
            client,
            model: model.to_string(),
            dim: std::sync::OnceLock::new(),
        })
    }
}

#[async_trait]
impl Embedder for OpenAiEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let request = CreateEmbeddingRequestArgs::default()
            .model(&self.model)
            .input(text)
            .build()
            .map_err(|e| anyhow!("构建 Embedding 请求失败: {}", e))?;

        // async-openai 自带重试（可配置），这里再包一层兜底
        let mut last_error = None;
        for attempt in 1..=3 {
            match self.client.embeddings().create(request.clone()).await {
                Ok(response) => {
                    let embedding = response
                        .data
                        .into_iter()
                        .next()
                        .ok_or_else(|| anyhow!("Embedding API 返回空数据"))?
                        .embedding;

                    // 首次调用时缓存维度
                    if self.dim.get().is_none() {
                        let _ = self.dim.set(embedding.len());
                        info!("OpenAI Embedder 维度探测: {}", embedding.len());
                    }

                    return Ok(embedding);
                }
                Err(e) => {
                    warn!("Embedding API 调用失败 (第 {} 次): {}", attempt, e);
                    last_error = Some(anyhow!("{}", e));
                    if attempt < 3 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("未知错误")))
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    fn dim(&self) -> usize {
        self.dim.get().copied().unwrap_or(0)
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}
