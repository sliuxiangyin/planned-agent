//! PlanTodoView 组件：Plan 页面聊天面板顶部的「执行计划」区域。
//!
//! - 当前为 UI mock：内置 4 条固定 Todo 项用于演示
//! - 通过 `plan_generated` signal 监听来自 `chat::handle_user_action` 的计划生成事件
//! - 后续将：从 `PlanGeneratedEvent.plan_text` 解析计划条目，同步填充 Todo
//!   并在执行计划时按状态机更新 `TodoStatus`

use dioxus::prelude::*;

use crate::components::todo::{Todo, TodoItemData, TodoStatus};
use crate::pages::plan::types::PlanGeneratedEvent;

#[css_module("/src/pages/plan/components/plan_todo_view/style.css")]
struct Styles;

/// Plan 页面聊天面板顶部的 Todo 计划区。
///
/// # Props
/// - `plan_generated` — 计划生成事件信号（用户点击"确认生成"时触发）
#[component]
pub fn PlanTodoView(
    plan_generated: Signal<Option<PlanGeneratedEvent>, SyncStorage>,
) -> Element {
    // 监听计划生成事件（后续在此解析 plan_text → TodoItemData）
    use_effect(move || {
        if let Some(ref event) = *plan_generated.read() {
            tracing::info!(
                "PlanTodoView: 收到计划生成事件, source={:?}, text={:?}",
                event.source,
                event.plan_text
            );
        }
    });

    rsx! {
        div {
            class: Styles::plan_todo_view,
            Todo {
                items: vec![
                    TodoItemData::new(
                        TodoStatus::Completed,
                        "分析当前项目结构与依赖关系",
                        "读取 Cargo.toml 与 crate 目录结构，识别已实现的核心模块（core、planned-agent、agent-gui 等），确认本次改动边界。",
                    ),
                    TodoItemData::new(
                        TodoStatus::Running,
                        "创建 Todo UI 组件",
                        "在 crates/agent-gui/src/components/todo/ 下新增 mod.rs、component.rs、style.css，复用 dioxus_primitives::accordion 行为，自定义 trigger / content 样式。",
                    ),
                    TodoItemData::new(
                        TodoStatus::Queued,
                        "在 Plan 页面接入 Todo 组件",
                        "在 pages/plan.rs 的右侧聊天面板顶部插入 Todo，固定 200px 高度，列表区支持展开收起，底部执行按钮暂不接业务。",
                    ),
                    TodoItemData::new(
                        TodoStatus::Pending,
                        "后续接入 AI 计划数据流",
                        "从 Assistant 回复中解析计划条目，同步填充 Todo；执行计划时按状态机更新 TodoStatus；本轮仅完成 UI，不实现数据流。",
                    ),
                ],
                on_execute: move |_| {
                    // TODO(后续): 触发 Agent 执行计划
                },
            }
        }
    }
}