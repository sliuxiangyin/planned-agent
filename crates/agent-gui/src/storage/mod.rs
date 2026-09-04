//! `agent-gui` 本地持久化基础设施（SeaORM + SQLite）
//!
//! MVP 阶段仅搭建骨架：1 张测试表（`tests`）+ Repository 签名 + Migration 框架。
//! 业务表（plans / insights / ...）在阶段 2 加，本表可保留作 smoke test 或删除。

pub mod entities;
pub mod migrations;
pub mod repository;
pub mod error;

// 公共 re-export —— 阶段 2 业务接入时会被使用
#[allow(unused_imports)]
pub use error::{StorageError, StorageResult};
#[allow(unused_imports)]
pub use migrations::Migrator;