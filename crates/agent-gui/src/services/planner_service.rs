//! PlannerService 派生 hook

use std::sync::Arc;
use dioxus::prelude::*;
use planned_agent::LlmCoarsePlanner;
use planned_agent_prompt_manager::FilePromptManager;

use crate::context::{AiContext, PromptContext};

/// PlannerService 缓存 Signal 的类型别名
pub(crate) type PlannerServiceSignal =
    Signal<Option<Arc<LlmCoarsePlanner<FilePromptManager>>>, SyncStorage>;

/// 派生 Signal：根据 `ai_resource` + `prompt_resource` 当前值，缓存一个
/// `Arc<LlmCoarsePlanner<FilePromptManager>>`。两者都 Ready 时构建，否则为 `None`。
///
/// 用 `use_signal_sync + use_effect` 组合，绕开 `use_memo` 对 `PartialEq` 的要求
/// （`LlmCoarsePlanner` 没有实现 `PartialEq`）。
///
/// 必须在组件渲染期间调用（依赖 Dioxus 当前 scope）。
///
/// 当前没有任何 page 调用，预留供后续页面（如 Trace）使用，因此标注 `#[allow(dead_code)]`。
#[allow(dead_code)]
pub(crate) fn use_planner_service() -> PlannerServiceSignal {
    let ai_resource = use_context::<Resource<Option<Arc<AiContext>>>>();
    let prompt_resource = use_context::<Resource<Option<Arc<PromptContext>>>>();

    let planner_signal: PlannerServiceSignal = use_signal_sync(|| None);
    {
        let mut planner_signal = planner_signal;
        use_effect(move || {
            let ai = ai_resource.read().as_ref().and_then(|x| x.as_ref()).cloned();
            let prompt = prompt_resource.read().as_ref().and_then(|x| x.as_ref()).cloned();
            let next = match (ai, prompt) {
                (Some(ai), Some(prompt)) => match ai.manager.default() {
                    Ok(ai_client) => Some(Arc::new(LlmCoarsePlanner::new(
                        ai_client,
                        prompt.manager.clone(),
                    ))),
                    Err(e) => {
                        tracing::warn!("Planner: AI 默认客户端不可用: {}", e);
                        None
                    }
                },
                _ => None,
            };
            planner_signal.set(next);
        });
    }
    planner_signal
}