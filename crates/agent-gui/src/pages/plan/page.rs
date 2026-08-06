//! Plan 页面主组件：组合左侧计划详情面板与右侧聊天面板。
//!
//! 计划模式在创建时固定，不可在聊天中切换。

use std::sync::Arc;

use crate::components::alert_dialog::{
    AlertDialog, AlertDialogAction, AlertDialogActions, AlertDialogCancel, AlertDialogDescription,
    AlertDialogTitle,
};
use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::dropdown_menu::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
};
use crate::components::page_header::PageHeader;
use crate::components::resizable_panel::ResizablePanel;
use crate::components::textarea::Textarea;
use crate::components::tooltip::{Tooltip, TooltipContent, TooltipTrigger};
use dioxus::prelude::*;
use dioxus_primitives::ContentSide;
use planned_agent_core::types::{Message, MessageContent, MessageRole};

use crate::context::{InitStatus, ModuleState, StorageContext};
use crate::services::chat_service::{use_chat_service, ChatServiceSignal};
use crate::storage::repository::{MessageRepo, PlanFlexibleRepo, PlanRepo};
use planned_agent::ChatService;
use planned_agent_prompt_manager::FilePromptManager;

use super::components::chat_ui_actions_view::ChatUIActionsView;
use super::components::plan_todo_view::PlanTodoView;

use super::chat::{handle_user_action, send_message};
use super::message::MessageListView;
use super::types::{
    ChatState, ParamDef, PendingUIState, PlanGeneratedEvent, PlanInfo, PlanSource, PlanState,
};

/// 本页面专属样式（按需加载）。
const PLAN_CSS: Asset = asset!("/assets/plan.css");
/// 左侧面板 Bento 瓷块样式。
const LEFT_PANEL_CSS: Asset = asset!("/assets/plan-left-panel.css");
/// ResizablePanel 所需样式（按需加载）。
const RESIZABLE_CSS: Asset = asset!("/assets/resizable_panel.css");

#[component]
pub fn PlanPage(plan_id: String, on_back: EventHandler<()>) -> Element {
    // ── 全局 Context ──
    let init_status = use_context::<Memo<InitStatus>>();
    let storage = use_context::<Resource<Option<Arc<StorageContext>>>>();

    // ── 计划信息（从 DB 异步加载） ──
    let plan_info = use_signal_sync(|| None::<PlanInfo>);

    // ── 聊天状态 ──
    let messages = use_signal_sync(|| vec![]);
    let input_text = use_signal_sync(String::new);
    let streaming_idx = use_signal_sync(|| None::<usize>);
    let pending_ui = use_signal_sync(|| None::<PendingUIState>);
    let reasoning_texts = use_signal_sync(|| Vec::<Option<String>>::new());

    // ── 计划模式（从 DB 加载后固定） ──
    let plan_mode = use_signal_sync(|| None::<String>);

    // ── 计划生成事件 ──
    let plan_generated = use_signal_sync(|| None::<PlanGeneratedEvent>);

    // ── 计划版本号（save_flexible_plan 成功后递增，通知 PlanTodoView 重新加载） ──
    let plan_version = use_signal_sync(|| 0u32);

    // ── 已固化的参数定义（清晰度检查勾选后暂存，确认生成时随事件落库） ──
    let plan_params = use_signal_sync(Vec::<ParamDef>::new);

    // ── 自动需求开关（灵活模式：开启后 AI 自动确认模糊需求，无需用户手动点击） ──
    let auto_requirement = use_signal_sync(|| false);

    // ── 删除确认弹窗 ──
    let mut show_delete_dialog = use_signal_sync(|| false);

    // ── 清除消息确认弹窗 ──
    let mut show_clear_dialog = use_signal_sync(|| false);

    // ── 分组状态结构体：收敛所有信号，后续函数仅传入 struct ──
    let mut chat = ChatState {
        messages,
        reasoning_texts,
        streaming_idx,
        pending_ui,
        input_text,
    };
    let plan = PlanState {
        plan_info,
        plan_mode,
        plan_generated,
        plan_version,
        plan_params,
    };

    // ── 加载计划元数据 + 历史消息 ──
    let pid = plan_id.clone();
    use_effect(move || {
        let storage_opt = storage.read().as_ref().and_then(|x| x.as_ref()).cloned();
        if let Some(ctx) = storage_opt {
            spawn(load_plan_data(
                pid.clone(),
                ctx.plan_repo.clone(),
                ctx.message_repo.clone(),
                chat,
                plan,
            ));
        }
    });

    // ── 根据 plan_mode 派生 system prompt 模板路径 ──
    let system_prompt_template = use_memo(move || {
        plan.plan_mode
            .read()
            .as_ref()
            .map(|mode| match mode.as_str() {
                "flexible" => "chat/flexible_system".to_string(),
                "thorough" => "chat/thorough_system".to_string(),
                _ => "chat/thorough_system".to_string(),
            })
    });

    // ── Chat Service ──
    let chat_signal = use_chat_service(system_prompt_template.into());

    // ── 灵活模式确认生成：提炼 CoarseGrainedPlan 并保存到 plans_flexible ──
    {
        let chat_signal = chat_signal;
        let storage = storage.clone();
        let chat = chat;
        let plan = plan;
        let pid = plan_id.clone();
        use_effect(move || {
            if let Some(ref event) = *plan.plan_generated.read() {
                if event.source == PlanSource::Flexible && !event.plan_text.is_empty() {
                    let storage_opt = storage.read().as_ref().and_then(|x| x.as_ref()).cloned();
                    let chat_opt = (*chat_signal.read()).clone();
                    if let (Some(ctx), Some(svc)) = (storage_opt, chat_opt) {
                        spawn(save_flexible_plan(
                            svc,
                            pid.clone(),
                            event.plan_text.clone(),
                            event.params.clone(),
                            ctx.plan_repo.clone(),
                            ctx.plan_flexible_repo.clone(),
                            ctx.message_repo.clone(),
                            chat,
                            plan,
                        ));
                    }
                }
            }
        });
    }

    // ── 按钮可用性 ──
    let can_create = init_status.read().ai.state == ModuleState::Ready
        && init_status.read().prompt.state == ModuleState::Ready;

    // ── 获取 message_repo / plan_repo 用于持久化与删除 ──
    let message_repo = storage
        .read()
        .as_ref()
        .and_then(|x| x.as_ref())
        .map(|ctx| ctx.message_repo.clone());
    let plan_repo = storage
        .read()
        .as_ref()
        .and_then(|x| x.as_ref())
        .map(|ctx| ctx.plan_repo.clone());

    // ── 清除消息记录回调：乐观清空内存信号，异步删除 DB 记录 ──
    let on_confirm_clear = {
        let pid = plan_id.clone();
        let repo = message_repo.clone();
        move |_: ()| {
            chat.clear();
            let pid = pid.clone();
            let repo = repo.clone();
            spawn(async move {
                if let Some(ref repo) = repo {
                    if let Err(e) = repo.delete_by_plan_id(&pid).await {
                        tracing::error!("清除消息失败: {}", e);
                    }
                }
            });
        }
    };

    // ── 右侧聊天面板 ──
    let chat_panel = render_chat_panel(
        plan_id.clone(),
        storage.clone(),
        chat,
        plan,
        chat_signal,
        can_create,
        message_repo.clone(),
        show_clear_dialog,
        auto_requirement,
    );

    // ── 左侧计划详情面板 ──
    let plan_name = plan
        .plan_info
        .read()
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| format!("计划 {}", plan_id));
    let plan_mode_label = plan
        .plan_info
        .read()
        .as_ref()
        .map(|p| match p.mode.as_str() {
            "thorough" => "周密模式".to_string(),
            _ => "灵活模式".to_string(),
        })
        .unwrap_or_default();
    let plan_status_label = plan
        .plan_info
        .read()
        .as_ref()
        .map(|p| match p.status.as_str() {
            "generated" => "已生成".to_string(),
            _ => "待生成".to_string(),
        })
        .unwrap_or_default();
    let status_chip_class = plan
        .plan_info
        .read()
        .as_ref()
        .map(|p| match p.status.as_str() {
            "generated" => "header-chip--status--generated",
            _ => "header-chip--status--pending",
        })
        .unwrap_or("header-chip--status--pending");

    rsx! {
        document::Stylesheet { href: PLAN_CSS }
        document::Stylesheet { href: LEFT_PANEL_CSS }
        document::Stylesheet { href: RESIZABLE_CSS }
        div { class: "plan-page",
            ResizablePanel {
                initial_left_percent: 70.0,
                min_left_percent: 25.0,
                max_left_percent: 75.0,
                left: rsx! {
                    div { class: "plan-left-panel",
                        // ── Header topbar：返回 + 计划名称（PageHeader 组件） ──
                        PageHeader {
                            title: plan_name.clone(),
                            on_back: Some(on_back),
                            class: Some("dx-page-header--nested".to_string()),
                            actions: Some(rsx! {
                                // ① 模式 chip
                                span {
                                    class: "header-chip header-chip--mode",
                                    "{plan_mode_label}"
                                }
                                // ② 状态 chip
                                span {
                                    class: "header-chip header-chip--status {status_chip_class}",
                                    "{plan_status_label}"
                                }
                                // ③ 更多操作下拉菜单
                                DropdownMenu {
                                    class: "header-more-menu",
                                    DropdownMenuTrigger {
                                        class: "header-more-btn",
                                        title: "更多操作",
                                        svg {
                                            xmlns: "http://www.w3.org/2000/svg",
                                            width: "16",
                                            height: "16",
                                            view_box: "0 0 24 24",
                                            fill: "currentColor",
                                            circle { cx: "12", cy: "12", r: "1.5" }
                                            circle { cx: "12", cy: "5", r: "1.5" }
                                            circle { cx: "12", cy: "19", r: "1.5" }
                                        }
                                    }
                                    DropdownMenuContent {
                                        class: "header-more-menu-content",
                                        DropdownMenuItem::<String> {
                                            value: "delete".to_string(),
                                            index: 0usize,
                                            on_select: move |_| show_delete_dialog.set(true),
                                            class: "header-dropdown-item--danger",
                                            svg {
                                                xmlns: "http://www.w3.org/2000/svg",
                                                width: "14",
                                                height: "14",
                                                view_box: "0 0 24 24",
                                                fill: "none",
                                                stroke: "currentColor",
                                                stroke_width: "2",
                                                stroke_linecap: "round",
                                                stroke_linejoin: "round",
                                                path { d: "M3 6h18" }
                                                path { d: "M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6" }
                                                path { d: "M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2" }
                                            }
                                            "删除"
                                        }
                                    }
                                }
                            }),
                        }
                        div { class: "plan-left-panel__divider" }
                        // ── Bento 瓷块容器 ──
                        div { class: "plan-bento-container",
                            // ══════════════════════════════════════════
                            // ① PIPELINE — 执行时间线
                            // ══════════════════════════════════════════
                            div { class: "plan-bento-block",
                                div { class: "plan-bento-block__header",
                                    span { class: "plan-bento-block__header-emoji", "🎯" }
                                    span { class: "plan-bento-block__header-label", "PIPELINE" }
                                    span { class: "plan-bento-chip plan-bento-chip--version", "v3" }
                                    span { class: "plan-bento-block__header-spacer" }
                                    // 历史按钮
                                    button {
                                        class: "plan-bento-header-btn",
                                        title: "历史版本",
                                        svg {
                                            xmlns: "http://www.w3.org/2000/svg",
                                            width: "14",
                                            height: "14",
                                            view_box: "0 0 24 24",
                                            fill: "none",
                                            stroke: "currentColor",
                                            stroke_width: "2",
                                            stroke_linecap: "round",
                                            stroke_linejoin: "round",
                                            circle { cx: "12", cy: "12", r: "10" }
                                            polyline { points: "12 6 12 12 16 14" }
                                        }
                                    }
                                    // 执行按钮
                                    button {
                                        class: "plan-bento-header-btn",
                                        title: "执行计划",
                                        svg {
                                            xmlns: "http://www.w3.org/2000/svg",
                                            width: "14",
                                            height: "14",
                                            view_box: "0 0 16 16",
                                            fill: "currentColor",
                                            path { d: "M 4 2.5 L 13 8 L 4 13.5 Z" }
                                        }
                                    }
                                    // 停止按钮
                                    button {
                                        class: "plan-bento-header-btn",
                                        title: "停止执行",
                                        svg {
                                            xmlns: "http://www.w3.org/2000/svg",
                                            width: "14",
                                            height: "14",
                                            view_box: "0 0 16 16",
                                            fill: "currentColor",
                                            rect { x: "3", y: "2.5", width: "3.5", height: "11" }
                                            rect { x: "9.5", y: "2.5", width: "3.5", height: "11" }
                                        }
                                    }
                                }

                                div { class: "plan-bento-block__body",
                                    div { class: "plan-pipeline__timeline",
                                        // Step 1 — done
                                        div { class: "plan-pipeline__step plan-pipeline__step--done",
                                            div { class: "plan-pipeline__step-rail",
                                                div { class: "plan-pipeline__step-dot",
                                                    svg {
                                                        class: "plan-pipeline-node--done",
                                                        xmlns: "http://www.w3.org/2000/svg",
                                                        view_box: "0 0 16 16",
                                                        width: "14",
                                                        height: "14",
                                                        fill: "none",
                                                        stroke: "currentColor",
                                                        stroke_width: "1.75",
                                                        stroke_linecap: "round",
                                                        stroke_linejoin: "round",
                                                        circle { cx: "8", cy: "8", r: "6" }
                                                        path { d: "M 5 8.5 L 7 10.5 L 11 6" }
                                                    }
                                                }
                                                div { class: "plan-pipeline__step-line plan-pipeline__step-line--done" }
                                            }
                                            div { class: "plan-pipeline__step-body",
                                                div { class: "plan-pipeline__step-header",
                                                    span { class: "plan-pipeline__step-index", "S1" }
                                                    span { class: "plan-pipeline__step-title", "获取搜索结果" }
                                                    span { class: "plan-pipeline__step-meta", "2.3s · 3 tools" }
                                                }
                                                div { class: "plan-pipeline__step-detail", "预期输出: 结构化的搜索结果 JSON 数组" }
                                            }
                                        }

                                        // Step 2 — running + think box
                                        div { class: "plan-pipeline__step plan-pipeline__step--running",
                                            div { class: "plan-pipeline__step-rail",
                                                div { class: "plan-pipeline__step-dot",
                                                    svg {
                                                        class: "plan-pipeline-node--running",
                                                        xmlns: "http://www.w3.org/2000/svg",
                                                        view_box: "0 0 16 16",
                                                        width: "14",
                                                        height: "14",
                                                        fill: "none",
                                                        stroke: "currentColor",
                                                        stroke_width: "1.75",
                                                        stroke_linecap: "round",
                                                        path { d: "M 8 2 A 6 6 0 1 1 2 8" }
                                                    }
                                                }
                                                div { class: "plan-pipeline__step-line plan-pipeline__step-line--active" }
                                            }
                                            div { class: "plan-pipeline__step-body",
                                                div { class: "plan-pipeline__step-header",
                                                    span { class: "plan-pipeline__step-index", "S2" }
                                                    span { class: "plan-pipeline__step-title", "分析数据依赖" }
                                                    span { class: "plan-pipeline__step-meta", "⬤ RUNNING · 4.1s" }
                                                }
                                                // THINK 终端
                                                div { class: "plan-think-box",
                                                    span { class: "plan-think-box__line",
                                                        span { class: "plan-think-box__prompt", "$ " }
                                                        "analyze_deps --scope step_1_output"
                                                    }
                                                    span { class: "plan-think-box__line",
                                                        span { class: "plan-think-box__result", "> " }
                                                        "解析 Step 1 返回的 3 个端点..."
                                                    }
                                                    span { class: "plan-think-box__line",
                                                        span { class: "plan-think-box__result", "> " }
                                                        "检测到 /internal/docs 缺少 auth header"
                                                    }
                                                    span { class: "plan-think-box__line",
                                                        span { class: "plan-think-box__result", "> " }
                                                        "自动调用 search_docs(\"auth\") 补充..."
                                                    }
                                                    span { class: "plan-think-box__line",
                                                        span { class: "plan-think-box__result", "> " }
                                                        "收到 127 行文档，提取 Bearer token 格式"
                                                    }
                                                    span { class: "plan-think-box__line",
                                                        span { class: "plan-think-box__result", "> " }
                                                        "决策: 并入 Step 3 输出前验证"
                                                    }
                                                    span { class: "plan-think-box__line plan-think-box__cursor" }
                                                }
                                            }
                                        }

                                        // Step 3 — pending
                                        div { class: "plan-pipeline__step plan-pipeline__step--pending",
                                            div { class: "plan-pipeline__step-rail",
                                                div { class: "plan-pipeline__step-dot",
                                                    svg {
                                                        class: "plan-pipeline-node--pending",
                                                        xmlns: "http://www.w3.org/2000/svg",
                                                        view_box: "0 0 16 16",
                                                        width: "14",
                                                        height: "14",
                                                        fill: "none",
                                                        stroke: "currentColor",
                                                        stroke_width: "1.5",
                                                        circle { cx: "8", cy: "8", r: "6" }
                                                    }
                                                }
                                                div { class: "plan-pipeline__step-line plan-pipeline__step-line--pending" }
                                            }
                                            div { class: "plan-pipeline__step-body",
                                                div { class: "plan-pipeline__step-header",
                                                    span { class: "plan-pipeline__step-index", "S3" }
                                                    span { class: "plan-pipeline__step-title", "生成最终报告" }
                                                }
                                                div { class: "plan-pipeline__step-detail", "预期输出: Markdown 格式的综合分析报告" }
                                            }
                                        }

                                        // Step 4 — pending
                                        div { class: "plan-pipeline__step plan-pipeline__step--pending",
                                            div { class: "plan-pipeline__step-rail",
                                                div { class: "plan-pipeline__step-dot",
                                                    svg {
                                                        class: "plan-pipeline-node--pending",
                                                        xmlns: "http://www.w3.org/2000/svg",
                                                        view_box: "0 0 16 16",
                                                        width: "14",
                                                        height: "14",
                                                        fill: "none",
                                                        stroke: "currentColor",
                                                        stroke_width: "1.5",
                                                        circle { cx: "8", cy: "8", r: "6" }
                                                    }
                                                }
                                                div { class: "plan-pipeline__step-line plan-pipeline__step-line--pending" }
                                            }
                                            div { class: "plan-pipeline__step-body",
                                                div { class: "plan-pipeline__step-header",
                                                    span { class: "plan-pipeline__step-index", "S4" }
                                                    span { class: "plan-pipeline__step-title", "验证并输出" }
                                                }
                                                div { class: "plan-pipeline__step-detail", "预期输出: 验证通过的最终结果 + 摘要" }
                                            }
                                        }
                                    }

                                    // 底部状态栏
                                    div { class: "plan-pipeline__statusbar",
                                        span { class: "plan-pipeline__statusbar-item",
                                            span { class: "plan-pipeline__statusbar-dot plan-pipeline__statusbar-dot--ok" }
                                            "2/4 steps"
                                        }
                                        span { class: "plan-pipeline__statusbar-item",
                                            "⏱ "
                                            span { class: "plan-pipeline__statusbar-val", "6.4s" }
                                        }
                                        span { class: "plan-pipeline__statusbar-item",
                                            "🔧 "
                                            span { class: "plan-pipeline__statusbar-val", "5" }
                                            " calls"
                                        }
                                        span { class: "plan-pipeline__statusbar-item",
                                            "📝 "
                                            span { class: "plan-pipeline__statusbar-val", "1.2k" }
                                            " / "
                                            span { class: "plan-pipeline__statusbar-val", "8.0k" }
                                            " tk"
                                        }
                                    }
                                }
                            }

                            // ══════════════════════════════════════════
                            // ② + ③ 双列行：PARAMS | STATS
                            // ══════════════════════════════════════════
                            div { class: "plan-bento-row",
                                // ② PARAMS
                                div { class: "plan-bento-block",
                                    div { class: "plan-bento-block__header",
                                        span { class: "plan-bento-block__header-emoji", "🔧" }
                                        span { class: "plan-bento-block__header-label", "PARAMS" }
                                    }
                                    div { class: "plan-bento-block__body",
                                        div { class: "plan-params__item",
                                            span { class: "plan-params__label", "target_url" }
                                            div { class: "plan-params__input-wrap",
                                                input {
                                                    class: "plan-params__input plan-params__input--readonly",
                                                    value: "https://example.com/api/v2",
                                                    readonly: true,
                                                }
                                            }
                                        }
                                        div { class: "plan-params__item",
                                            span { class: "plan-params__label", "max_pages" }
                                            div { class: "plan-params__input-wrap",
                                                input {
                                                    class: "plan-params__input plan-params__input--readonly",
                                                    value: "5",
                                                    readonly: true,
                                                }
                                            }
                                        }
                                        div { class: "plan-params__item",
                                            span { class: "plan-params__label", "output_format" }
                                            div { class: "plan-params__input-wrap",
                                                input {
                                                    class: "plan-params__input plan-params__input--readonly",
                                                    value: "markdown",
                                                    readonly: true,
                                                }
                                                button {
                                                    class: "plan-params__edit-btn",
                                                    title: "编辑参数",
                                                    svg {
                                                        xmlns: "http://www.w3.org/2000/svg",
                                                        width: "13",
                                                        height: "13",
                                                        view_box: "0 0 24 24",
                                                        fill: "none",
                                                        stroke: "currentColor",
                                                        stroke_width: "2",
                                                        stroke_linecap: "round",
                                                        stroke_linejoin: "round",
                                                        path { d: "M17 3a2.85 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z" }
                                                        path { d: "m15 5 4 4" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                // ③ STATS
                                div { class: "plan-bento-block",
                                    div { class: "plan-bento-block__header",
                                        span { class: "plan-bento-block__header-emoji", "⚡" }
                                        span { class: "plan-bento-block__header-label", "STATS" }
                                    }
                                    div { class: "plan-bento-block__body",
                                        div { class: "plan-stats__row",
                                            span { class: "plan-stats__label", "Exec time" }
                                            span { class: "plan-stats__value plan-stats__value--highlight", "6.4s" }
                                        }
                                        div { class: "plan-stats__row",
                                            span { class: "plan-stats__label", "Tokens" }
                                            span { class: "plan-stats__value", "1.2k / 8.0k" }
                                        }
                                        div { class: "plan-stats__row",
                                            span { class: "plan-stats__label", "Tools called" }
                                            span { class: "plan-stats__value", "5" }
                                        }
                                        div { class: "plan-stats__row",
                                            span { class: "plan-stats__label", "Steps done" }
                                            span { class: "plan-stats__value plan-stats__value--success", "2/4" }
                                        }
                                        div { class: "plan-stats__row",
                                            span { class: "plan-stats__label", "Errors" }
                                            span { class: "plan-stats__value", "0" }
                                        }
                                        div { class: "plan-stats__row",
                                            span { class: "plan-stats__label", "Mode" }
                                            span { class: "plan-stats__value", "{plan_mode_label}" }
                                        }
                                    }
                                }
                            }

                            // ══════════════════════════════════════════
                            // ④ HISTORY — 历史版本
                            // ══════════════════════════════════════════
                            div { class: "plan-bento-block",
                                div { class: "plan-bento-block__header",
                                    span { class: "plan-bento-block__header-emoji", "📜" }
                                    span { class: "plan-bento-block__header-label", "HISTORY" }
                                    span { class: "plan-bento-block__header-spacer" }
                                    button {
                                        class: "plan-bento-header-btn",
                                        title: "加载所选版本",
                                        svg {
                                            xmlns: "http://www.w3.org/2000/svg",
                                            width: "13",
                                            height: "13",
                                            view_box: "0 0 24 24",
                                            fill: "none",
                                            stroke: "currentColor",
                                            stroke_width: "2",
                                            stroke_linecap: "round",
                                            stroke_linejoin: "round",
                                            path { d: "M21 12a9 9 0 1 1-6.219-8.56" }
                                            path { d: "M21 3v5h-5" }
                                        }
                                    }
                                }
                                div { class: "plan-bento-block__body",
                                    div { class: "plan-history__list",
                                        // v4 — 当前
                                        div {
                                            class: "plan-history__item plan-history__item--current",
                                            span { class: "plan-history__version", "v4" }
                                            span { class: "plan-history__time", "08-06 10:30" }
                                            span { class: "plan-history__separator", "·" }
                                            span { class: "plan-history__status plan-history__status--ok", "✓ 完成" }
                                            span { class: "plan-history__separator", "·" }
                                            span { class: "plan-history__meta", "4/4 · 12.3s · 7 tk" }
                                        }
                                        // v3
                                        div {
                                            class: "plan-history__item",
                                            span { class: "plan-history__version", "v3" }
                                            span { class: "plan-history__time", "08-05 14:20" }
                                            span { class: "plan-history__separator", "·" }
                                            span { class: "plan-history__status plan-history__status--ok", "✓ 完成" }
                                            span { class: "plan-history__separator", "·" }
                                            span { class: "plan-history__meta", "3/4 · 8.7s · 5 tk" }
                                        }
                                        // v2
                                        div {
                                            class: "plan-history__item plan-history__item--failed",
                                            span { class: "plan-history__version", "v2" }
                                            span { class: "plan-history__time", "08-05 09:00" }
                                            span { class: "plan-history__separator", "·" }
                                            span { class: "plan-history__status plan-history__status--fail", "✗ 取消" }
                                            span { class: "plan-history__separator", "·" }
                                            span { class: "plan-history__meta", "1/4 · — · 2 tk" }
                                        }
                                        // v1
                                        div {
                                            class: "plan-history__item plan-history__item--failed",
                                            span { class: "plan-history__version", "v1" }
                                            span { class: "plan-history__time", "08-04 18:30" }
                                            span { class: "plan-history__separator", "·" }
                                            span { class: "plan-history__status plan-history__status--fail", "✗ 失败" }
                                            span { class: "plan-history__separator", "·" }
                                            span { class: "plan-history__meta", "0/4 · — · 1 tk" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
                right: chat_panel,
            }
        }

        // ── 删除确认弹窗 ──
        AlertDialog {
            open: show_delete_dialog(),
            on_open_change: move |v: bool| show_delete_dialog.set(v),
            AlertDialogTitle { "删除计划？" }
            AlertDialogDescription {
                "确定要删除这个计划吗？所有相关的对话消息也会被删除，操作无法撤销。"
            }
            AlertDialogActions {
                AlertDialogCancel { "取消" }
                AlertDialogAction {
                    on_click: move |_| {
                        show_delete_dialog.set(false);
                        let pid = plan_id.clone();
                        let plan_repo = plan_repo.clone();
                        let msg_repo = message_repo.clone();
                        let on_back = on_back;
                        spawn(async move {
                            if let Some(repo) = msg_repo {
                                if let Err(e) = repo.delete_by_plan_id(&pid).await {
                                    tracing::error!("删除消息失败: {}", e);
                                }
                            }
                            if let Some(repo) = plan_repo {
                                if let Err(e) = repo.delete(&pid).await {
                                    tracing::error!("删除计划失败: {}", e);
                                }
                            }
                            on_back.call(());
                        });
                    },
                    "删除"
                }
            }
        }

        // ── 清除消息确认弹窗 ──
        AlertDialog {
            open: show_clear_dialog(),
            on_open_change: move |v: bool| show_clear_dialog.set(v),
            AlertDialogTitle { "清除消息记录？" }
            AlertDialogDescription {
                "确定要清空所有对话消息吗？此操作不可撤销，计划本身会被保留。"
            }
            AlertDialogActions {
                AlertDialogCancel { "取消" }
                AlertDialogAction {
                    on_click: {
                        let mut on_confirm_clear = on_confirm_clear.clone();
                        move |_| {
                            show_clear_dialog.set(false);
                            on_confirm_clear(());
                        }
                    },
                    "清除"
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// 私有辅助函数
// ─────────────────────────────────────────────────────────────────────

/// 从 DB 异步加载计划元数据与历史消息。
async fn load_plan_data(
    pid: String,
    plan_repo: Arc<PlanRepo>,
    msg_repo: Arc<MessageRepo>,
    mut chat: ChatState,
    mut plan: PlanState,
) {
    // 加载计划元数据
    if let Ok(Some(plan_model)) = plan_repo.find_by_id(&pid).await {
        tracing::info!(
            "load_plan_data: 加载计划 '{}', mode='{}', status='{}'",
            plan_model.name,
            plan_model.mode,
            plan_model.status,
        );
        plan.plan_info.set(Some(PlanInfo {
            name: plan_model.name,
            mode: plan_model.mode.clone(),
            status: plan_model.status,
            created_at: plan_model.created_at,
        }));
        plan.set_mode(plan_model.mode);
    }
    // 加载历史消息
    if let Ok(msg_list) = msg_repo.find_by_plan_id(&pid).await {
        let loaded: Vec<Message> = msg_list
            .into_iter()
            .map(|m| Message {
                role: match m.role.as_str() {
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    "system" => MessageRole::System,
                    "tool" => MessageRole::Tool,
                    _ => MessageRole::User,
                },
                content: if m.content.is_empty() {
                    None
                } else {
                    Some(MessageContent::Text { text: m.content })
                },
                ..Default::default()
            })
            .collect();
        chat.messages.set(loaded);
        // 对齐 reasoning_texts 长度
        chat.reasoning_texts
            .set(vec![None; chat.messages.read().len()]);
    }
}

/// 将状态块所在消息的完整内容同步回 DB 的最后一条 assistant 记录。
///
/// 流结束时的首次持久化可能尚未落库，此处做有限次重试。
async fn persist_status_block(
    message_repo: &Arc<MessageRepo>,
    pid: &str,
    chat: &ChatState,
    msg_idx: usize,
) {
    let Some(final_text) = chat.text_at(msg_idx) else {
        return;
    };
    for attempt in 0..5 {
        match message_repo.update_last_assistant(pid, &final_text).await {
            Ok(true) => return,
            Ok(false) if attempt < 4 => {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            Ok(false) => {
                tracing::error!("未找到可更新的 assistant 消息: plan_id={}", pid);
                return;
            }
            Err(e) => {
                tracing::error!("持久化状态块失败: {}", e);
                return;
            }
        }
    }
}

/// 灵活模式：从轨迹提炼粗粒度计划并保存到 `plans_flexible`。
///
/// 执行期间通过 `streaming_idx` 保持"发送中"状态阻止用户发送新消息；
/// 在最后一条 Assistant 消息后追加状态块（分割线 + 标题），
/// 以闪烁光标表示进行中，完成后追加结果文本并同步持久化。
async fn save_flexible_plan(
    chat_svc: Arc<ChatService<FilePromptManager>>,
    pid: String,
    summary: String,
    params: Vec<ParamDef>,
    plan_repo: Arc<PlanRepo>,
    flex_repo: Arc<PlanFlexibleRepo>,
    message_repo: Arc<MessageRepo>,
    mut chat: ChatState,
    mut plan: PlanState,
) {
    // ── 追加状态块头部（分割线 + 标题），开启闪烁光标表示创建中 ──
    let msg_idx = chat.start_status_block("创建计划");

    // 1. 提炼 CoarseGrainedPlan
    let todos_json = match chat_svc.generate_coarse_plan_from_trace(&summary).await {
        Ok(json) => json,
        Err(e) => {
            tracing::error!("从轨迹提炼粗粒度计划失败: {}", e);
            let msg = format!("❌ 灵活计划生成失败：从轨迹提炼粗粒度计划失败 — {e}");
            chat.finish_status_block(msg_idx, &msg);
            persist_status_block(&message_repo, &pid, &chat, msg_idx).await;
            return;
        }
    };

    // 2. 获取下一个版本号
    let version = match flex_repo.next_version(&pid).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("获取版本号失败: {}", e);
            let msg = format!("❌ 灵活计划生成失败：获取版本号失败 — {e}");
            chat.finish_status_block(msg_idx, &msg);
            persist_status_block(&message_repo, &pid, &chat, msg_idx).await;
            return;
        }
    };

    // 3. 保存到 plans_flexible（best-effort）
    let params_json = serde_json::to_string(&params).unwrap_or_else(|_| "[]".to_string());
    if let Err(e) = flex_repo
        .create(&pid, version, &summary, &todos_json, &params_json)
        .await
    {
        tracing::error!("保存灵活计划快照失败: {}", e);
    }
    // 4. 更新 plans.flexible_version（best-effort）
    if let Err(e) = plan_repo.update_flexible_version(&pid, version).await {
        tracing::error!("更新灵活计划版本失败: {}", e);
    }
    // 5. 更新计划状态（best-effort）
    if let Err(e) = plan_repo.update_status(&pid, "generated").await {
        tracing::error!("更新计划状态失败: {}", e);
    }

    // 6. 追加成功结果文本并持久化
    chat.finish_status_block(msg_idx, &format!("✅ 灵活计划 v{version} 已生成并保存"));
    persist_status_block(&message_repo, &pid, &chat, msg_idx).await;

    tracing::info!("灵活计划 v{} 已保存: plan_id={}", version, pid);
    plan.bump_version();
}

/// 渲染右侧聊天面板：PlanTodo + MessageList + 待处理 UI + 输入发送区。
fn render_chat_panel(
    plan_id: String,
    storage: Resource<Option<Arc<StorageContext>>>,
    mut chat: ChatState,
    plan: PlanState,
    chat_signal: ChatServiceSignal,
    can_create: bool,
    message_repo: Option<Arc<MessageRepo>>,
    mut show_clear_dialog: Signal<bool, SyncStorage>,
    mut auto_requirement: Signal<bool, SyncStorage>,
) -> Element {
    let sidx = chat.sidx();
    rsx! {
        div { class: "chat-panel",
            PlanTodoView {
                plan_id: plan_id.clone(),
                storage: storage.clone(),
                plan_generated: plan.plan_generated,
                plan_version: plan.plan_version,
            }

            MessageListView {
                messages: chat.messages,
                reasoning_texts: chat.reasoning_texts,
                streaming_idx: chat.streaming_idx,
            }

            // 待处理的 UI 交互
            {
                let ui = chat.pending_ui.read();
                if let Some(ref pending) = *ui {
                    let p = pending.clone();
                    let pid = plan_id.clone();
                    let msg_repo = message_repo.clone();
                    rsx! {
                        ChatUIActionsView {
                            message: p.message.clone(),
                            actions: p.actions.clone(),
                            on_action: move |(action, choice)| {
                                handle_user_action(
                                    action,
                                    choice,
                                    p.clone(),
                                    chat,
                                    chat_signal,
                                    plan,
                                    pid.clone(),
                                    msg_repo.clone(),
                                );
                            },
                        }
                    }
                } else {
                    rsx! {}
                }
            }

            // 输入发送区
            div { class: "chat-input-area",
                Textarea {
                    placeholder: if can_create { "输入消息..." } else { "等待 AI 与 Prompt 初始化..." },
                    value: "{chat.input_text}",
                    disabled: !can_create,
                    oninput: move |e: FormEvent| chat.input_text.set(e.value()),
                    onkeydown: {
                        let pid = plan_id.clone();
                        let repo = message_repo.clone();
                        move |e: KeyboardEvent| {
                            if e.data.key() == keyboard_types::Key::Enter && !e.data.modifiers().shift() {
                                e.prevent_default();
                                if can_create {
                                    if let Some(ref repo) = repo {
                                        send_message(
                                            chat_signal,
                                            chat,
                                            pid.clone(),
                                            repo.clone(),
                                            plan.mode(),
                                            auto_requirement(),
                                        );
                                    }
                                }
                            }
                        }
                    },
                }
                // ── 操作行：清除消息 / 发送 / 停止 ──
                div { class: "chat-input-area__actions",
                      Button {
                                class: "chat-input-area__icon-btn chat-input-area__icon-btn--clear",
                                variant: ButtonVariant::Ghost,
                                size: ButtonSize::Xs,
                                disabled: sidx.is_some(),
                                title: Some("清除消息记录"),
                                onclick: move |_: MouseEvent| show_clear_dialog.set(true),
                                svg {
                                    xmlns: "http://www.w3.org/2000/svg",
                                    width: "16",
                                    height: "16",
                                    view_box: "0 0 24 24",
                                    fill: "none",
                                    stroke: "currentColor",
                                    stroke_width: "2",
                                    stroke_linecap: "round",
                                    stroke_linejoin: "round",
                                    path { d: "M3 6h18" }
                                    path { d: "M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6" }
                                    path { d: "M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2" }
                                    line { x1: "10", y1: "11", x2: "10", y2: "17" }
                                    line { x1: "14", y1: "11", x2: "14", y2: "17" }
                                }
                            }
                    
                      
                    // 自动需求开关（仅灵活模式显示）
                    if plan.mode() == "flexible" {
                        div { class: "chat-input-area__auto-toggle",
                            span { class: "chat-input-area__auto-toggle-label", "自动需求" }
                            crate::components::switch::Switch {
                                checked: auto_requirement(),
                                on_checked_change: move |v: bool| auto_requirement.set(v),
                            }
                        }
                    }

                    div { class: "chat-input-area__placeholder" }
                    if sidx.is_none() {
                        Button {
                            class: "chat-input-area__icon-btn chat-input-area__icon-btn--send",
                            variant: ButtonVariant::Primary,
                            size: ButtonSize::Xs,
                            disabled: !can_create,
                            title: if !can_create { Some("AI 与 Prompt 初始化完成后才能发送") } else { Some("发送") },
                            onclick: {
                                let pid = plan_id.clone();
                                let repo = message_repo.clone();
                                move |_: MouseEvent| {
                                    if can_create {
                                        if let Some(ref repo) = repo {
                                            send_message(
                                                chat_signal,
                                                chat,
                                                pid.clone(),
                                                repo.clone(),
                                                plan.mode(),
                                                auto_requirement(),
                                            );
                                        }
                                    }
                                }
                            },
                            svg {
                                xmlns: "http://www.w3.org/2000/svg",
                                width: "16",
                                height: "16",
                                view_box: "0 0 24 24",
                                fill: "none",
                                stroke: "currentColor",
                                stroke_width: "2",
                                stroke_linecap: "round",
                                stroke_linejoin: "round",
                                path { d: "M22 2 11 13" }
                                path { d: "m22 2-7 20-4-9-9-4 20-7z" }
                            }
                        }
                    } else {
                        Button {
                            class: "chat-input-area__icon-btn chat-input-area__icon-btn--stop",
                            variant: ButtonVariant::Destructive,
                            size: ButtonSize::Xs,
                            title: Some("停止生成"),
                            onclick: move |_: MouseEvent| {
                                if let Some(ref svc) = *chat_signal.read() {
                                    svc.stop();
                                }
                            },
                            svg {
                                xmlns: "http://www.w3.org/2000/svg",
                                width: "16",
                                height: "16",
                                view_box: "0 0 24 24",
                                fill: "currentColor",
                                rect { x: "6", y: "6", width: "12", height: "12", rx: "1.5" }
                            }
                        }
                    }
                }
            }
        }
    }
}
