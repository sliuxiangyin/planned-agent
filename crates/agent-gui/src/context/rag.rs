//! RAG 模块 GUI 封装层
//!
//! 将 planned-agent-rag 的异步组件聚合为 RagContext，
//! 供 Dioxus 组件通过 Context 消费。

use std::sync::Arc;

use planned_agent_rag::embedder::{Embedder, EmbedderFactory, EmbedderProvider};
use planned_agent_rag::retriever::{RetrievalConfig, Retriever};
use planned_agent_rag::store::TraceStore;
use planned_agent_rag::PolarisDbStore;

use crate::config::RagConfig;

/// GUI 层 RAG 上下文（聚合 Embedder + Store + Retriever）
///
/// 组件通过 `use_context::<Resource<Option<Arc<RagContext>>>>()` 获取，
/// 再调用 `ctx.retriever.search(...)` 进行语义检索。
pub struct RagContext {
    /// 检索器（一站式：文本→向量→存储查询→排序）
    pub retriever: Arc<Retriever>,
}

impl RagContext {
    /// 从 RagConfig 异步初始化 RAG 组件
    ///
    /// 流程：打开向量存储 → 创建 Embedder → 组合为 Retriever
    pub async fn init(config: &RagConfig) -> anyhow::Result<Self> {
        // 若无 API Key，跳过初始化
        if config.embedding_api_key.is_empty() {
            anyhow::bail!("RAG embedding_api_key 未配置，跳过初始化");
        }

        // 1. 打开 PolarisDB 向量存储（bge-m3 默认维度 1024）
        let store = PolarisDbStore::open(&config.store.path, 1024).await?;
        let store: Arc<dyn TraceStore> = Arc::new(store);
        tracing::info!("RAG 向量存储已打开: {}", config.store.path);

        // 2. 创建 Embedder（OpenAI 兼容 API）
        let provider = EmbedderProvider::OpenAI {
            base_url: config.embedding_base_url.clone(),
            api_key: config.embedding_api_key.clone(),
            model: config.embedding_model.clone(),
        };
        let embedder: Box<dyn Embedder> = EmbedderFactory::create(&provider)?;
        let embedder: Arc<dyn Embedder> = Arc::from(embedder);
        tracing::info!(
            "RAG Embedder 已创建: model={}, dim={}",
            embedder.model_name(),
            embedder.dim()
        );

        // 3. 组装 Retriever
        let retrieval_config = RetrievalConfig {
            top_k: config.retrieval.top_k,
            threshold: config.retrieval.similarity_threshold,
        };
        let retriever = Arc::new(Retriever::new(embedder, store, retrieval_config));

        let count = retriever.count().await.unwrap_or(0);
        tracing::info!("RAG 初始化完成: {} 条历史记录", count);

        Ok(Self { retriever })
    }
}