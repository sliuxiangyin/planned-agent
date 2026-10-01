//! 归档类工具：`builtin_zip_files` / `builtin_zip_directory` / `builtin_unzip_file`。
//!
//! 语义对齐上游（③）：
//! - `zip_files {input_files, target_zip_file}`；
//! - `zip_directory {input_directory, pattern?, target_zip_file}`（pattern 默认 `**/*`）；
//! - `unzip_file {zip_file, target_path}`。
//!
//! 内部保留本仓横切强化（①）：cap-std 沙箱、**zip-slip 防护**（`enclosed_name`）、
//! 原子写产物、`tool_audit` 审计。

pub(crate) mod unzip;
pub(crate) mod zip;
