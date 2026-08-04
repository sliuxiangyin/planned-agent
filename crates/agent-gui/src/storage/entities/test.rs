//! tests 表 entity —— 仅用于验证 SeaORM 接入与迁移框架。
//!
//! 阶段 2 加业务表（plans / insights / ...）时，本表可保留作 smoke test 也可直接删除。

use sea_orm::entity::prelude::*;

/// tests 表主键 id（自增）
#[allow(dead_code)] // 阶段 2 接入业务时被使用
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "tests")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub value: String,
    pub created_at: i64,
}

#[allow(dead_code)] // DeriveRelation 要求枚举存在
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}