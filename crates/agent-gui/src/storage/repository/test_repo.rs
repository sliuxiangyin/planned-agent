//! tests 表仓库 —— MVP 阶段仅验证 Entity ↔ Repository ↔ DbConn 链路。
//!
//! 方法签名仅含 `list_all` 与 `insert` 两个最小集合，供阶段 2 业务接入时参考。
//! 阶段 2 可随表增量加 `find_by_id` / `update` / `delete`。

use sea_orm::DatabaseConnection;

use crate::storage::entities::test;
use crate::storage::error::StorageResult;

/// tests 表仓库
#[allow(dead_code)] // MVP 占位 —— 阶段 2 业务接入时启用
pub struct TestRepo {
    db: DatabaseConnection,
}

#[allow(dead_code)] // MVP 占位 —— 阶段 2 业务接入时启用
impl TestRepo {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// 列出全部测试记录
    pub async fn list_all(&self) -> StorageResult<Vec<test::Model>> {
        todo!("MVP 阶段未实现：阶段 2 接入业务时填充")
    }

    /// 插入一条测试记录（返回自增 id）
    pub async fn insert(&self, _name: String, _value: String) -> StorageResult<i32> {
        todo!("MVP 阶段未实现")
    }
}