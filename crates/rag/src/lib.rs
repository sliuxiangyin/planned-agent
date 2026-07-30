//! RAG（Retrieval-Augmented Generation）独立模块
//!
//! 提供 Embedding 生成、向量存储、语义检索的统一抽象。
//! 所有 trait/type 自包含，不耦合业务层的 ExecutionTrace / ReAct 等概念。
//!
//! # 分层
//!
//! ```text
//! planned-agent  ──→  rag::Embedder / rag::Retriever / rag::TraceStore
//! rag            ──→  自包含（零外部业务依赖）
//! ```

pub mod embedder;
pub mod retriever;
pub mod store;

// 重新导出常用类型，方便外部调用
pub use crate::store::polaris::PolarisDbStore;
