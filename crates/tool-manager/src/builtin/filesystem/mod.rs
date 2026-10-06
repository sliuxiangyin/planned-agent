//! filesystem 工具族：cap-std 沙箱 + 对齐 rust-mcp-filesystem 重写的文件 / 目录工具。
//!
//! 设计依据：`docs/planned-agent/filesystem-tools-rewrite.md`。
//!
//! 分层（照搬上游 `fs_service` + `tools` 两层）：
//! - 服务层 [`core`]：cap-std 能力沙箱与路径解析，**不认识** `Value` / `ToolResult`；
//! - 契约层 [`contract`]：schema + description（本仓库唯一真正生效的契约）；
//! - 横切层 [`support`]：审计 / 错误码 / 原子写 / 编码探测；
//! - 外壳层（本文件）：把 `Value` 解释成调用、把结果组装成 `ToolResult` 并写审计。

pub(crate) mod archive;
pub(crate) mod core;
mod contract;
pub(crate) mod info;
pub(crate) mod io;
pub(crate) mod list;
pub(crate) mod search;
pub(crate) mod support;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::{BuiltinToolProvider, ToolCategory, ToolExecutor};

use core::FilesystemService;

/// filesystem 工具提供者。
pub struct FilesystemProvider {
    service: Arc<FilesystemService>,
}

impl FilesystemProvider {
    /// `roots` 是允许访问的目录白名单（cap-std 沙箱边界）。
    ///
    /// 空列表合法，但此后一切访问都会被拒（`path_outside_allowed`）—— 这是安全默认。
    pub fn new(roots: &[PathBuf]) -> Result<Self> {
        let service = FilesystemService::try_new(roots)
            .map_err(|error| anyhow::anyhow!("filesystem 工具初始化失败：{}", error.message()))?;
        Ok(Self {
            service: Arc::new(service),
        })
    }
}

impl BuiltinToolProvider for FilesystemProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![
            (
                Tool {
                    name: "builtin_read_text_file".to_string(),
                    description: contract::READ_TEXT_FILE_DESCRIPTION.to_string(),
                    input_schema: contract::read_text_file_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_read_file_lines".to_string(),
                    description: contract::READ_FILE_LINES_DESCRIPTION.to_string(),
                    input_schema: contract::read_file_lines_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_read_multiple_text_files".to_string(),
                    description: contract::READ_MULTIPLE_TEXT_FILES_DESCRIPTION.to_string(),
                    input_schema: contract::read_multiple_text_files_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_head_file".to_string(),
                    description: contract::HEAD_FILE_DESCRIPTION.to_string(),
                    input_schema: contract::head_file_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_tail_file".to_string(),
                    description: contract::TAIL_FILE_DESCRIPTION.to_string(),
                    input_schema: contract::tail_file_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_read_media_file".to_string(),
                    description: contract::READ_MEDIA_FILE_DESCRIPTION.to_string(),
                    input_schema: contract::read_media_file_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_read_multiple_media_files".to_string(),
                    description: contract::READ_MULTIPLE_MEDIA_FILES_DESCRIPTION.to_string(),
                    input_schema: contract::read_multiple_media_files_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_write_file".to_string(),
                    description: contract::WRITE_FILE_DESCRIPTION.to_string(),
                    input_schema: contract::write_file_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_create_directory".to_string(),
                    description: contract::CREATE_DIRECTORY_DESCRIPTION.to_string(),
                    input_schema: contract::create_directory_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_move_file".to_string(),
                    description: contract::MOVE_FILE_DESCRIPTION.to_string(),
                    input_schema: contract::move_file_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_edit_file".to_string(),
                    description: contract::EDIT_FILE_DESCRIPTION.to_string(),
                    input_schema: contract::edit_file_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_list_directory".to_string(),
                    description: contract::LIST_DIRECTORY_DESCRIPTION.to_string(),
                    input_schema: contract::list_directory_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_list_directory_with_sizes".to_string(),
                    description: contract::LIST_DIRECTORY_WITH_SIZES_DESCRIPTION.to_string(),
                    input_schema: contract::list_directory_with_sizes_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_directory_tree".to_string(),
                    description: contract::DIRECTORY_TREE_DESCRIPTION.to_string(),
                    input_schema: contract::directory_tree_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_get_file_info".to_string(),
                    description: contract::GET_FILE_INFO_DESCRIPTION.to_string(),
                    input_schema: contract::get_file_info_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_search_files".to_string(),
                    description: contract::SEARCH_FILES_DESCRIPTION.to_string(),
                    input_schema: contract::search_files_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_search_files_content".to_string(),
                    description: contract::SEARCH_FILES_CONTENT_DESCRIPTION.to_string(),
                    input_schema: contract::search_files_content_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_grep_file".to_string(),
                    description: contract::GREP_FILE_DESCRIPTION.to_string(),
                    input_schema: contract::grep_file_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_calculate_directory_size".to_string(),
                    description: contract::CALCULATE_DIRECTORY_SIZE_DESCRIPTION.to_string(),
                    input_schema: contract::calculate_directory_size_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_find_duplicate_files".to_string(),
                    description: contract::FIND_DUPLICATE_FILES_DESCRIPTION.to_string(),
                    input_schema: contract::find_duplicate_files_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_find_empty_directories".to_string(),
                    description: contract::FIND_EMPTY_DIRECTORIES_DESCRIPTION.to_string(),
                    input_schema: contract::find_empty_directories_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_zip_files".to_string(),
                    description: contract::ZIP_FILES_DESCRIPTION.to_string(),
                    input_schema: contract::zip_files_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_zip_directory".to_string(),
                    description: contract::ZIP_DIRECTORY_DESCRIPTION.to_string(),
                    input_schema: contract::zip_directory_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_unzip_file".to_string(),
                    description: contract::UNZIP_FILE_DESCRIPTION.to_string(),
                    input_schema: contract::unzip_file_schema(),
                },
                vec![ToolCategory::File],
            ),
        ]
    }

    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(FilesystemExecutor {
            service: self.service.clone(),
        })
    }
}

/// filesystem 工具执行器。
struct FilesystemExecutor {
    service: Arc<FilesystemService>,
}

#[async_trait]
impl ToolExecutor for FilesystemExecutor {
    fn name(&self) -> &str {
        "builtin_filesystem"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec![
            "builtin_read_text_file".to_string(),
            "builtin_read_file_lines".to_string(),
            "builtin_read_multiple_text_files".to_string(),
            "builtin_head_file".to_string(),
            "builtin_tail_file".to_string(),
            "builtin_read_media_file".to_string(),
            "builtin_read_multiple_media_files".to_string(),
            "builtin_write_file".to_string(),
            "builtin_create_directory".to_string(),
            "builtin_move_file".to_string(),
            "builtin_edit_file".to_string(),
            "builtin_list_directory".to_string(),
            "builtin_list_directory_with_sizes".to_string(),
            "builtin_directory_tree".to_string(),
            "builtin_get_file_info".to_string(),
            "builtin_search_files".to_string(),
            "builtin_search_files_content".to_string(),
            "builtin_grep_file".to_string(),
            "builtin_calculate_directory_size".to_string(),
            "builtin_find_duplicate_files".to_string(),
            "builtin_find_empty_directories".to_string(),
            "builtin_zip_files".to_string(),
            "builtin_zip_directory".to_string(),
            "builtin_unzip_file".to_string(),
        ]
    }

    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        // 约定：可预期失败一律 `Ok + is_error`（见 `support::failure`）；`Err` 只留给「未知工具名」。
        Ok(match tool_name {
            "builtin_read_text_file" => io::read::read_text_file(&self.service, &arguments).await,
            "builtin_read_file_lines" => io::read::read_file_lines(&self.service, &arguments).await,
            "builtin_read_multiple_text_files" => {
                io::read::read_multiple_text_files(&self.service, &arguments).await
            }
            "builtin_head_file" => io::read::head_file(&self.service, &arguments).await,
            "builtin_tail_file" => io::read::tail_file(&self.service, &arguments).await,
            "builtin_read_media_file" => io::read::read_media_file(&self.service, &arguments).await,
            "builtin_read_multiple_media_files" => {
                io::read::read_multiple_media_files(&self.service, &arguments).await
            }
            "builtin_write_file" => io::write::write_file(&self.service, &arguments).await,
            "builtin_create_directory" => io::write::create_directory(&self.service, &arguments).await,
            "builtin_move_file" => io::write::move_file(&self.service, &arguments).await,
            "builtin_edit_file" => io::edit::edit_file(&self.service, &arguments).await,
            "builtin_list_directory" => list::list_directory(&self.service, &arguments).await,
            "builtin_list_directory_with_sizes" => {
                list::list_directory_with_sizes(&self.service, &arguments).await
            }
            "builtin_directory_tree" => {
                search::tree::directory_tree(&self.service, &arguments).await
            }
            "builtin_get_file_info" => info::get_file_info(&self.service, &arguments).await,
            "builtin_search_files" => {
                search::files::search_files(&self.service, &arguments).await
            }
            "builtin_search_files_content" => {
                search::content::search_files_content(&self.service, &arguments).await
            }
            "builtin_grep_file" => search::grep::grep_file(&self.service, &arguments).await,
            "builtin_calculate_directory_size" => {
                info::calculate_directory_size(&self.service, &arguments).await
            }
            "builtin_find_duplicate_files" => {
                info::find_duplicate_files(&self.service, &arguments).await
            }
            "builtin_find_empty_directories" => {
                info::find_empty_directories(&self.service, &arguments).await
            }
            "builtin_zip_files" => archive::zip::zip_files(&self.service, &arguments).await,
            "builtin_zip_directory" => {
                archive::zip::zip_directory(&self.service, &arguments).await
            }
            "builtin_unzip_file" => archive::unzip::unzip_file(&self.service, &arguments).await,
            other => return Err(anyhow::anyhow!("Unknown filesystem tool: {other}")),
        })
    }
}
