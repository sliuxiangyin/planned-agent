//! OpenAI 兼容 Embedding API 实现
//!
//! 支持所有 OpenAI Embeddings API 兼容的提供商（OpenAI、DeepSeek、SiliconFlow 等）。
//! 通过 reqwest 直接调用 HTTP API，不依赖 async-openai crate。

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::Embedder;

/// OpenAI Embedding API 请求
#[derive(Debug, Serialize)]
struct EmbeddingRequest {
    model: String,
    input: String,
}

/// OpenAI Embedding API 响应
#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

/// OpenAI 兼容 Embedding 实现
pub struct OpenAiEmbedder {
    /// HTTP 客户端
    client: reqwest::Client,
    /// API 端点 URL（如 https://api.openai.com/v1/embeddings）
    endpoint: String,
    /// API Key
    api_key: String,
    /// 模型名称
    model: String,
    /// 向量维度（首次调用后缓存）
    dim: std::sync::OnceLock<usize>,
}

impl OpenAiEmbedder {
    /// 创建新的 OpenAI Embedder
    ///
    /// `base_url` 应包含 /v1 前缀但不含 /embeddings 后缀，
    /// 如 `https://api.openai.com/v1` 或 `https://api.siliconflow.cn/v1`
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Result<Self> {
        let endpoint = format!("{}/embeddings", base_url.trim_end_matches('/'));

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| anyhow!("创建 HTTP 客户端失败: {}", e))?;

        info!("OpenAI Embedder 初始化: endpoint={}, model={}", endpoint, model);

        Ok(Self {
            client,
            endpoint,
            api_key: api_key.to_string(),
            model: model.to_string(),
            dim: std::sync::OnceLock::new(),
        })
    }

    async fn call_api(&self, text: &str) -> Result<Vec<f32>> {
        let request = EmbeddingRequest {
            model: self.model.clone(),
            input: text.to_string(),
        };

        let response = self
            .client
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow!("Embedding API 请求失败: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Embedding API 返回错误 ({}): {}",
                status,
                body
            ));
        }

        let embedding_response: EmbeddingResponse = response
            .json()
            .await
            .map_err(|e| anyhow!("解析 Embedding API 响应失败: {}", e))?;

        let embedding = embedding_response
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

        Ok(embedding)
    }
}

#[async_trait]
impl Embedder for OpenAiEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // 重试机制（最多 3 次）
        let mut last_error = None;
        for attempt in 1..=3 {
            match self.call_api(text).await {
                Ok(embedding) => return Ok(embedding),
                Err(e) => {
                    warn!(
                        "Embedding API 调用失败 (第 {} 次): {}",
                        attempt, e
                    );
                    last_error = Some(e);
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
        // OnceLock 初始化时返回 0，首次 embed() 调用后自动填充
        self.dim.get().copied().unwrap_or(0)
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}
