//! 统一分片缓存。
//!
//! 同时服务工具输出和引用数据的大文本存储、清洗、索引和窗口读取。
//! 内部通过 `Arc<RwLock<>>` 实现线程安全。

use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::{anyhow, Result};
use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

use super::chunk_view::{ChunkedView, SearchMatch};
use super::smart_index::{self, ContentType};
use crw_core::types::{ChunkStrategy, FilterMode};

// ── 配置 ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ChunkConfig {
    pub window_size: usize,
    pub chunk_threshold: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            window_size: 4096,
            chunk_threshold: 8192,
        }
    }
}

// ── 内部条目 ──────────────────────────────────────────

struct ChunkEntry {
    text: String,
    total_bytes: usize,
    sections: Vec<super::chunk_view::Section>,
    /// 语义分块字节偏移: (start_byte, end_byte)，用于 BM25 搜索定位
    chunk_offsets: Vec<(usize, usize)>,
}

// ── ChunkStore ────────────────────────────────────────

pub struct ChunkStore {
    entries: RwLock<HashMap<String, ChunkEntry>>,
    config: ChunkConfig,
}

impl ChunkStore {
    pub fn new() -> Self {
        Self::with_config(ChunkConfig::default())
    }

    fn with_config(config: ChunkConfig) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            config,
        }
    }

    // ── 存储 ───────────────────────────────────────

    /// 存入文本，自动检测类型 → 清洗 → 语义分块 → 建索引 → 返回 chunk_id。
    async fn store(&self, raw_text: &str, tool_name: &str) -> Result<String> {
        let chunk_id = format!("tool_{}_{}", tool_name, short_uuid());

        // ── 1. 类型检测 ──
        let ct: ContentType = smart_index::detect_content_type(raw_text);

        // ── 2. HTML 清洗（crw-extract）→ 转为 Markdown ──
        let cleaned = if ct == ContentType::Html {
            self.clean_html(raw_text).unwrap_or_else(|e| {
                warn!("HTML 清洗失败，使用原文: {}", e);
                raw_text.to_string()
            })
        } else {
            raw_text.to_string()
        };

        // 清洗后 HTML 已转为 Markdown，后续用 Markdown 索引
        let index_ct = if ct == ContentType::Html {
            ContentType::Markdown
        } else {
            ct
        };

        let total_bytes = cleaned.len();

        // ── 3. 语义分块（crw-extract chunking）──
        let chunk_offsets = self.build_semantic_chunks(&cleaned, index_ct);

        // ── 4. 结构索引 ──
        let sections = smart_index::build_index_for(&cleaned, index_ct);

        let entry = ChunkEntry {
            text: cleaned,
            total_bytes,
            sections,
            chunk_offsets,
        };

        self.entries
            .write()
            .map_err(|e| anyhow!("ChunkStore 写锁失败: {}", e))?
            .insert(chunk_id.clone(), entry);

        Ok(chunk_id)
    }

    /// 使用 crw-extract 清洗 HTML：去除 boilerplate → 转为 Markdown。
    /// Markdown 对 LLM 更友好，且可直接复用现有 index_markdown。
    fn clean_html(&self, html: &str) -> Result<String> {
        // Step 1: 去除 script/style/nav/footer/ads 等噪音标签
        let cleaned = crw_extract::clean::clean_html(html, true, &[], &[])
            .map_err(|e| anyhow::anyhow!("crw-extract HTML 清洗失败: {}", e))?;

        // Step 2: 转为 Markdown（LLM 友好格式，且可复用 Markdown 索引）
        let markdown = crw_extract::markdown::html_to_markdown(&cleaned);

        Ok(markdown)
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
        // 确保 offset 和 end 落在 UTF-8 字符边界上，避免切片 panic
        let safe_offset = floor_char_boundary(&entry.text, offset);
        let safe_end = if end < entry.text.len() {
            floor_char_boundary(&entry.text, end)
        } else {
            entry.text.len()
        };
        let window = if safe_offset < entry.text.len() && safe_end <= entry.text.len() {
            entry.text[safe_offset..safe_end].to_string()
        } else {
            String::new()
        };

        Ok(ChunkedView::new(
            chunk_id.to_string(),
            entry.total_bytes,
            entry.chunk_offsets.len(),
            entry.sections.clone(),
            window,
            offset,
        ))
    }

    /// 按语义分块索引读取窗口，自动对齐 chunk 边界。
    /// 不会在句子中间截断，比字节偏移更适合 LLM 阅读。
    /// 无语义分块时自动回退到字节窗口。
    pub fn read_chunk(&self, chunk_id: &str, chunk_index: usize) -> Result<ChunkedView> {
        let guard = self
            .entries
            .read()
            .map_err(|e| anyhow!("ChunkStore 读锁失败: {}", e))?;

        let entry = guard
            .get(chunk_id)
            .ok_or_else(|| anyhow!("未找到分片数据: {}", chunk_id))?;

        // 有语义分块 → 按分块索引读取
        if chunk_index < entry.chunk_offsets.len() {
            let (start, end) = entry.chunk_offsets[chunk_index];
            let window = if start < entry.text.len() && end <= entry.text.len() {
                entry.text[start..end].to_string()
            } else {
                String::new()
            };

            return Ok(ChunkedView::new(
                chunk_id.to_string(),
                entry.total_bytes,
                entry.chunk_offsets.len(),
                entry.sections.clone(),
                window,
                start,
            ));
        }

        // 回退：无分块或索引越界 → 按字节窗口读取
        let offset = chunk_index * self.config.window_size;
        self.read_view(chunk_id, offset, self.config.window_size)
    }

    // ── 搜索 ────────────────────────────────────────

    /// BM25 语义搜索 + 关键词回退。
    /// 优先使用 store 时构建的语义分块进行 BM25 相关性排序，
    /// 无分块时回退到简单关键词匹配。
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

        // 有语义分块 → BM25 搜索
        if !entry.chunk_offsets.is_empty() {
            return self.search_bm25(entry, query);
        }

        // 回退：简单关键词搜索
        self.search_fallback_keyword(entry, query)
    }

    /// BM25 语义搜索：用 crw-extract 的 filter_chunks_scored 排序并映射回 SearchMatch。
    fn search_bm25(&self, entry: &ChunkEntry, query: &str) -> Result<Vec<SearchMatch>> {
        // 1. 按 chunk_offsets 提取各 chunk 文本
        let chunks: Vec<String> = entry
            .chunk_offsets
            .iter()
            .map(|&(s, e)| {
                if s < entry.text.len() && e <= entry.text.len() {
                    entry.text[s..e].to_string()
                } else {
                    String::new()
                }
            })
            .collect();

        // 2. BM25 排序，取 top 20
        let scored = crw_extract::filter::filter_chunks_scored(
            &chunks,
            query,
            &FilterMode::Bm25,
            20,
        );

        // 3. 映射回 SearchMatch
        let mut matches = Vec::with_capacity(scored.len());
        for sc in &scored {
            if sc.index < entry.chunk_offsets.len() {
                let (chunk_start, _chunk_end) = entry.chunk_offsets[sc.index];

                // 上下文：截取 chunk 前 300 字符作为摘要
                let context: String = sc.content.chars().take(300).collect();
                let context = if sc.content.len() > 300 {
                    format!("{}…", context)
                } else {
                    context
                };

                matches.push(SearchMatch {
                    offset: chunk_start,
                    context,
                    section_title: find_section_title(&entry.sections, chunk_start),
                });

                if matches.len() >= 20 {
                    break;
                }
            }
        }

        Ok(matches)
    }

    /// 回退关键词搜索（保留原逻辑作为降级方案）。
    fn search_fallback_keyword(&self, entry: &ChunkEntry, query: &str) -> Result<Vec<SearchMatch>> {
        let text = &entry.text;
        let lower_text = text.to_lowercase();
        let lower_query = query.to_lowercase();

        let mut matches = Vec::new();
        let context_radius: usize = 200;

        let mut search_start = 0usize;
        while let Some(pos) = lower_text[search_start..].find(&lower_query) {
            let abs_pos = search_start + pos;

            let ctx_start = abs_pos.saturating_sub(context_radius);
            let ctx_end = (abs_pos + lower_query.len() + context_radius).min(text.len());

            let context = if ctx_start > 0 && ctx_end < text.len() {
                format!("…{}…", &text[ctx_start..ctx_end])
            } else if ctx_start == 0 {
                format!("{}…", &text[ctx_start..ctx_end])
            } else {
                format!("…{}", &text[ctx_start..ctx_end])
            };

            let section_title = find_section_title(&entry.sections, abs_pos);

            matches.push(SearchMatch {
                offset: abs_pos,
                context,
                section_title,
            });

            if matches.len() >= 20 {
                break;
            }
            search_start = abs_pos + lower_query.len().max(1);
        }

        Ok(matches)
    }

    // ── 判定 ────────────────────────────────────────

    /// 判断文本是否应该分片。
    fn should_chunk(&self, text: &str) -> bool {
        text.len() > self.config.chunk_threshold
    }

    // ── 统一处理入口 ───────────────────────────────

    /// 处理工具输出：大文本自动分片存储并返回 ChunkedView，小文本原样透传。
    ///
    /// `tool_name` 用于标记来源（日志/调试），同时可据此做条件逻辑。
    pub async fn handle(&self, content: Value, tool_name: &str) -> Result<Value> {
        let raw_text = serde_json::to_string(&content).unwrap_or_default();

        if !self.should_chunk(&raw_text) {
            return Ok(content);
        }

        let chunk_id = self.store(&raw_text, tool_name).await?;

        let view = self.read_chunk(&chunk_id, 0)?;
        Ok(view.to_observation_json())
    }

    /// 使用 crw-extract 的 Topic/Sentence 策略将文本切分为语义块，
    /// 返回每个块的 (start_byte, end_byte) 偏移，用于 BM25 搜索定位。
    fn build_semantic_chunks(&self, text: &str, ct: ContentType) -> Vec<(usize, usize)> {
        let strategy = match ct {
            ContentType::Markdown | ContentType::Html => ChunkStrategy::Topic {
                max_chars: Some(2000),
                overlap_chars: Some(200),
                dedupe: Some(true),
            },
            _ => ChunkStrategy::Sentence {
                max_chars: Some(2000),
                overlap_chars: Some(100),
                dedupe: None,
            },
        };

        let chunks = crw_extract::chunking::chunk_text(text, &strategy);

        // 将 chunk 字符串映射回原文本的字节偏移
        let mut offsets = Vec::with_capacity(chunks.len());
        let mut search_from = 0usize;

        for chunk in &chunks {
            if let Some(pos) = text[search_from..].find(chunk.as_str()) {
                let start = search_from + pos;
                let end = start + chunk.len();
                offsets.push((start, end));
                search_from = start + chunk.len().max(1);
            }
        }

        offsets
    }

}

// ── 工具函数 ──────────────────────────────────────────

/// 在 sections 中查找包含 byte_pos 的最近 section 标题。
fn find_section_title(sections: &[super::chunk_view::Section], byte_pos: usize) -> Option<String> {
    sections
        .iter()
        .rev()
        .find(|s| s.start_byte <= byte_pos)
        .map(|s| s.title.clone())
}

fn short_uuid() -> String {
    Uuid::new_v4()
        .to_string()
        .chars()
        .take(8)
        .collect()
}

/// 将字节偏移向下取整到最近的 UTF-8 字符边界
fn floor_char_boundary(s: &str, mut pos: usize) -> usize {
    while pos > 0 && !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
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
