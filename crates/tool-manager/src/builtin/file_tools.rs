use std::sync::Arc;
use async_trait::async_trait;
use anyhow::Result;
use serde_json::{json, Value};
use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::{ToolExecutor, ToolCategory, BuiltinToolProvider};

/// 内置文件工具提供者
pub struct FileToolsProvider;

impl BuiltinToolProvider for FileToolsProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![
            (
                Tool {
                    name: "builtin_read_file".to_string(),
                    description: "读取文件内容（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" }
                        },
                        "required": ["path"]
                    }),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_write_file".to_string(),
                    description: "写入文件内容（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "content": { "type": "string" }
                        },
                        "required": ["path", "content"]
                    }),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_list_dir".to_string(),
                    description: "列出目录内容（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" }
                        },
                        "required": ["path"]
                    }),
                },
                vec![ToolCategory::File],
            ),
        ]
    }
    
    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(FileToolsExecutor)
    }
}

/// 文件工具执行器
struct FileToolsExecutor;

#[async_trait]
impl ToolExecutor for FileToolsExecutor {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        match tool_name {
            "builtin_read_file" => {
                let path = arguments["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path"))?;
                let content = std::fs::read_to_string(path)
                    .map_err(|error| fs_error("读取文件", path, error))?;
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({ "content": content }),
                    is_error: false,
                })
            }
            "builtin_write_file" => {
                let path = arguments["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path"))?;
                let content = arguments["content"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing content"))?;
                std::fs::write(path, content)
                    .map_err(|error| fs_error("写入文件", path, error))?;
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({ "success": true }),
                    is_error: false,
                })
            }
            "builtin_list_dir" => {
                let path = arguments["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path"))?;
                let entries: Vec<String> = std::fs::read_dir(path)
                    .map_err(|error| fs_error("列出目录", path, error))?
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({ "entries": entries }),
                    is_error: false,
                })
            }
            _ => Err(anyhow::anyhow!("Unknown tool: {}", tool_name))
        }
    }
    
    fn name(&self) -> &str {
        "builtin_file_tools"
    }
    
    fn supported_tools(&self) -> Vec<String> {
        vec![
            "builtin_read_file".to_string(),
            "builtin_write_file".to_string(),
            "builtin_list_dir".to_string(),
        ]
    }
}

/// 构造带 **路径回显**的文件操作错误。
///
/// `std::fs` 的 `io::Error` 只说「系统找不到指定的路径。 (os error 3)」，不告诉调用方是哪个
/// 路径 —— 传错路径的一方（典型是拼写/用户名打错）只能看到一串没有信息量的错误码，进而靠反复
/// 换写法试错，白白烧掉大量重试轮次。这里把路径与「可能是拼写问题」一并回显。
fn fs_error(action: &str, path: &str, error: std::io::Error) -> anyhow::Error {
    let hint = if error.kind() == std::io::ErrorKind::NotFound {
        "（该路径不存在，请核对拼写）"
    } else {
        ""
    };
    anyhow::anyhow!("{action}「{path}」失败：{error}{hint}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 报错必须**回显路径**：否则调用方只看到一个没有信息量的错误码，无从得知是自己传的
    /// 路径写错了（真实案例：`C:\Users\wodpp\...` 比真实用户名多打了一个 p，白烧 10 轮重试）。
    #[tokio::test]
    async fn missing_path_error_echoes_the_path() {
        let missing = "C:/Users/no-such-user-wodpp/Desktop/Downloads";
        let message = match FileToolsExecutor
            .execute("builtin_list_dir", json!({ "path": missing }))
            .await
        {
            Ok(_) => panic!("不存在的目录不该成功"),
            Err(error) => error.to_string(),
        };
        assert!(
            message.contains(missing),
            "错误信息必须回显路径，否则无从定位：{message}"
        );
        assert!(
            message.contains("核对拼写"),
            "「路径不存在」应点明可能是拼写问题：{message}"
        );
    }
}
