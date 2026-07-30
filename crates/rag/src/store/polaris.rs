//! PolarisDB 向量存储实现
//!
//! 基于 PolarisDB（纯 Rust 嵌入式向量库）实现 TraceStore trait。
//! 数据持久化到本地文件系统，进程内运行。

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use polarisdb::prelude::*;
use polarisdb::{AsyncCollection, SearchResult};
use tokio::sync::RwLock;
use tracing::info;

use super::{ScoredEntry, SearchFilters, StoreEntry, TraceStore};

/// 载荷字段名：业务 ID（对应 StoreEntry.id）
const FIELD_ENTRY_ID: &str = "_entry_id";
/// 载荷字段名：检索文本
const FIELD_TEXT: &str = "_text";
/// 载荷字段名：JSON 序列化的 metadata
const FIELD_META: &str = "_meta";
/// 载荷字段名：JSON 序列化的 labels
const FIELD_LABELS: &str = "_labels";

/// PolarisDB 存储实现
///
/// ```ignore
/// let store = PolarisDbStore::open("./traces/vectors", 1024).await?;
/// store.add(entry).await?;
/// let results = store.search(&query_vec, 5, &filters).await?;
/// ```
pub struct PolarisDbStore {
    collection: AsyncCollection,
    dim: usize,
    /// 业务 ID → PolarisDB 内部 ID 映射（用于删除）
    id_map: RwLock<HashMap<String, u64>>,
}

impl PolarisDbStore {
    /// 打开或创建 PolarisDB 存储
    ///
    /// - `path`: 数据目录路径
    /// - `dim`: 向量维度（由 Embedder 决定）
    pub async fn open(path: &str, dim: usize) -> Result<Self> {
        info!("打开 PolarisDB 存储: path={}, dim={}", path, dim);

        let config = CollectionConfig::new(dim, DistanceMetric::Cosine);
        let collection = AsyncCollection::open_or_create(path.to_string(), config)
            .await
            .map_err(|e| anyhow!("打开 PolarisDB 失败 ({}): {}", path, e))?;

        Ok(Self {
            collection,
            dim,
            id_map: RwLock::new(HashMap::new()),
        })
    }

    /// 将 StoreEntry 转为 PolarisDB Payload
    fn entry_to_payload(entry: &StoreEntry) -> Result<Payload> {
        let meta_json = serde_json::to_string(&entry.metadata)
            .unwrap_or_else(|_| "{}".to_string());
        let labels_json = serde_json::to_string(&entry.labels)
            .unwrap_or_else(|_| "{}".to_string());

        Ok(Payload::new()
            .with_field(FIELD_ENTRY_ID, entry.id.as_str())
            .with_field(FIELD_TEXT, entry.text.as_str())
            .with_field(FIELD_META, meta_json.as_str())
            .with_field(FIELD_LABELS, labels_json.as_str()))
    }

    /// 从 PolarisDB SearchResult 还原 StoreEntry
    fn result_to_entry(result: &SearchResult) -> Result<StoreEntry> {
        let payload = result
            .payload
            .as_ref()
            .ok_or_else(|| anyhow!("搜索结果缺少 payload"))?;

        let id = payload.get_str(FIELD_ENTRY_ID).unwrap_or("").to_string();
        let text = payload.get_str(FIELD_TEXT).unwrap_or("").to_string();
        let meta_str = payload.get_str(FIELD_META).unwrap_or("{}");
        let labels_str = payload.get_str(FIELD_LABELS).unwrap_or("{}");

        let metadata: serde_json::Value =
            serde_json::from_str(meta_str).unwrap_or(serde_json::Value::Null);
        let labels: HashMap<String, String> =
            serde_json::from_str(labels_str).unwrap_or_default();

        Ok(StoreEntry {
            id,
            text,
            embedding: Vec::new(),
            metadata,
            labels,
        })
    }

    /// cosine distance → similarity score (0~1)
    fn distance_to_score(distance: f32) -> f32 {
        (1.0 - distance).max(0.0)
    }
}

#[async_trait]
impl TraceStore for PolarisDbStore {
    async fn add(&self, entry: StoreEntry) -> Result<()> {
        if entry.embedding.len() != self.dim {
            return Err(anyhow!(
                "向量维度不匹配: 期望 {} 实际 {} (id={})",
                self.dim,
                entry.embedding.len(),
                entry.id
            ));
        }

        let vector: Vec<f32> = entry.embedding.clone();
        let payload = Self::entry_to_payload(&entry)?;
        let entry_id = entry.id.clone();

        let internal_id = self
            .collection
            .insert_auto(vector, payload)
            .await
            .map_err(|e| anyhow!("PolarisDB 写入失败 (id={}): {}", entry_id, e))?;

        self.id_map.write().await.insert(entry_id, internal_id);
        Ok(())
    }

    async fn add_batch(&self, entries: Vec<StoreEntry>) -> Result<()> {
        for entry in entries {
            self.add(entry).await?;
        }
        Ok(())
    }

    async fn search(
        &self,
        embedding: &[f32],
        top_k: usize,
        filters: &SearchFilters,
    ) -> Result<Vec<ScoredEntry>> {
        if top_k == 0 {
            return Ok(Vec::new());
        }

        let query: Vec<f32> = embedding.to_vec();

        // 向量检索（不带 payload 过滤，Phase 2 先做后过滤）
        let results = self
            .collection
            .search(&query, top_k, None)
            .await;

        let mut scored: Vec<ScoredEntry> = Vec::with_capacity(results.len());
        for result in &results {
            let entry = Self::result_to_entry(result)?;
            let score = Self::distance_to_score(result.distance);

            // 相似度门槛
            if let Some(threshold) = filters.threshold {
                if score < threshold {
                    continue;
                }
            }

            // 标签精确匹配（后过滤）
            if !filters.labels.is_empty() {
                let all_match = filters
                    .labels
                    .iter()
                    .all(|(k, v)| entry.labels.get(k).map_or(false, |lv| lv == v));
                if !all_match {
                    continue;
                }
            }

            scored.push(ScoredEntry { entry, score });
        }

        // 按分数降序排列
        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);

        Ok(scored)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let internal_id = self
            .id_map
            .read()
            .await
            .get(id)
            .copied()
            .ok_or_else(|| anyhow!("未找到记录 (id={})", id))?;

        self.collection
            .delete(internal_id)
            .await
            .map_err(|e| anyhow!("PolarisDB 删除失败 (id={}): {}", id, e))?;

        self.id_map.write().await.remove(id);
        Ok(())
    }

    async fn count(&self) -> Result<usize> {
        Ok(self.collection.len())
    }
}
