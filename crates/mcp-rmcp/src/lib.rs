#![doc = include_str!("../README.md")]

pub mod bundle;
pub mod client;
pub mod command_resolver;
pub mod config;
pub mod manager;
pub mod storage;
pub mod tools;

pub use bundle::McpServerView;
pub use client::McpClientImpl;
pub use command_resolver::resolve_command;
pub use manager::McpManager;
pub use storage::{
    FileMcpConfigStorage, FileMcpStatusStorage, InMemoryMcpConfigStorage,
    InMemoryMcpStatusStorage, LastStatus, McpConfigStorage, McpStatusStorage, ServerStatus,
};
pub use tools::ToolManager;
