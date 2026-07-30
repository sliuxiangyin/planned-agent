//! 分片视图定义 + 序列化。
//!
//! `ChunkedView` 是 LLM 看到的"数据窗口"——包含结构索引和当前窗口内容。
//! 每次 chunk 操作（read / search / summary）都返回完整视图，
//! 确保 LLM 始终拥有全局导航能力。

use serde::Serialize;
use serde_json::{json, Value};

/// LLM 可见的分片视图。
#[derive(Debug, Clone, Serialize)]
pub struct ChunkedView {
    pub chunk_id: String,
    pub total_bytes: usize,
    /// 语义分块总数（0 表示未构建语义分块，回退字节窗口）
    pub total_chunks: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<Section>,
    pub current_window: String,
    pub current_offset: usize,
}

/// 结构索引中的片段描述。
#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub title: String,
    pub start_byte: usize,
}

/// 关键词搜索结果。
#[derive(Debug, Clone, Serialize)]
pub struct SearchMatch {
    pub offset: usize,
    pub context: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section_title: Option<String>,
}

impl ChunkedView {
    /// 构建分片视图。
    pub fn new(
        chunk_id: String,
        total_bytes: usize,
        total_chunks: usize,
        sections: Vec<Section>,
        current_window: String,
        current_offset: usize,
    ) -> Self {
        Self {
            chunk_id,
            total_bytes,
            total_chunks,
            sections,
            current_window,
            current_offset,
        }
    }

    /// 序列化为 Observation 可用的 JSON。
    pub fn to_observation_json(&self) -> Value {
        let nav_hint = format!(
            "- builtin_chunk_read(\"{}\", chunk=N) → 跳到第 N 个语义块\n\
             - builtin_chunk_search(\"{}\", \"关键词\") → BM25 语义搜索\n\
             - builtin_chunk_summary(\"{}\") → 查看结构索引 + 第 0 块内容",
            self.chunk_id, self.chunk_id, self.chunk_id,
        );

        json!({
            "chunk_id": self.chunk_id,
            "total_bytes": self.total_bytes,
            "total_chunks": self.total_chunks,
            "sections": self.sections.iter().map(|s| json!({
                "title": s.title,
                "start_byte": s.start_byte,
            })).collect::<Vec<_>>(),
            "current_window": self.current_window,
            "hint": format!(
                "📋 分片视图 (ID: {}, 共{}个语义块, 当前偏移 {})\n\
                 可操作:\n\
                 {}",
                self.chunk_id,
                self.total_chunks,
                self.current_offset,
                nav_hint,
            ),
        })
    }
}
