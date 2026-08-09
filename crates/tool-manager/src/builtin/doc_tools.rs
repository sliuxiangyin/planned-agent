use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use anyhow::Result;
use serde_json::{json, Value};

use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::{ToolExecutor, ToolCategory, BuiltinToolProvider};

/// 内置文档工具提供者。
///
/// 提供 `builtin_read_documentation` 工具，允许 AI 按需加载 `docs/` 目录下的规范文档。
///
/// # 安全限制
/// - 仅允许读取 `.md` 扩展名文件
/// - 路径穿越防护：拒绝包含 `..` 或 `/` 的 name 参数
pub struct DocToolsProvider {
    docs_dir: PathBuf,
}

impl DocToolsProvider {
    /// 创建文档工具提供者。
    ///
    /// `docs_dir` 为文档根目录（如 `prompts/docs/`）。
    pub fn new(docs_dir: PathBuf) -> Self {
        Self { docs_dir }
    }
}

impl BuiltinToolProvider for DocToolsProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![(
            Tool {
                name: "builtin_read_documentation".to_string(),
                description:
                    "读取内置规范文档。当需要了解某个工具或功能的详细使用规范时调用，\
                     例如 request_user_action 的 actions 格式、参数生成规则等。\
                     传入文档名（不含 .md 扩展名），返回文档全文。"
                        .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "文档名，不含 .md 扩展名（如 \"request_user_action\"）"
                        }
                    },
                    "required": ["name"]
                }),
            },
            vec![ToolCategory::Utility],
        )]
    }

    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(DocToolsExecutor {
            docs_dir: self.docs_dir.clone(),
        })
    }
}

/// 文档工具执行器。
struct DocToolsExecutor {
    docs_dir: PathBuf,
}

#[async_trait]
impl ToolExecutor for DocToolsExecutor {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        match tool_name {
            "builtin_read_documentation" => {
                let name = arguments["name"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing required parameter: name"))?;

                // 安全校验：拒绝路径穿越
                if name.contains("..") || name.contains('/') || name.contains('\\') {
                    return Ok(ToolResult {
                        call_id: uuid::Uuid::new_v4().to_string(),
                        content: json!({
                            "error": format!("Invalid document name: '{}'. Name must not contain path separators or '..'.", name)
                        }),
                        is_error: true,
                    });
                }

                let file_path = self.docs_dir.join(format!("{}.md", name));

                if !file_path.exists() {
                    // 列出可用文档帮助 AI 定位
                    let available: Vec<String> = match std::fs::read_dir(&self.docs_dir) {
                        Ok(entries) => entries
                            .filter_map(|e| e.ok())
                            .filter(|e| e.path().extension().map_or(false, |ext| ext == "md"))
                            .filter_map(|e| {
                                e.path()
                                    .file_stem()
                                    .map(|s| s.to_string_lossy().to_string())
                            })
                            .collect(),
                        Err(_) => vec![],
                    };

                    let hint = if available.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " Available documents: {}.",
                            available
                                .iter()
                                .map(|d| format!("'{}'", d))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };

                    return Ok(ToolResult {
                        call_id: uuid::Uuid::new_v4().to_string(),
                        content: json!({
                            "error": format!("Document '{}' not found.{}", name, hint)
                        }),
                        is_error: true,
                    });
                }

                let content = std::fs::read_to_string(&file_path)?;

                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({
                        "name": name,
                        "content": content,
                    }),
                    is_error: false,
                })
            }
            _ => Err(anyhow::anyhow!("Unknown tool: {}", tool_name)),
        }
    }

    fn name(&self) -> &str {
        "builtin_doc_tools"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec!["builtin_read_documentation".to_string()]
    }
}
