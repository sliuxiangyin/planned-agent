//! 统一分片缓存。
//!
//! 同时服务工具输出和引用数据的大文本存储、清洗、索引和窗口读取。
//! 内部通过 `Arc<RwLock<>>` 实现线程安全。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tracing::warn;
use uuid::Uuid;

use planned_agent_tool_manager::ToolRegistry;

use super::chunk_view::{ChunkedView, SearchMatch};
use super::smart_index::{self, ContentType};

// ── 配置 ──────────────────────────────────────────────

pub const DEFAULT_WINDOW_SIZE: usize = 4096;
pub const DEFAULT_CHUNK_THRESHOLD: usize = 8192;
pub const EXPAND_THRESHOLD: usize = 800;

#[derive(Debug, Clone)]
pub struct ChunkConfig {
    pub window_size: usize,
    pub chunk_threshold: usize,
    pub expand_threshold: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            window_size: DEFAULT_WINDOW_SIZE,
            chunk_threshold: DEFAULT_CHUNK_THRESHOLD,
            expand_threshold: EXPAND_THRESHOLD,
        }
    }
}

// ── 来源标记 ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ChunkSource {
    ToolOutput { tool_name: String },
    Reference { ref_id: String },
}

// ── 内部条目 ──────────────────────────────────────────

struct ChunkEntry {
    text: String,
    total_bytes: usize,
    sections: Vec<super::chunk_view::Section>,
    #[allow(dead_code)]
    source: ChunkSource,
}

// ── ChunkStore ────────────────────────────────────────

pub struct ChunkStore {
    entries: RwLock<HashMap<String, ChunkEntry>>,
    config: ChunkConfig,
    tool_registry: Arc<ToolRegistry>,
}

impl ChunkStore {
    pub fn new(tool_registry: Arc<ToolRegistry>) -> Self {
        Self::with_config(tool_registry, ChunkConfig::default())
    }

    pub fn with_config(tool_registry: Arc<ToolRegistry>, config: ChunkConfig) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            config,
            tool_registry,
        }
    }

    // ── 存储 ───────────────────────────────────────

    /// 存入文本，自动检测类型 → 清洗 → 建索引 → 返回 chunk_id。
    pub async fn store(&self, raw_text: &str, source: ChunkSource) -> Result<String> {
        let chunk_id = match &source {
            ChunkSource::ToolOutput { tool_name } => {
                format!("tool_{}_{}", tool_name, short_uuid())
            }
            ChunkSource::Reference { ref_id } => {
                format!("ref_{}", ref_id.trim_start_matches('#').trim_start_matches('E'))
            }
        };

        // 清洗：HTML → 调用 builtin_clean_html 转为纯文本
        let ct = smart_index::detect_content_type(raw_text);
        let cleaned = if ct == ContentType::Html {
            self.clean_html(raw_text).await.unwrap_or_else(|e| {
                warn!("HTML 清洗失败，使用原文: {}", e);
                raw_text.to_string()
            })
        } else {
            raw_text.to_string()
        };

        let total_bytes = cleaned.len();

        // 建索引（基于清洗后的文本）
        let sections = smart_index::build_index_for(&cleaned, ct);

        let entry = ChunkEntry {
            text: cleaned,
            total_bytes,
            sections,
            source,
        };

        self.entries
            .write()
            .map_err(|e| anyhow!("ChunkStore 写锁失败: {}", e))?
            .insert(chunk_id.clone(), entry);

        Ok(chunk_id)
    }

    /// 调用 builtin_clean_html 工具进行清洗。
    async fn clean_html(&self, html: &str) -> Result<String> {
        let outcome = self
            .tool_registry
            .call_tool(
                "builtin_clean_html",
                json!({ "html": html, "format": "text" }),
            )
            .await?;

        let content = outcome
            .result
            .content
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("builtin_clean_html 未返回 content 字段"))?;

        Ok(content.to_string())
    }

    // ── 读取窗口 ────────────────────────────────────

    /// 以指定偏移和大小读取窗口，包装为 ChunkedView。
    pub fn read_view(
        &self,
        chunk_id: &str,
        offset: usize,
        size: usize,
    ) -> Result<ChunkedView> {
        let guard = self
            .entries
            .read()
            .map_err(|e| anyhow!("ChunkStore 读锁失败: {}", e))?;

        let entry = guard
            .get(chunk_id)
            .ok_or_else(|| anyhow!("未找到分片数据: {}", chunk_id))?;

        let effective_size = size.min(self.config.window_size).max(256);
        let offset = offset.min(entry.total_bytes.saturating_sub(1));

        let end = (offset + effective_size).min(entry.total_bytes);
        let window = if offset < entry.text.len() && end <= entry.text.len() {
            entry.text[offset..end].to_string()
        } else {
            String::new()
        };

        Ok(ChunkedView::new(
            chunk_id.to_string(),
            entry.total_bytes,
            entry.sections.clone(),
            window,
            offset,
            effective_size,
        ))
    }

    /// 自动翻页：读取当前 offset 后的下一个窗口（offset = prev_offset + window_size）。
    pub fn read_next_view(&self, chunk_id: &str, prev_offset: usize) -> Result<ChunkedView> {
        let next_offset = prev_offset + self.config.window_size;
        self.read_view(chunk_id, next_offset, self.config.window_size)
    }

    // ── 搜索 ────────────────────────────────────────

    /// 关键词搜索，返回匹配列表。
    pub fn search(
        &self,
        chunk_id: &str,
        query: &str,
    ) -> Result<Vec<SearchMatch>> {
        let guard = self
            .entries
            .read()
            .map_err(|e| anyhow!("ChunkStore 读锁失败: {}", e))?;

        let entry = guard
            .get(chunk_id)
            .ok_or_else(|| anyhow!("未找到分片数据: {}", chunk_id))?;

        let text = &entry.text;
        let lower_text = text.to_lowercase();
        let lower_query = query.to_lowercase();

        let mut matches = Vec::new();
        let context_radius: usize = 200;

        let mut search_start = 0usize;
        while let Some(pos) = lower_text[search_start..].find(&lower_query) {
            let abs_pos = search_start + pos;

            // 上下文范围
            let ctx_start = abs_pos.saturating_sub(context_radius);
            let ctx_end = (abs_pos + lower_query.len() + context_radius).min(text.len());

            let context = if ctx_start > 0 && ctx_end < text.len() {
                format!(
                    "…{}…",
                    &text[ctx_start..ctx_end]
                )
            } else if ctx_start == 0 {
                format!("{}…", &text[ctx_start..ctx_end])
            } else {
                format!("…{}", &text[ctx_start..ctx_end])
            };

            // 所属 section
            let section_title = entry
                .sections
                .iter()
                .rev()
                .find(|s| s.start_byte <= abs_pos)
                .map(|s| s.title.clone());

            matches.push(SearchMatch {
                offset: abs_pos,
                context,
                section_title,
            });

            // 前进，最多返回 20 条
            if matches.len() >= 20 {
                break;
            }
            search_start = abs_pos + lower_query.len().max(1);
        }

        Ok(matches)
    }

    // ── 索引查询 ────────────────────────────────────

    /// 获取结构索引。
    pub fn get_sections(&self, chunk_id: &str) -> Result<Vec<super::chunk_view::Section>> {
        let guard = self
            .entries
            .read()
            .map_err(|e| anyhow!("ChunkStore 读锁失败: {}", e))?;

        let entry = guard
            .get(chunk_id)
            .ok_or_else(|| anyhow!("未找到分片数据: {}", chunk_id))?;

        Ok(entry.sections.clone())
    }

    // ── 判定 ────────────────────────────────────────

    /// 判断文本是否应该分片。
    pub fn should_chunk(&self, text: &str) -> bool {
        text.len() > self.config.chunk_threshold
    }

    // ── 统一处理入口 ───────────────────────────────

    /// 处理工具输出：大文本自动分片存储并返回 ChunkedView，小文本原样透传。
    ///
    /// `handle_generic_tool` 的唯一调用入口，内部封装阈值判断、存储、视图构建。
    pub async fn handle(&self, content: Value) -> Result<Value> {
        let raw_text = serde_json::to_string(&content).unwrap_or_default();

        if !self.should_chunk(&raw_text) {
            return Ok(content);
        }

        let source = ChunkSource::ToolOutput {
            tool_name: "output".to_string(),
        };
        let chunk_id = self.store(&raw_text, source).await?;

        let view = self.read_view(&chunk_id, 0, self.config.window_size)?;
        Ok(view.to_observation_json())
    }

    pub fn expand_threshold(&self) -> usize {
        self.config.expand_threshold
    }

    pub fn chunk_threshold(&self) -> usize {
        self.config.chunk_threshold
    }

    pub fn window_size(&self) -> usize {
        self.config.window_size
    }

    /// 判断 chunk_id 是否存在。
    pub fn exists(&self, chunk_id: &str) -> bool {
        self.entries
            .read()
            .map(|g| g.contains_key(chunk_id))
            .unwrap_or(false)
    }

    /// 当前缓存的条目数。
    pub fn len(&self) -> usize {
        self.entries.read().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // ── 元数据 ──────────────────────────────────────

    /// 根据 chunk_id 获取来源信息（用于日志）。
    pub fn source_info(&self, chunk_id: &str) -> Option<String> {
        self.entries
            .read()
            .ok()
            .and_then(|g| {
                g.get(chunk_id).map(|e| match &e.source {
                    ChunkSource::ToolOutput { tool_name } => format!("tool:{}", tool_name),
                    ChunkSource::Reference { ref_id } => format!("ref:{}", ref_id),
                })
            })
    }
}

// ── 工具函数 ──────────────────────────────────────────

fn short_uuid() -> String {
    Uuid::new_v4()
        .to_string()
        .chars()
        .take(8)
        .collect()
}

// ── 测试 ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_uuid_is_8_chars() {
        let id = short_uuid();
        assert_eq!(id.len(), 8);
    }
}
