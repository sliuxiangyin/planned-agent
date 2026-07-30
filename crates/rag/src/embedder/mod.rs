//! Embedder 模块
//!
//! 定义 Embedder trait 及工厂模式，支持运行时切换后端。

pub mod openai;

use anyhow::Result;
use async_trait::async_trait;

use openai::OpenAiEmbedder;

// ═══════════════════════════════════════════════════════════
// Embedder trait
// ═══════════════════════════════════════════════════════════

/// Embedding 生成器 trait
///
/// 将任意文本转为固定维度稠密向量，用于语义检索。
/// 实现者可选择 OpenAI Embeddings API、ONNX Runtime 本地模型等。
#[async_trait]
pub trait Embedder: Send + Sync {
    /// 将单条文本转为向量
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// 批量嵌入（默认逐条调用，实现者可覆盖以利用批量 API 加速）
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    /// 向量维度（如 OpenAI text-embedding-3-small = 1536，bge-m3 = 1024）
    fn dim(&self) -> usize;

    /// 模型标识名称
    fn model_name(&self) -> &str;
}

// ═══════════════════════════════════════════════════════════
// EmbedderFactory
// ═══════════════════════════════════════════════════════════

/// Embedding 提供商枚举
#[derive(Debug, Clone)]
pub enum EmbedderProvider {
    /// OpenAI 兼容 API（含 DeepSeek、SiliconFlow 等）
    OpenAI {
        /// API 基础 URL（如 https://api.openai.com/v1）
        base_url: String,
        /// API Key
        api_key: String,
        /// 模型名称（如 text-embedding-3-small、BAAI/bge-m3）
        model: String,
    },
}

/// Embedder 工厂
///
/// 使用方式：
/// ```ignore
/// let provider = EmbedderProvider::OpenAI { ... };
/// let embedder = EmbedderFactory::create(&provider)?;
/// ```
pub struct EmbedderFactory;

impl EmbedderFactory {
    /// 根据 provider 配置创建 Embedder 实例
    pub fn create(provider: &EmbedderProvider) -> Result<Box<dyn Embedder>> {
        match provider {
            EmbedderProvider::OpenAI {
                base_url,
                api_key,
                model,
            } => Ok(Box::new(OpenAiEmbedder::new(base_url, api_key, model)?)),
        }
    }
}
