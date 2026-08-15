//! PlanTodoView 组件：Plan 页面聊天面板顶部的「执行计划」区域。
//!
//! 当前为占位组件（柔性模式已迁移到 FlexiblePage）；保留 props 接口与 UI 容器，
//! 未来若重新引入计划步骤可视化，可在此扩展。

use std::sync::Arc;

use dioxus::prelude::*;

use crate::components::todo::Todo;
use crate::context::StorageContext;

#[css_module("/src/pages/plan/components/plan_todo_view/style.css")]
struct Styles;

/// Plan 页面聊天面板顶部的 Todo 计划区（占位）。
///
/// # Props
/// - `plan_id` — 当前计划的 ID（占位保留）
/// - `storage` — 持久化上下文（占位保留）
/// - `plan_version` — 计划版本号（占位保留）
#[allow(unused_variables)]
#[component]
pub fn PlanTodoView(
    plan_id: String,
    storage: Resource<Option<Arc<StorageContext>>>,
    plan_version: Signal<u32, SyncStorage>,
) -> Element {
    // 柔性模式已迁移到 FlexiblePage 自身维护消息流；周密模式暂无计划可视化。
    let expand_token = use_signal_sync(|| 0u32);
    rsx! {
        div {
            class: Styles::plan_todo_view,
            Todo {
                items: vec![],
                expand_token,
                on_execute: move |_| {},
            }
        }
    }
}