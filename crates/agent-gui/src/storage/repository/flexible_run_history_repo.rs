//! flexible_run_history 表仓库 — 每步「上次成功用过的工具链」记忆。
//!
//! 由 `(session_id, result_reference)` 唯一定位（一个会话的同一步骤至多一行），
//! 写入走 upsert 覆盖；读取按 `(session_id, revision)` 取「当前这一代」的行
//! （`revision` 不匹配的旧行不注入，也不残留成第二行）。

use chrono::Utc;
use sea_orm::*;
use uuid::Uuid;

use crate::storage::entities::flexible_run_history;
use crate::storage::error::StorageResult;

/// flexible_run_history 表仓库
pub struct FlexibleRunHistoryRepo {
    db: DatabaseConnection,
}

impl FlexibleRunHistoryRepo {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// 按 session_id + revision 列出「这一代」的全部步骤记忆。
    ///
    /// 注入侧只读这个结果，再按当前模板的 `result_reference` 对齐。
    pub async fn list_by_session_revision(
        &self,
        session_id: &str,
        revision: i32,
    ) -> StorageResult<Vec<flexible_run_history::Model>> {
        let rows = flexible_run_history::Entity::find()
            .filter(flexible_run_history::Column::SessionId.eq(session_id))
            .filter(flexible_run_history::Column::Revision.eq(revision))
            .all(&self.db)
            .await?;
        Ok(rows)
    }

    /// 按 session_id + result_reference 查一行（唯一键的前两列）。
    pub async fn find_by_session_reference(
        &self,
        session_id: &str,
        result_reference: &str,
    ) -> StorageResult<Option<flexible_run_history::Model>> {
        let model = flexible_run_history::Entity::find()
            .filter(flexible_run_history::Column::SessionId.eq(session_id))
            .filter(flexible_run_history::Column::ResultReference.eq(result_reference))
            .one(&self.db)
            .await?;
        Ok(model)
    }

    /// 写入某一步的工具链：`(session_id, result_reference)` 已存在则**覆盖**
    /// （含 `revision` 与 `tool_chain`），否则插入。返回更新后的 Model。
    ///
    /// 覆盖也刷新 `revision` —— 于是旧一代的行会被新一次执行「改籍」，不会留成第二行。
    pub async fn upsert(
        &self,
        plan_id: &str,
        session_id: &str,
        result_reference: &str,
        revision: i32,
        tool_chain: &str,
    ) -> StorageResult<flexible_run_history::Model> {
        let now = Utc::now().to_rfc3339();
        let existing = self
            .find_by_session_reference(session_id, result_reference)
            .await?;

        let res = match existing {
            Some(model) => {
                let mut am: flexible_run_history::ActiveModel = model.into();
                am.plan_id = Set(plan_id.to_string());
                am.revision = Set(revision);
                am.tool_chain = Set(tool_chain.to_string());
                am.updated_at = Set(now);
                am.update(&self.db).await?
            }
            None => {
                let model = flexible_run_history::ActiveModel {
                    id: Set(Uuid::new_v4().to_string()),
                    plan_id: Set(plan_id.to_string()),
                    session_id: Set(session_id.to_string()),
                    revision: Set(revision),
                    result_reference: Set(result_reference.to_string()),
                    tool_chain: Set(tool_chain.to_string()),
                    created_at: Set(now.clone()),
                    updated_at: Set(now),
                };
                model.insert(&self.db).await?
            }
        };
        Ok(res)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::entities::plans_flexible_sessions;
    use crate::storage::migrations::Migrator;
    use crate::storage::repository::{PlanRepo, PlansFlexibleSessionsRepo};
    use sea_orm::{ConnectOptions, Database, DatabaseConnection};
    use sea_orm_migration::MigratorTrait;

    /// 内存 SQLite（`max_connections(1)` —— in-memory 每连接是独立库，多连接会各看到空库），
    /// 跑全部迁移，并造一对 plan + session 父行（满足外键）。
    async fn setup() -> (DatabaseConnection, String, String) {
        let mut opt = ConnectOptions::new("sqlite::memory:");
        opt.max_connections(1).min_connections(1).sqlx_logging(false);
        let db = Database::connect(opt).await.expect("connect");
        Migrator::up(&db, None).await.expect("migrate");

        let plan = PlanRepo::new(db.clone())
            .create("t", "flexible")
            .await
            .expect("plan");
        let session = PlansFlexibleSessionsRepo::new(db.clone())
            .create(&plan.id, "s")
            .await
            .expect("session");
        (db, plan.id, session.id)
    }

    #[tokio::test]
    async fn migrate_creates_table_and_revision_column() {
        let (db, _plan_id, session_id) = setup().await;

        // 迁移 1：会话表新列 revision 存在，新会话默认 0
        let session = plans_flexible_sessions::Entity::find_by_id(&session_id)
            .one(&db)
            .await
            .expect("query session")
            .expect("session row");
        assert_eq!(session.revision, 0);

        // 迁移 2：新表可查（初始为空）
        let repo = FlexibleRunHistoryRepo::new(db);
        assert!(repo
            .list_by_session_revision(&session_id, 0)
            .await
            .expect("list")
            .is_empty());
    }

    #[tokio::test]
    async fn upsert_overwrites_same_step_and_keeps_one_row() {
        let (db, plan_id, session_id) = setup().await;
        let repo = FlexibleRunHistoryRepo::new(db);

        repo.upsert(&plan_id, &session_id, "#E1", 1, r#"[{"tool":"read_file"}]"#)
            .await
            .expect("insert");
        repo.upsert(&plan_id, &session_id, "#E1", 1, r#"[{"tool":"write_file"}]"#)
            .await
            .expect("update");

        let rows = repo
            .list_by_session_revision(&session_id, 1)
            .await
            .expect("list");
        assert_eq!(rows.len(), 1, "同一步骤只留最新一份");
        assert!(rows[0].tool_chain.contains("write_file"));
    }

    #[tokio::test]
    async fn next_revision_overwrites_and_old_generation_disappears() {
        let (db, plan_id, session_id) = setup().await;
        let repo = FlexibleRunHistoryRepo::new(db);

        repo.upsert(&plan_id, &session_id, "#E1", 1, "[]")
            .await
            .expect("gen 1");
        // 计划内容换过一代（revision +1）：同一 #E1 写入会「改籍」到新一代
        repo.upsert(&plan_id, &session_id, "#E1", 2, r#"[{"tool":"ls"}]"#)
            .await
            .expect("gen 2");

        // 旧一代查询为空 —— 旧行不是留成第二条，而是被覆盖掉了
        assert!(repo
            .list_by_session_revision(&session_id, 1)
            .await
            .expect("list gen1")
            .is_empty());
        let current = repo
            .list_by_session_revision(&session_id, 2)
            .await
            .expect("list gen2");
        assert_eq!(current.len(), 1);
        assert!(current[0].tool_chain.contains("ls"));
    }
}
