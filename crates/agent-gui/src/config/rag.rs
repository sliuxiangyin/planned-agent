//! RAG 向量检索配置（`[rag]` / `[rag.store]` / `[rag.retrieval]`）。

use serde::{Deserialize, Serialize};

/// RAG 向量检索完整配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagConfig {
    /// Embedding 提供商: "openai"
    #[serde(default = "default_embedding_provider")]
    pub embedding_provider: String,

    /// Embedding 模型名称
    #[serde(default)]
    pub embedding_model: String,

    /// Embedding API 基础 URL
    #[serde(default)]
    pub embedding_base_url: String,

    /// Embedding API Key
    #[serde(default)]
    pub embedding_api_key: String,

    /// 向量存储配置
    #[serde(default)]
    pub store: RagStoreConfig,

    /// 检索配置
    #[serde(default)]
    pub retrieval: RagRetrievalConfig,
}

fn default_embedding_provider() -> String {
    "openai".to_string()
}

impl Default for RagConfig {
    fn default() -> Self {
        Self {
            embedding_provider: default_embedding_provider(),
            embedding_model: String::new(),
            embedding_base_url: String::new(),
            embedding_api_key: String::new(),
            store: RagStoreConfig::default(),
            retrieval: RagRetrievalConfig::default(),
        }
    }
}

/// 向量存储配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagStoreConfig {
    /// 向量存储路径 —— 默认是 `cache_root` 下的**单段名** `vector_store`
    /// （原默认 `./traces/vector_store` 已改为 `./data/vector_store`，且不迁移旧数据）。
    #[serde(default = "default_rag_store_path")]
    pub path: String,
}

fn default_rag_store_path() -> String {
    "vector_store".to_string()
}

impl Default for RagStoreConfig {
    fn default() -> Self {
        Self {
            path: default_rag_store_path(),
        }
    }
}

/// 检索参数配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagRetrievalConfig {
    /// 默认返回数量
    #[serde(default = "default_top_k")]
    pub top_k: usize,

    /// 相似度门槛 (0~1)
    #[serde(default = "default_similarity_threshold")]
    pub similarity_threshold: f32,
}

fn default_top_k() -> usize {
    5
}

fn default_similarity_threshold() -> f32 {
    0.7
}

impl Default for RagRetrievalConfig {
    fn default() -> Self {
        Self {
            top_k: default_top_k(),
            similarity_threshold: default_similarity_threshold(),
        }
    }
}
