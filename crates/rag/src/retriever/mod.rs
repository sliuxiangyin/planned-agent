//! 检索编排层
//!
//! 将 Embedder + TraceStore 组合为统一的检索入口，
//! 处理：文本→向量→检索→后过滤→排序。

pub mod types;

use std::sync::Arc;

use anyhow::Result;
use tracing::info;

use crate::embedder::Embedder;
use crate::store::{ScoredEntry, SearchFilters, TraceStore};

pub use types::{RetrievalConfig, SearchResult};

/// 检索器
///
/// 组合 Embedder 和 TraceStore，提供一站式语义检索。
///
/// 使用方式：
/// ```ignore
/// let retriever = Retriever::new(embedder, store, config);
/// let results = retriever.search("在腾讯新闻搜索AI新闻", 5, &filters).await?;
/// ```
pub struct Retriever {
    embedder: Arc<dyn Embedder>,
    store: Arc<dyn TraceStore>,
    config: RetrievalConfig,
}

impl Retriever {
    /// 创建检索器
    pub fn new(
        embedder: Arc<dyn Embedder>,
        store: Arc<dyn TraceStore>,
        config: RetrievalConfig,
    ) -> Self {
        info!(
            "Retriever 初始化: embedder={} dim={}, top_k={}, threshold={}",
            embedder.model_name(),
            embedder.dim(),
            config.top_k,
            config.threshold
        );
        Self { embedder, store, config }
    }

    /// 获取 Embedder 引用（供外部使用，如预先生成并缓存向量）
    pub fn embedder(&self) -> &Arc<dyn Embedder> {
        &self.embedder
    }

    /// 获取 Store 引用（供外部直接操作存储）
    pub fn store(&self) -> &Arc<dyn TraceStore> {
        &self.store
    }

    /// 语义检索
    ///
    /// `query_text` → Embedder → 向量检索 → 后过滤 → 排序
    pub async fn search(
        &self,
        query_text: &str,
        filters: &SearchFilters,
    ) -> Result<Vec<ScoredEntry>> {
        // Step 1: 文本 → 向量
        let embedding = self.embedder.embed(query_text).await?;

        // Step 2: 向量检索（合并配置中的 threshold）
        let mut merged_filters = filters.clone();
        if merged_filters.threshold.is_none() {
            merged_filters.threshold = Some(self.config.threshold);
        }

        let results = self
            .store
            .search(&embedding, self.config.top_k, &merged_filters)
            .await?;

        Ok(results)
    }

    /// 检索并格式化为 Prompt 可注入文本
    ///
    /// 用于直接注入 System Prompt 的场景。
    pub async fn search_for_prompt(
        &self,
        query_text: &str,
        filters: &SearchFilters,
    ) -> Result<String> {
        let results = self.search(query_text, filters).await?;

        if results.is_empty() {
            return Ok(String::new());
        }

        let mut prompt = String::from("## 参考模板（已验证的操作流程）\n\n");
        prompt.push_str("以下是历史中类似任务的执行轨迹，你可以参考其流程提高效率。\n");
        prompt.push_str("注意：模板中的 {{变量}} 需要根据当前任务替换为实际值。\n\n");

        for (i, result) in results.iter().enumerate() {
            prompt.push_str(&format!(
                "### 模板 {}（相似度 {:.2}）\n",
                i + 1,
                result.score
            ));
            prompt.push_str(&format!("意图：{}\n", result.entry.text));

            // 解析 metadata 中的 actions（如果有）
            if let Some(actions) = result.entry.metadata.get("actions") {
                if let Some(actions_arr) = actions.as_array() {
                    prompt.push_str("步骤：\n");
                    for (j, action) in actions_arr.iter().enumerate() {
                        let desc = action
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("-");
                        let tool = action
                            .get("tool_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        prompt.push_str(&format!(
                            "  {}. {} ({})\n",
                            j + 1,
                            desc,
                            tool
                        ));
                    }
                }
            }
            prompt.push('\n');
        }

        Ok(prompt)
    }

    /// 获取存储中的记录总数
    pub async fn count(&self) -> Result<usize> {
        self.store.count().await
    }
}
