//! MCP 存储抽象层
//!
//! 两类存储 trait + 内置实现：
//!
//! ## 配置存储 [`McpConfigStorage`]
//!
//! | 实现 | 用途 | 适用场景 |
//! | --- | --- | --- |
//! | [`FileMcpConfigStorage`] | JSON 文件 + 原子写 | **CLI 默认**，GUI 兼容回退 |
//! | [`InMemoryMcpConfigStorage`] | `RwLock<McpConfigFile>` | **测试** / 单元断言 |
//!
//! ## 连接状态存储 [`McpStatusStorage`]
//!
//! | 实现 | 用途 | 适用场景 |
//! | --- | --- | --- |
//! | [`FileMcpStatusStorage`] | JSON 文件 + 原子写 | **CLI 默认**，GUI 兼容回退 |
//! | [`InMemoryMcpStatusStorage`] | `RwLock<BTreeMap>` | **测试** / 单元断言 |
//!
//! ## 调用方自定义实现
//!
//! - **GUI 场景**：`agent-gui` 侧基于 sled 实现 `KvMcpConfigStorage` / `KvMcpStatusStorage`
//! - **未来**：DB / 网络 / 多端同步等任意后端，实现 trait 即可接入

pub mod file_storage;
pub mod memory_storage;
pub mod status_file_storage;
pub mod status_memory_storage;
pub mod status_trait;
pub mod trait_def;

pub use file_storage::FileMcpConfigStorage;
pub use memory_storage::InMemoryMcpConfigStorage;
pub use status_file_storage::FileMcpStatusStorage;
pub use status_memory_storage::InMemoryMcpStatusStorage;
pub use status_trait::{LastStatus, McpStatusStorage, ServerStatus};
pub use trait_def::McpConfigStorage;
