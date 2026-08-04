# agent-gui 本地持久化（SQLite + SeaORM）

`crates/agent-gui` 内嵌的本地存储层，使用 **SeaORM 2.0 + SQLite** 实现。MVP 阶段仅搭建骨架（1 张测试表 + Repository 签名 + 迁移框架），业务表（plans / insights / ...）在阶段 2 加。

## 概述

- 启动时打开 SQLite 数据库文件，按需自动创建父目录
- 通过 `sea-orm-migration` 幂等地应用 schema 迁移
- 通过 Dioxus `Resource<Option<Arc<StorageContext>>>` 上下文注入 GUI
- 失败仅 warn，不阻塞 GUI 启动（与 `ai` / `rag` / `mcp` 等模块保持一致的容错策略）

## 目录结构

```
crates/agent-gui/
├── Cargo.toml                                  # +sea-orm / +sea-orm-migration / +thiserror
├── config.toml                                 # +[storage] 配置段
└── src/
    ├── main.rs                                 # +mod storage; +第 6 个 use_resource
    ├── config.rs                               # +GuiStorageConfig
    ├── context/
    │   ├── mod.rs                              # +pub mod storage; +StorageContext 导出
    │   ├── init_status.rs                      # +storage: ModuleStatus（第 6 个模块）
    │   └── storage.rs                          # ← StorageContext + init + 路径解析
    └── storage/                                # ← 持久化基础设施
        ├── mod.rs                              #   模块入口 + 公共 re-export
        ├── error.rs                            #   StorageError / StorageResult
        ├── entities/
        │   ├── mod.rs
        │   └── test.rs                         #   tests 表 entity
        ├── migrations/
        │   ├── mod.rs                          #   Migrator trait 实现
        │   └── m20260101_000001_create_tests.rs #   首个迁移
        └── repository/
            ├── mod.rs
            └── test_repo.rs                    #   TestRepo（list_all / insert，todo!）
```

## 核心功能

### 1. SQLite 自动初始化

启动时：
1. 解析 DB 文件路径（环境变量 `PLANNED_AGENT_DB_PATH` > 配置 > cwd 拼接）
2. `Database::connect("sqlite://...?mode=rwc")` 建立连接
3. `Migrator::up(&db, None)` 应用全部 pending 迁移（幂等）
4. 构造 1 个 `TestRepo`（MVP 占位）

### 2. 迁移框架

通过 `sea-orm-migration` 管理 schema 版本：
- 启动时自动建表 `seaql_migrations(version PRIMARY KEY, applied_at)`
- 每次启动检查并应用 pending 迁移
- 已应用的迁移**不会**重复执行

### 3. 失败容错

`StorageContext::init` 失败时仅 `tracing::warn!`，UI 通过 `InitStatus.storage.state` 反映模块状态（`Init` / `Ready` / `Failed`）。

## 接口设计

### `GuiStorageConfig`（配置层）

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiStorageConfig {
    /// SQLite 数据库文件路径（相对路径以 cwd 为基准）
    #[serde(default = "default_storage_db_path")]
    pub db_path: String,            // 默认 "./data/agent-gui.db"

    /// 启动时打印 schema 概要（仅调试）
    #[serde(default)]
    pub echo_schema: bool,
}
```

### `StorageContext`（GUI 上下文）

```rust
#[allow(dead_code)] // MVP 占位 —— 阶段 2 业务接入时启用
pub struct StorageContext {
    /// SQLite 连接（SeaORM DatabaseConnection）
    pub db: DatabaseConnection,
    /// tests 表仓库（验证用）
    pub test_repo: Arc<TestRepo>,
}

impl StorageContext {
    /// 从配置异步初始化 SQLite + 迁移 + TestRepo
    pub async fn init(config: &GuiStorageConfig) -> anyhow::Result<Self>;
}
```

### `TestRepo`（MVP 占位）

```rust
pub struct TestRepo {
    db: DatabaseConnection,
}

impl TestRepo {
    pub fn new(db: DatabaseConnection) -> Self;

    /// 列出全部测试记录
    pub async fn list_all(&self) -> StorageResult<Vec<test::Model>>;   // todo!()

    /// 插入一条测试记录（返回自增 id）
    pub async fn insert(&self, _name: String, _value: String) -> StorageResult<i32>; // todo!()
}
```

### `StorageError` / `StorageResult`

```rust
pub enum StorageError {
    Db(DbErr),                // SeaORM 错误
    NotFound(String),         // 记录不存在
    Path(String),             // 路径解析错误
    Migration(String),        // 迁移错误
}

pub type StorageResult<T> = std::result::Result<T, StorageError>;
```

### `Migrator`（迁移入口）

```rust
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260101_000001_create_tests::Migration)]
    }
}
```

## 使用示例

### 1. 业务组件内读取 StorageContext

```rust
use dioxus::prelude::*;
use std::sync::Arc;
use planned_agent_gui::context::StorageContext;

#[component]
fn TestPanel() -> Element {
    // 第 6 个 Resource 的类型与现有 5 个一致
    let storage = use_context::<Resource<Option<Arc<StorageContext>>>>();

    match storage.read().as_ref() {
        Some(Some(ctx)) => {
            // 阶段 2 接入业务时使用
            // let models = ctx.test_repo.list_all().await?;
            rsx! { div { "Storage Ready" } }
        }
        Some(None) => rsx! { div { "Storage 不可用，查看 logs/" } },
        None => rsx! { div { "Storage 初始化中..." } },
    }
}
```

### 2. 检查模块状态

```rust
use planned_agent_gui::context::InitStatus;

let init_status: Signal<InitStatus> = use_context();
let s = init_status.read();

if s.storage.is_ready() {
    // Storage 已就绪
}
if s.all_ready() {
    // 全部 6 个模块都已 Ready
}
```

### 3. 临时切到指定 DB 文件（调试）

```bash
# 环境变量优先级最高，可临时切到指定路径
PLANNED_AGENT_DB_PATH=/tmp/debug.db cargo run -p planned-agent-gui
```

## 配置示例

### `crates/agent-gui/config.toml`

```toml
# ═══════════════════════════════════════════════════════════
# 本地持久化（SQLite via SeaORM）
# ═══════════════════════════════════════════════════════════
[storage]
# SQLite 数据库文件路径（启动时若不存在则自动创建）
db_path = "./data/agent-gui.db"
# 启动时是否打印 schema 概要（调试用；MVP 留 false）
echo_schema = false
```

### `crates/agent-gui/Cargo.toml` 关键依赖

```toml
# SeaORM 2.0 —— 本地 SQLite 持久化
sea-orm = { version = "2.0", features = [
    "sqlx-sqlite",      # SQLite 后端
    "macros",           # DeriveEntityModel / DeriveRelation
    "runtime-tokio",    # 与现有 tokio runtime 对齐
    "with-json",        # JSON 字段（阶段 2 可能用到）
] }

# Migration 框架（关闭默认 cli 特性，避免拉入 clap/dotenvy/sea-orm-cli）
sea-orm-migration = { version = "2.0", default-features = false, features = [
    "sqlx-sqlite",
    "runtime-tokio",
    "with-json",
] }

thiserror = { workspace = true }   # StorageError
```

## 阶段 2 接入业务表

业务接入时只需按以下 4 步操作：

### Step 1：新增 entity

```rust
// crates/agent-gui/src/storage/entities/plan.rs
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "plans")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub title: String,
    pub status: String,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
```

并在 `entities/mod.rs` 加 `pub mod plan;`。

### Step 2：新增 migration

```rust
// crates/agent-gui/src/storage/migrations/m20260803_120000_create_plans.rs
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.create_table(
            Table::create()
                .table(Plans::Table)
                .col(pk_auto(Plans::Id))
                .col(string(Plans::Title))
                .col(string(Plans::Status))
                .col(big_integer(Plans::CreatedAt))
                .to_owned(),
        ).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.drop_table(Table::drop().table(Plans::Table).to_owned()).await
    }
}

#[derive(DeriveIden)]
pub enum Plans { Table, Id, Title, Status, CreatedAt }
```

并在 `migrations/mod.rs` 的 `migrations()` 数组里 `Box::new(...)` 加入。

### Step 3：新增 Repository

```rust
// crates/agent-gui/src/storage/repository/plan_repo.rs
use sea_orm::DatabaseConnection;
use crate::storage::entities::plan;
use crate::storage::error::StorageResult;

pub struct PlanRepo { db: DatabaseConnection }

impl PlanRepo {
    pub fn new(db: DatabaseConnection) -> Self { Self { db } }

    pub async fn list_all(&self) -> StorageResult<Vec<plan::Model>> {
        // todo: 实现
        todo!()
    }
}
```

并在 `repository/mod.rs` 加 `pub mod plan_repo;` + `pub use plan_repo::PlanRepo;`。

### Step 4：在 StorageContext 加字段

```rust
// crates/agent-gui/src/context/storage.rs
pub struct StorageContext {
    pub db: DatabaseConnection,
    pub test_repo: Arc<TestRepo>,
    pub plan_repo: Arc<PlanRepo>,        // ← 新增
    // 后续可继续加：insight_repo / timeline_repo / ...
}

impl StorageContext {
    pub async fn init(config: &GuiStorageConfig) -> anyhow::Result<Self> {
        // ... 现有逻辑 ...
        Ok(Self {
            test_repo: Arc::new(TestRepo::new(db.clone())),
            plan_repo: Arc::new(PlanRepo::new(db.clone())),  // ← 新增
            db,
        })
    }
}
```

业务页面通过 `ctx.plan_repo.list_all().await?` 调用即可。

## 错误处理

| 错误场景 | 行为 | 反映到 UI |
|----------|------|-----------|
| DB 文件路径不可写 | `init` 返回 `Err` → `tracing::warn!` | `InitStatus.storage = Failed` |
| SQLite 打开失败 | `init` 返回 `Err` → `tracing::warn!` | `InitStatus.storage = Failed` |
| Migration 失败 | `init` 返回 `Err` → `tracing::warn!` | `InitStatus.storage = Failed` |
| Repository 调用失败 | 返回 `StorageError::Db(DbErr)` | 由调用方决定 |

**关键设计**：Storage 失败**不会**阻塞 GUI 启动。业务页面应判断 `Option<Arc<StorageContext>>::is_some()` 后再调用 Repository，否则回退到 mock 数据。

## 依赖关系

```
crates/agent-gui
├── sea-orm = 2.0 (sqlx-sqlite, macros, runtime-tokio, with-json)
├── sea-orm-migration = 2.0 (同上，关 cli 默认特性)
├── thiserror                  # StorageError
└── 内置 modules
    ├── storage/               # 持久化基础设施（Entity / Migration / Repository）
    └── context/storage.rs     # GUI 适配层（启动 + 注入）
```

## 设计优势

1. **零额外 crate**：所有 storage 代码位于 `agent-gui` 内部，未新增 workspace 成员
2. **复用既有模式**：6 个 `Resource` 注入与 `InitStatus` 汇总与现有 5 个模块完全对齐
3. **失败容错**：Storage 不可用时 GUI 仍可启动（与 rag/mcp 同策略）
4. **迁移幂等**：`Migrator::up(&db, None)` 配合 `seaql_migrations` 表确保二次启动不重跑
5. **路径灵活**：环境变量 > 配置文件 > 默认值三级优先级
6. **阶段 2 友好**：新增业务表 = 新增 entity + migration + repository + StorageContext 字段，零侵入现有代码

## 验证清单

- [x] 数据库文件 `./data/agent-gui.db` 在首次启动后自动创建
- [x] `data/` 父目录自动 mkdir
- [x] `tests` 表 schema 与设计一致：`id INTEGER PK AUTOINCREMENT, name varchar NOT NULL, value varchar NOT NULL, created_at integer NOT NULL`
- [x] `seaql_migrations` 表记录 `('m20260101_000001_create_tests', <applied_at>)`
- [x] 第二次启动不再 apply 已应用的迁移（幂等）
- [x] `cargo check -p planned-agent-gui` 0 错误
- [x] 业务文件（`pages/`、`services/`）零改动
