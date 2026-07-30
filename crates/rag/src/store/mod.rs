//! 向量存储层
//!
//! 定义存储抽象 trait，不耦合业务数据结构。
//! 业务层将 ExecutionTrace 转为 StoreEntry 后存入。

pub mod polaris;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 存入向量存储的通用条目
///
/// 设计原则：不依赖 ExecutionTrace 等业务类型，
/// 上游负责将业务数据序列化到 metadata / labels 中。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreEntry {
    /// 唯一标识（对应 ExecutionTrace.id）
    pub id: String,
    /// 用于生成向量的源文本（对应 generalized_intent）
    pub text: String,
    /// 向量（由 Embedder 生成）
    #[serde(skip)]
    pub embedding: Vec<f32>,
    /// 任意 JSON 元数据（业务层可塞入整个 ExecutionTrace）
    pub metadata: serde_json::Value,
    /// 标量过滤标签（如 upstream_intent、categories）
    pub labels: HashMap<String, String>,
}

/// 检索结果（含相似度分数）
#[derive(Debug, Clone)]
pub struct ScoredEntry {
    /// 匹配的条目
    pub entry: StoreEntry,
    /// 相似度分数（cosine 距离转换，0~1，越高越相似）
    pub score: f32,
}

/// 检索过滤条件
#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    /// 精确匹配标签（如 upstream_intent = "打开腾讯新闻首页"）
    pub labels: HashMap<String, String>,
    /// 相似度最低门槛（低于此分数的结果被丢弃）
    pub threshold: Option<f32>,
}

/// 向量存储 trait
///
/// 实现者可选择 LanceDB（当前）、Qdrant（后续）等后端。
#[async_trait]
pub trait TraceStore: Send + Sync {
    /// 添加单条记录
    async fn add(&self, entry: StoreEntry) -> Result<()>;

    /// 批量添加记录
    async fn add_batch(&self, entries: Vec<StoreEntry>) -> Result<()> {
        for entry in entries {
            self.add(entry).await?;
        }
        Ok(())
    }

    /// 向量语义检索
    ///
    /// - `embedding`: 查询向量
    /// - `top_k`: 返回数量
    /// - `filters`: 过滤条件（可选）
    async fn search(
        &self,
        embedding: &[f32],
        top_k: usize,
        filters: &SearchFilters,
    ) -> Result<Vec<ScoredEntry>>;

    /// 按 ID 删除记录
    async fn delete(&self, id: &str) -> Result<()>;

    /// 存储中的记录总数
    async fn count(&self) -> Result<usize>;
}
