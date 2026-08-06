//! PlanTodoView 组件：Plan 页面聊天面板顶部的「执行计划」区域。
//!
//! - 从 `plans_flexible` 表加载最新版本的 todos JSON，解析为 `TodoItemData` 列表
//! - 通过 `plan_generated` signal 监听计划生成事件（灵活模式时清空列表 + 展开面板 + 显示"生成中"）
//! - 通过 `plan_version` signal 监听保存完成事件，触发 DB 重新加载

use std::sync::Arc;

use dioxus::prelude::*;

use crate::components::todo::{Todo, TodoItemData, TodoStatus};
use crate::context::StorageContext;
use crate::pages::plan::types::{PlanGeneratedEvent, PlanSource};

#[css_module("/src/pages/plan/components/plan_todo_view/style.css")]
struct Styles;

/// Plan 页面聊天面板顶部的 Todo 计划区。
///
/// # Props
/// - `plan_id` — 当前计划的 ID，用于从 DB 加载数据
/// - `storage` — 持久化上下文
/// - `plan_generated` — 计划生成事件信号（用户点击"确认生成"时触发）
/// - `plan_version` — 计划版本号，`save_flexible_plan` 成功后递增，触发重新加载
#[component]
pub fn PlanTodoView(
    plan_id: String,
    storage: Resource<Option<Arc<StorageContext>>>,
    plan_generated: Signal<Option<PlanGeneratedEvent>, SyncStorage>,
    plan_version: Signal<u32, SyncStorage>,
) -> Element {
    let mut items = use_signal_sync(Vec::<TodoItemData>::new);
    let mut generating = use_signal_sync(|| false);
    // 强制展开触发器：生成事件 / 加载完成时递增，驱动 Todo 重新展开
    let mut expand_token = use_signal_sync(|| 0u32);

    // ── 加载计划：首次挂载 + plan_version 变更时从 DB 加载 ──
    {
        let plan_id = plan_id.clone();
        let storage = storage.clone();
        let mut expand_token = expand_token;
        let mut items = items;
        use_effect(move || {
            let _v = *plan_version.read();
            let storage_opt = storage.read().as_ref().and_then(|x| x.as_ref()).cloned();
            if let Some(ctx) = storage_opt {
                let pid = plan_id.clone();
                let flex_repo = ctx.plan_flexible_repo.clone();
                spawn(async move {
                    match flex_repo.find_latest(&pid).await {
                        Ok(Some(snapshot)) => {
                            if !snapshot.todos.is_empty() && snapshot.todos != "[]" {
                                items.set(parse_todos_json(&snapshot.todos));
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::error!("加载灵活计划快照失败: {}", e);
                        }
                    }
                    generating.set(false);
                    // 有可展示的计划项时，触发 Todo 强制展开（历史加载 / 保存完成）
                    if !items.read().is_empty() {
                        expand_token.with_mut(|v| *v += 1);
                    }
                });
            }
        });
    }

    // ── 灵活模式生成事件：清空旧列表 + 展开 + 显示"生成中" ──
    {
        let mut expand_token = expand_token;
        use_effect(move || {
            if let Some(ref event) = *plan_generated.read() {
                if event.source == PlanSource::Flexible {
                    items.set(vec![TodoItemData::new(
                        TodoStatus::Pending,
                        "正在生成执行计划...",
                        "AI 正在根据执行轨迹提炼可复用的计划步骤，请稍候",
                    )]);
                    generating.set(true);
                    expand_token.with_mut(|v| *v += 1);
                }
            }
        });
    }

    rsx! {
        div {
            class: Styles::plan_todo_view,
            Todo {
                items: items.read().clone(),
                expand_token: expand_token,
                on_execute: move |_| {
                    // TODO(后续): 触发 Agent 执行计划
                },
            }
        }
    }
}

/// 从 CoarseGrainedPlan JSON 的 steps 数组解析为 TodoItemData 列表。
///
/// `todos_json` 是 `CoarseGrainedPlan` 的完整 JSON，包含 `steps` 数组。
/// 每个 step 的 `intent` → title，`expected_output` → detail。
fn parse_todos_json(json_str: &str) -> Vec<TodoItemData> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) else {
        return vec![];
    };
    let Some(steps) = value.get("steps").and_then(|s| s.as_array()) else {
        return vec![];
    };
    steps
        .iter()
        .map(|step| {
            let intent = step
                .get("intent")
                .and_then(|v| v.as_str())
                .unwrap_or("未命名步骤");
            let expected = step
                .get("expected_output")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            TodoItemData::new(TodoStatus::Pending, intent, expected)
        })
        .collect()
}