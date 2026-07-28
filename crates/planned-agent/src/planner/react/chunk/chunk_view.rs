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
    pub total_windows: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<Section>,
    pub current_window: String,
    pub current_offset: usize,
    pub window_size: usize,
}

/// 结构索引中的片段描述。
#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub title: String,
    pub start_byte: usize,
    pub end_byte: usize,
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
        sections: Vec<Section>,
        current_window: String,
        current_offset: usize,
        window_size: usize,
    ) -> Self {
        let total_windows = if total_bytes == 0 {
            0
        } else {
            (total_bytes + window_size - 1) / window_size
        };
        Self {
            chunk_id,
            total_bytes,
            total_windows,
            sections,
            current_window,
            current_offset,
            window_size,
        }
    }

    /// 序列化为 Observation 可用的 JSON。
    pub fn to_observation_json(&self) -> Value {
        json!({
            "chunk_id": self.chunk_id,
            "total_bytes": self.total_bytes,
            "total_windows": self.total_windows,
            "window_size": self.window_size,
            "current_offset": self.current_offset,
            "sections": self.sections.iter().map(|s| json!({
                "title": s.title,
                "start_byte": s.start_byte,
            })).collect::<Vec<_>>(),
            "current_window": self.current_window,
            "hint": format!(
                "📋 分片视图 (ID: {}, 共{}页, 当前第{}页)\n\
                 可操作:\n\
                 - chunk_read(\"{}\") → 翻到下一页\n\
                 - chunk_read(\"{}\", offset=N) → 跳到指定位置\n\
                 - chunk_search(\"{}\", \"关键词\") → 搜索\n\
                 - chunk_summary(\"{}\") → 重新查看结构索引",
                self.chunk_id,
                self.total_windows,
                self.current_offset / self.window_size.max(1) + 1,
                self.chunk_id,
                self.chunk_id,
                self.chunk_id,
                self.chunk_id,
            ),
        })
    }
}
