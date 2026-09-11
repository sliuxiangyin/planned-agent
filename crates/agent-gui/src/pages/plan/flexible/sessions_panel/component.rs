//! 会话面板（sessions_panel）—— 会话/版本列表，参照 DeepSeek 侧边栏样式。
//!
//! 结构：顶部搜索框 + 中部可滚动**分组列表** + 底部「新建会话」按钮。
//! 分组**固定为两个**：「默认」与「项目」。
//! - **项目**：接入 `plans_flexible_sessions` 真实数据（`list_by_plan(plan_id, …)`）；
//! - **默认**：暂无数据源，保留分组占位。
//!
//! 每个 item：左侧 = 会话名称(title) + 版本(version，次级色小字；未定稿显示「草稿」)，
//! 右侧 = 相对时间（由 `created_at` 计算）。**选中项**：整行淡色背景。
//!
//! 会话 id 与 `FlexiblePage` 一致：本组件自持 `Signal<String>`（初值来自 props），
//! 经 `use_listen_session_manager` 跟随 `SessionManager` 的「当前会话」变化；
//! **点选某行即通过 `SessionManager::set_active` 修改当前会话**（不向上抛事件）。
//!
//! **新建会话**：点「新建会话」→ 弹 `Dialog` 输入标题 → 确定/取消；
//! 确定后调用 `PlansFlexibleSessionsRepo::create` 落库，**成功后 restart 列表 resource 刷新**。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use dioxus_icons::lucide::{Ellipsis, Plus, Search};

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::dialog::{Dialog, DialogDescription, DialogTitle};
use crate::components::input::Input;
use crate::components::scroll_area::ScrollArea;
use crate::components::separator::Separator;
use crate::context::StorageContext;
use crate::pages::plan::shared::session::{use_listen_session_manager, SessionManager};
use crate::storage::entities::plans_flexible_sessions;
use crate::storage::repository::plans_flexible_sessions_repo::status;

#[css_module("/src/pages/plan/sessions_panel/style.css")]
struct Styles;

/// 固定分组标题。
const GROUP_DEFAULT: &str = "默认";
const GROUP_PROJECT: &str = "项目";

#[derive(Props, Clone, PartialEq)]
pub struct SessionPanelProps {
    /// 目标计划 id（用于拉取/创建该 plan 的会话/版本）。
    pub plan_id: String,
    /// 当前会话 id（初值；随后跟随 `SessionManager` 的当前会话变化）。
    pub session_id: String,
}

/// 单条会话行（从 `plans_flexible_sessions::Model` 派生的展示数据）。
#[derive(Clone, PartialEq)]
struct RowData {
    id: String,
    title: String,
    /// 版本展示文本：已定稿为 vX.Y.Z，未定稿为「草稿」。
    version: String,
    /// 未定稿会话版本以弱化样式显示。
    draft: bool,
    /// 创建时间（RFC3339），用于渲染相对时间。
    created_at: String,
}

impl RowData {
    fn from_model(m: plans_flexible_sessions::Model) -> Self {
        // active（进行中）= 未定稿草稿；produced/abandoned 视为已定稿展示版本号。
        let draft = m.status == status::ACTIVE;
        Self {
            id: m.id,
            title: m.title,
            version: if draft { "草稿".to_string() } else { m.version },
            draft,
            created_at: m.created_at,
        }
    }
}

/// 会话面板：顶部搜索 → 中部可滚动分组列表 → 底部「新建会话」。
#[component]
pub fn SessionPanel(props: SessionPanelProps) -> Element {
    // ── 全局 Context：storage 由启动门保证就绪 ──
    let storage: Arc<StorageContext> = use_context();
    let sessions_repo = storage.plans_flexible_sessions_repo();
    // 会话状态管理中心：点选行即写入其「当前会话」。
    let session_mgr = use_context::<Arc<SessionManager>>();

    // ── 会话 id 信号：初值来自 props，随后跟随 SessionManager（与 FlexiblePage 一致） ──
    let initial_session_id = props.session_id.clone();
    let session_id = use_signal(move || initial_session_id.clone());
    use_listen_session_manager(session_id);

    // ── 按 plan_id 拉取「项目」分组的会话/版本列表（search 走本地过滤，此处拉全量） ──
    let pid = props.plan_id.clone();
    let list_repo = sessions_repo.clone();
    let sessions_resource = use_resource(move || {
        let pid = pid.clone();
        let repo = list_repo.clone();
        async move { repo.list_by_plan(&pid, None).await }
    });

    // 三态：Pending 视为加载中；Err 静默为空（列表页不阻塞）；Ready 取数据。
    let loading = sessions_resource.value().read().is_none();
    let all_rows: Vec<RowData> = match &*sessions_resource.value().read_unchecked() {
        Some(Ok(list)) => list.iter().cloned().map(RowData::from_model).collect(),
        _ => Vec::new(),
    };

    // 本地 UI 态：搜索词。
    let mut query = use_signal_sync(String::new);

    // ── 新建会话弹窗态 ──
    let mut show_create = use_signal_sync(|| false);
    let mut new_title = use_signal_sync(String::new);
    let mut create_error = use_signal_sync(|| None::<String>);

    // 弹窗「确定」：校验 → create 落库 → 成功则 restart 列表 resource 刷新并关窗；
    // 失败保留弹窗并提示错误。
    let on_create_confirm = {
        let repo = sessions_repo.clone();
        let pid = props.plan_id.clone();
        move |_: MouseEvent| {
            let title = new_title.read().trim().to_string();
            if title.is_empty() {
                create_error.set(Some("会话标题不能为空".into()));
                return;
            }
            let repo = repo.clone();
            let pid = pid.clone();
            // Resource 为 Copy：复制一份进 async 任务，不影响渲染侧继续读同一资源。
            let mut resource = sessions_resource;
            spawn(async move {
                match repo.create(&pid, &title).await {
                    Ok(_) => {
                        resource.restart();
                        show_create.set(false);
                        new_title.set(String::new());
                        create_error.set(None);
                    }
                    Err(e) => create_error.set(Some(format!("创建失败：{e}"))),
                }
            });
        }
    };

    // 弹窗「取消」：清空输入并关窗。
    let on_create_cancel = move |_: MouseEvent| {
        new_title.set(String::new());
        create_error.set(None);
        show_create.set(false);
    };

    // 依据搜索词过滤（大小写不敏感，匹配 title 或 version）。
    let q = query.read().trim().to_lowercase();
    let project_rows: Vec<RowData> = all_rows
        .into_iter()
        .filter(|r| {
            q.is_empty()
                || r.title.to_lowercase().contains(&q)
                || r.version.to_lowercase().contains(&q)
        })
        .collect();

    rsx! {
        div { class: Styles::panel,
            // ── 顶部：搜索 ──
            div { class: Styles::searchbar,
                Search { size: "14" }
                Input {
                    placeholder: "搜索会话…",
                    oninput: move |e: FormEvent| query.set(e.value()),
                    class: Styles::search_input,
                }
            }

            // ── 中部：可滚动的分组列表（固定两组：默认 / 项目） ──
            div { class: Styles::scroll,
                ScrollArea {
                    id: "sessions-panel-scroll",
                    div { class: Styles::list,
                        if loading {
                            div { class: Styles::empty, "加载中…" }
                        } else {
                            // 「默认」分组：暂无数据源，保留分组占位
                            { render_group(GROUP_DEFAULT, &[], session_id, session_mgr.clone()) }

                            // 分组分隔线
                            div { class: Styles::group_sep, Separator { horizontal: true } }

                            // 「项目」分组：plans_flexible_sessions 真实数据
                            { render_group(GROUP_PROJECT, &project_rows, session_id, session_mgr.clone()) }
                        }
                    }
                }
            }

            // ── 底部分隔线（统一用 Separator） + 新建会话 ──
            Separator { horizontal: true }
            div { class: Styles::footer,
                Button {
                    variant: ButtonVariant::Secondary,
                    size: ButtonSize::Default,
                    style: "width: 100%;",
                    onclick: move |_| show_create.set(true),
                    Plus { size: "15" }
                    span { " 新建会话" }
                }
            }
        }

        // ── 新建会话弹窗 ──
        Dialog {
            open: show_create(),
            // 关闭请求一律忽略（点击遮罩 / ESC 不关闭），仅由取消/确定按钮关窗。
            on_open_change: move |open: bool| {
                if open {
                    return;
                }
            },
            DialogTitle { "新建会话" }
            DialogDescription {
                class: Styles::create_dialog_body,
                div { class: Styles::create_dialog_field,
                    label { class: Styles::create_dialog_label, "会话标题" }
                    Input {
                        placeholder: "输入会话标题…",
                        value: "{new_title}",
                        oninput: move |e: FormEvent| {
                            new_title.set(e.value());
                            create_error.set(None);
                        },
                    }
                }
                if let Some(ref err) = *create_error.read() {
                    div { class: Styles::create_dialog_error, "{err}" }
                }
            }
            div { class: Styles::create_dialog_footer,
                Button {
                    variant: ButtonVariant::Outline,
                    size: ButtonSize::Sm,
                    onclick: on_create_cancel,
                    "取消"
                }
                Button {
                    variant: ButtonVariant::Primary,
                    size: ButtonSize::Sm,
                    onclick: on_create_confirm,
                    "确定"
                }
            }
        }
    }
}

/// 渲染一个分组：标题行（+ 右侧占位操作按钮）与若干会话行；空时显示占位。
fn render_group(
    name: &str,
    items: &[RowData],
    session_id: Signal<String>,
    session_mgr: Arc<SessionManager>,
) -> Element {
    rsx! {
        div { class: Styles::group,
            div { class: Styles::group_header,
                span { class: Styles::group_title, "{name}" }
                Button {
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::IconSm,
                    class: Styles::group_action,
                    // 占位：仅样式，无行为
                    Ellipsis { size: "14" }
                }
            }
            if items.is_empty() {
                div { class: Styles::group_empty, "暂无会话" }
            } else {
                for item in items {
                    { render_row(item, session_id, session_mgr.clone()) }
                }
            }
        }
    }
}

/// 渲染单个会话行：左 = title + version（同行），右 = 相对时间。
fn render_row(item: &RowData, session_id: Signal<String>, session_mgr: Arc<SessionManager>) -> Element {
    let sid = item.id.clone();
    // 高亮当前会话：以组件的 session_id 信号为准（与 SessionManager 双向一致）。
    let is_active = session_id.read().as_str() == sid.as_str();
    // 选中态是「叠加」class，而非替换：始终保留 .row（布局 + 内边距），
    // 选中再追加 .row_selected（仅背景色）。
    let row_cls = if is_active {
        format!("{} {}", Styles::row, Styles::row_selected)
    } else {
        Styles::row.to_string()
    };
    let version_cls = if item.draft {
        Styles::version_draft
    } else {
        Styles::version
    };

    rsx! {
        div {
            class: row_cls,
            role: "button",
            onclick: move |_| {
                // 走 SessionManager 修改「当前会话」（唯一写入入口）；
                // 同步更新本地 session_id 以即时高亮，随后 use_listen 会保持一致。
                session_mgr.set_active(sid.clone());
                let mut s = session_id;
                s.set(sid.clone());
            },
            div { class: Styles::row_main,
                span { class: Styles::row_title, "{item.title}" }
                span { class: version_cls, "{item.version}" }
            }
            span { class: Styles::row_time, "{format_relative(&item.created_at)}" }
        }
    }
}

/// 相对时间：刚刚 / 分钟 / 小时 / 天 / 周 / 月（由 RFC3339 时间戳计算）。
fn format_relative(created_at: &str) -> String {
    const HOUR: u64 = 60;
    const DAY: u64 = 24 * HOUR;
    const WEEK: u64 = 7 * DAY;
    const MONTH: u64 = 30 * DAY;

    let minutes = DateTime::parse_from_rfc3339(created_at)
        .map(|t| (Utc::now() - t.with_timezone(&Utc)).num_minutes().max(0) as u64)
        .unwrap_or(0);

    if minutes < 1 {
        "刚刚".to_string()
    } else if minutes < HOUR {
        format!("{minutes}分钟")
    } else if minutes < DAY {
        format!("{}小时", minutes / HOUR)
    } else if minutes < WEEK {
        format!("{}天", minutes / DAY)
    } else if minutes < MONTH {
        format!("{}周", minutes / WEEK)
    } else {
        format!("{}个月", minutes / MONTH)
    }
}
