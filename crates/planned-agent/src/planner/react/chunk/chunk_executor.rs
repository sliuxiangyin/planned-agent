//! 分片工具定义 + Executor + 内置 Provider。
//!
//! Executor 通过 `ExecutorContext` 获取运行时的 `ChunkStore`，
//! 无需在注册时持有 `ChunkStore`，解耦构造时机，同时避免循环引用。

use std::sync::{Arc, Weak};

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use planned_agent_core::tool_registry::{BuiltinToolProvider, ToolCategory, ToolExecutor};
use planned_agent_core::types::{Tool, ToolResult};

use super::chunk_store::ChunkStore;
use super::executor_context::ExecutorContext;

/// 分片工具执行器。
///
/// 持有 `Weak<ExecutorContext>`（不增加 refcount，无循环引用风险），
/// 运行时通过 `upgrade()` 获取 `ChunkStore`。
pub struct ChunkStoreExecutor {
    ctx: Weak<ExecutorContext>,
}

impl ChunkStoreExecutor {
    pub fn new(ctx: Weak<ExecutorContext>) -> Self {
        Self { ctx }
    }

    fn get_chunk_store(&self) -> Result<Arc<ChunkStore>> {
        let exec_ctx = self.ctx
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("ExecutorContext 已释放"))?;
        exec_ctx
            .chunk_store()
            .ok_or_else(|| anyhow::anyhow!("ChunkStore 尚未注入到 ExecutorContext"))
    }
}

#[async_trait]
impl ToolExecutor for ChunkStoreExecutor {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        let cs = self.get_chunk_store()?;

        match tool_name {
            "builtin_chunk_read" => {
                let chunk_id = arguments["chunk_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 chunk_id"))?;
                let chunk_index = arguments["chunk"].as_u64().unwrap_or(0) as usize;

                let view = cs.read_chunk(chunk_id, chunk_index)?;
                let content = serde_json::to_value(&view)?;
                Ok(ToolResult {
                    call_id: String::new(),
                    content,
                    is_error: false,
                })
            }
            "builtin_chunk_search" => {
                let chunk_id = arguments["chunk_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 chunk_id"))?;
                let query = arguments["query"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 query"))?;

                let matches = cs.search(chunk_id, query)?;
                let content = serde_json::to_value(&matches)?;
                Ok(ToolResult {
                    call_id: String::new(),
                    content,
                    is_error: false,
                })
            }
            "builtin_chunk_summary" => {
                let chunk_id = arguments["chunk_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("缺少 chunk_id"))?;

                let view = cs.read_chunk(chunk_id, 0)?;
                let content = serde_json::to_value(&view)?;
                Ok(ToolResult {
                    call_id: String::new(),
                    content,
                    is_error: false,
                })
            }
            _ => Err(anyhow::anyhow!("未知 chunk 工具: {}", tool_name)),
        }
    }

    fn name(&self) -> &str {
        "chunk_store_executor"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec![
            "builtin_chunk_read".to_string(),
            "builtin_chunk_search".to_string(),
            "builtin_chunk_summary".to_string(),
        ]
    }
}

// ── 工具定义 ──────────────────────────────────────────

fn chunk_read_tool() -> (Tool, Vec<ToolCategory>) {
    (
        Tool {
            name: "builtin_chunk_read".to_string(),
            description: "按语义块翻页读取分片缓存，自动对齐段落/句子边界，不会截断内容。每次返回当前块内容 + 全局结构索引。"
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "chunk_id": {
                        "type": "string",
                        "description": "分片缓存 ID（从 ChunkedView 中获取）"
                    },
                    "chunk": {
                        "type": "integer",
                        "description": "语义分块索引（0-based，默认 0）"
                    }
                },
                "required": ["chunk_id"]
            }),
        },
        vec![ToolCategory::Utility],
    )
}

fn chunk_search_tool() -> (Tool, Vec<ToolCategory>) {
    (
        Tool {
            name: "builtin_chunk_search".to_string(),
            description: "在分片缓存中搜索关键词，返回匹配位置列表（含上下文和所属片段标题）。最多返回 20 条匹配。"
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "chunk_id": {
                        "type": "string",
                        "description": "分片缓存 ID"
                    },
                    "query": {
                        "type": "string",
                        "description": "搜索关键词"
                    }
                },
                "required": ["chunk_id", "query"]
            }),
        },
        vec![ToolCategory::Utility],
    )
}

fn chunk_summary_tool() -> (Tool, Vec<ToolCategory>) {
    (
        Tool {
            name: "builtin_chunk_summary".to_string(),
            description: "重新查看分片缓存的结构索引（各片段标题 + 字节偏移），不返回内容窗口。用于导航定位。"
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "chunk_id": {
                        "type": "string",
                        "description": "分片缓存 ID"
                    }
                },
                "required": ["chunk_id"]
            }),
        },
        vec![ToolCategory::Utility],
    )
}

// ── BuiltinToolProvider ───────────────────────────────

/// 分片工具提供者。
///
/// 持有 `Arc<ExecutorContext>`，注册时将 ctx 的 `Weak` 引用传给 `ChunkStoreExecutor`，
/// executor 运行时通过 `upgrade()` 获取 `ChunkStore`。
pub struct ChunkToolsProvider {
    exec_ctx: Arc<ExecutorContext>,
}

impl ChunkToolsProvider {
    pub fn new(exec_ctx: Arc<ExecutorContext>) -> Self {
        Self { exec_ctx }
    }
}

impl BuiltinToolProvider for ChunkToolsProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![
            chunk_read_tool(),
            chunk_search_tool(),
            chunk_summary_tool(),
        ]
    }

    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(ChunkStoreExecutor::new(Arc::downgrade(&self.exec_ctx)))
    }
}
