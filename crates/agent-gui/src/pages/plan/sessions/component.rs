//! 会话/版本列表抽屉的内容体 —— 纯列表组件，不含弹层外壳。
//!
//! 弹层外壳（`Sheet` / 动画 / 遮罩 / 关闭按钮）由挂载方（`flexible/page.rs`）
//! 渲染并受控；本组件只在被挂载时用 `use_resource` 拉一次列表，卸载即取消，
//! 因此不接收 open、不做加载 gate。
//!
//! 与 `pages/plan/flexible` 零耦合：只依赖 `StorageContext`（取 session /
//! plans_flexible 快照做命名映射）与共享的 `SessionManager`（读当前会话做高亮），
//! 不触碰 ChatService / controller 的类型。
//!
//! 命名约定：
//! - 已有产出版本的会话（`session → plans_flexible.version`）→ `v{version}`
//! - 尚无产出版本的会话（active 草稿 / abandoned）→ 「草稿」并弱化显示
//!
//! 点击某会话：仅通过 `on_select(session_id)` 上抛，由挂载方决定后续动作；
//! 高亮始终以 `SessionManager.current()` 为准，不在组件内自存「当前」。

use dioxus::prelude::*;
use dioxus_icons::lucide::{Check, Clock};

use crate::context::StorageContext;
use crate::pages::plan::shared::session::SessionManager;
use std::sync::Arc;

#[css_module("/src/pages/plan/sessions/style.css")]
struct Styles;

/// 抽屉内一行会话的数据（命名依据在加载时换算好，渲染零计算）。
#[derive(Debug, Clone)]
struct SessionRow {
    session_id: String,
    /// 展示名：`v{n}` / 「草稿」；abandoned 会在渲染时加弱化前缀。
    version: Option<i32>,
    status: String,
    created_at: String,
}

#[derive(Props, Clone, PartialEq)]
pub struct SessionListSheetProps {
    /// 目标计划 id（以它查会话与版本）。
    pub plan_id: String,
    /// 用户点选某会话（会话 id）。
    pub on_select: EventHandler<String>,
}

/// 会话/版本列表抽屉的内容体。被挂载即拉取列表（`use_resource`），卸载即取消。
#[component]
pub fn SessionListSheet(props: SessionListSheetProps) -> Element {
    // 数据取自 context（全局注入）。
    let storage: Arc<StorageContext> = use_context();
    let session_mgr: Arc<SessionManager> = use_context();
    let current = session_mgr.current();

    // ── 数据加载：挂载即拉一次。组件不接 open、不做 gate，卸载时 resource 自动取消。
    let plan_id = props.plan_id.clone();
    let session_repo = storage.session_repo();
    let plans_flexible_repo = storage.plans_flexible_repo();
    let sessions = use_resource(move || {
        let pid = plan_id.clone();
        let srepo = session_repo.clone();
        let prepo = plans_flexible_repo.clone();
        async move {
            match load_rows(pid, srepo, prepo).await {
                Ok(list) => Ok(list),
                Err(e) => Err(e.to_string()),
            }
        }
    });

    rsx! {
        div { class: Styles::session_body,
            match sessions.value().cloned() {
                // 未就绪 → 加载中
                None => rsx! { div { class: Styles::session_hint, "加载中…" } },
                Some(Err(err)) => rsx! { div { class: Styles::session_hint, "{err}" } },
                Some(Ok(list)) => {
                    let rows = list.clone();
                    rsx! {
                        div { class: Styles::session_list,
                            for row in rows {
                                { render_row(&row, current, props.on_select.clone()) }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn render_row(
    row: &SessionRow,
    current: Signal<Option<String>, SyncStorage>,
    on_select: EventHandler<String>,
) -> Element {
    let is_current = current().as_deref() == Some(row.session_id.as_str());
    let (primary, weak) = display_name(row);
    let status_label = status_text(row);
    let mut row_cls = Styles::session_item;
    if is_current {
        row_cls = Styles::session_item_active;
    }

    let sid = row.session_id.clone();
    rsx! {
        button {
            class: row_cls,
            onclick: move |_| {
                on_select.call(sid.clone());
            },
            div { class: Styles::session_item_name,
                if weak {
                    span { class: Styles::session_item_weak, "{primary}" }
                } else {
                    span { "{primary}" }
                }
                if is_current {
                    span { class: Styles::session_item_current_mark,
                        Check { size: "14" }
                    }
                }
            }
            div { class: Styles::session_item_meta,
                span { "{status_label}" }
                span { class: Styles::session_item_sep, "·" }
                span { class: Styles::session_item_time,
                    Clock { size: "12" }
                    "{created_short(&row.created_at)}"
                }
            }
        }
    }
}

/// 生成展示名：Some(v) → "v{n}"，None → "草稿"。返回 (主文案, 是否弱化草稿)。
fn display_name(row: &SessionRow) -> (String, bool) {
    match row.version {
        Some(v) => (format!("v{v}"), false),
        None => ("草稿".to_string(), true),
    }
}

fn status_text(row: &SessionRow) -> String {
    match row.status.as_str() {
        "produced" => "已产出".to_string(),
        "abandoned" => "已废弃".to_string(),
        _ => "进行中".to_string(),
    }
}

fn created_short(ts: &str) -> String {
    // 简单截取 RFC3339 的日期时间部分，避免引入 chrono 依赖到 UI。
    ts.get(5..16).unwrap_or(ts).to_string()
}

/// 加载某 plan 的全部会话并按 plans_flexible 反查版本号。
async fn load_rows(
    plan_id: String,
    session_repo: Arc<crate::storage::repository::session_repo::SessionRepo>,
    plans_flexible_repo: Arc<crate::storage::repository::plans_flexible_repo::PlansFlexibleRepo>,
) -> Result<Vec<SessionRow>, Box<dyn std::error::Error>> {
    let sessions = session_repo.find_by_plan_id(&plan_id).await?;
    // session_id → version（1:1，见 plans_flexible 设计）
    let versions: std::collections::HashMap<String, i32> = plans_flexible_repo
        .list_versions_by_plan(&plan_id)
        .await?
        .into_iter()
        .collect();

    // 按创建时间倒序（最新在前）；abandoned 沉底。
    let mut list: Vec<SessionRow> = sessions
        .into_iter()
        .map(|s| SessionRow {
            session_id: s.id.clone(),
            version: versions.get(&s.id).copied(),
            status: s.status.clone(),
            created_at: s.created_at.clone(),
        })
        .collect();
    list.sort_by_key(|r| r.status == "abandoned"); // false < true → abandoned 在后
    Ok(list)
}
