//! 会话面板（sessions_panel）—— 会话/版本列表，参照 DeepSeek 侧边栏样式。
//!
//! 本实现为**纯 UI 占位版**（先完成样式，不接数据）：
//! - 顶部搜索框 + 中部可滚动**分组列表** + 底部「新建会话」按钮；
//! - 分组标题（如「默认」「项目」）右侧有**占位操作按钮**；
//! - 每个 item：左侧 = 会话名称(title) + 版本(version，次级色小字)，右侧 = 相对时间；
//! - **选中项**：最左侧一条竖条 + 整行淡色背景（`--focused-border-color` 的同色淡化）。
//!
//! 不使用 `components/item`，列表行/分组均为自定义元素 + 本模块 css。
//!
//! 数据**尚未接入** `plans_flexible_sessions`，暂以 `DEMO_GROUPS` 示例展示布局与交互；
//! 交互仅上抛、不代做业务：点 item → `on_select(id)`；点「新建会话」→ `on_create()`。

use dioxus::prelude::*;
use dioxus_icons::lucide::{Ellipsis, Plus, Search};

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::input::Input;
use crate::components::scroll_area::ScrollArea;
use crate::components::separator::Separator;

#[css_module("/src/pages/plan/sessions_panel/style.css")]
struct Styles;

#[derive(Props, Clone, PartialEq)]
pub struct SessionPanelProps {
    /// 目标计划 id（未来接入 plans_flexible_sessions 数据时使用）。
    pub plan_id: String,
    /// 用户点选某会话（上抛会话 id；行为由挂载方决定）。
    pub on_select: EventHandler<String>,
    /// 用户点击「新建会话」（上抛；行为由挂载方决定）。
    pub on_create: EventHandler<()>,
}

/// 单条示例会话（UI 占位数据；接入真实数据后由 DB 提供）。
struct DemoSession {
    id: &'static str,
    title: &'static str,
    /// 版本展示文本：已定稿为 vX.Y.Z，未定稿为「草稿」。
    version: &'static str,
    /// 未定稿会话版本以弱化样式显示。
    draft: bool,
    /// 距今时长（分钟），用于渲染相对时间。
    minutes_ago: u64,
}

/// 分组（section）：标题 + 其下会话行。
struct DemoGroup {
    name: &'static str,
    items: &'static [DemoSession],
}

/// 占位示例：覆盖分组标题、相对时间各档位。
const DEMO_GROUPS: &[DemoGroup] = &[
    DemoGroup {
        name: "默认",
        items: &[
            DemoSession {
                id: "demo-default-1",
                title: "客户对账生成",
                version: "v2.0.1",
                draft: false,
                minutes_ago: 0,
            },
        ],
    },
    DemoGroup {
        name: "项目",
        items: &[
            DemoSession {
                id: "demo-project-1",
                title: "客户对账生成",
                version: "v1.0.1",
                draft: false,
                minutes_ago: 8 * 60,
            },
            DemoSession {
                id: "demo-project-2",
                title: "客户对账生成2",
                version: "v1.0.0",
                draft: false,
                minutes_ago: 2 * 24 * 60,
            },
            DemoSession {
                id: "demo-project-3",
                title: "批量报表导出（进行中）",
                version: "草稿",
                draft: true,
                minutes_ago: 20 * 24 * 60,
            },
            DemoSession {
                id: "demo-project-4",
                title: "渠道周度销售汇总模板",
                version: "v1.2.0",
                draft: false,
                minutes_ago: 90 * 24 * 60,
            },
        ],
    },
];

/// 会话面板：顶部搜索 → 中部可滚动分组列表 → 底部「新建会话」。
#[component]
pub fn SessionPanel(props: SessionPanelProps) -> Element {
    // 占位读取 plan_id（保持接口；接数据后使用），避免未用告警。
    let _plan_id = props.plan_id.clone();

    // 本地 UI 态：搜索词 + 当前选中（真实数据接入前的高亮代理）。
    let mut query = use_signal_sync(String::new);
    // 预置一个选中项，便于预览竖条 + 背景高亮效果。
    let selected = use_signal_sync(|| Some("demo-project-1".to_string()));

    // 依据搜索词过滤（大小写不敏感）；空分组隐藏。
    let q = query.read().trim().to_lowercase();
    let shown: Vec<(&'static str, Vec<&'static DemoSession>)> = DEMO_GROUPS
        .iter()
        .map(|g| {
            let items: Vec<&DemoSession> = g
                .items
                .iter()
                .filter(|s| q.is_empty() || s.title.to_lowercase().contains(&q))
                .collect();
            (g.name, items)
        })
        .filter(|(_, items)| !items.is_empty())
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

            // ── 中部：可滚动的分组列表 ──
            div { class: Styles::scroll,
                ScrollArea {
                    id: "sessions-panel-scroll",
                    div { class: Styles::list,
                        if shown.is_empty() {
                            div { class: Styles::empty, "没有匹配的会话" }
                        } else {
                            for (idx, (name, items)) in shown.into_iter().enumerate() {
                                if idx > 0 {
                                    div { class: Styles::group_sep, Separator { horizontal: true } }
                                }
                                div { class: Styles::group,
                                    // 分组标题 + 右侧占位操作按钮
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
                                    for item in items {
                                        { render_row(item, selected, props.on_select.clone()) }
                                    }
                                }
                            }
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
                    onclick: move |_| props.on_create.call(()),
                    Plus { size: "15" }
                    span { " 新建会话" }
                }
            }
        }
    }
}

/// 渲染单个会话行：左 = title + version（同行），右 = 相对时间。
fn render_row(
    item: &DemoSession,
    selected: Signal<Option<String>, SyncStorage>,
    on_select: EventHandler<String>,
) -> Element {
    let sid = item.id.to_string();
    let is_active = selected.read().as_deref() == Some(sid.as_str());
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
                let mut sel = selected;
                sel.set(Some(sid.clone()));
                on_select.call(sid.clone());
            },
            div { class: Styles::row_main,
                span { class: Styles::row_title, "{item.title}" }
                span { class: version_cls, "{item.version}" }
            }
            span { class: Styles::row_time, "{format_relative(item.minutes_ago)}" }
        }
    }
}

/// 相对时间：刚刚 / 分钟 / 小时 / 天 / 周 / 月。
fn format_relative(minutes: u64) -> String {
    const HOUR: u64 = 60;
    const DAY: u64 = 24 * HOUR;
    const WEEK: u64 = 7 * DAY;
    const MONTH: u64 = 30 * DAY;
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
