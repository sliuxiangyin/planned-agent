//! 检索层类型定义

/// 检索配置
#[derive(Debug, Clone)]
pub struct RetrievalConfig {
    /// 默认返回数量
    pub top_k: usize,
    /// 默认相似度门槛（0~1），低于此值的结果被丢弃
    pub threshold: f32,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            top_k: 5,
            threshold: 0.7,
        }
    }
}

/// 检索结果（高层封装）
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// 匹配文本（generalized_intent）
    pub text: String,
    /// 相似度分数（0~1）
    pub score: f32,
    /// 完整元数据
    pub metadata: serde_json::Value,
    /// 条目 ID
    pub id: String,
}
