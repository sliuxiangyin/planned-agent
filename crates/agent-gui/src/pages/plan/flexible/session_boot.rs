//! 灵活模式「会话启动门」—— 仿 `context/boot.rs` 的一次会话就绪流程。
//!
//! 目的：把 `use_flexible_controller` 里原本靠多个 `use_effect` + signal 值互相串联
//! 才拼起来的初始化（ensure_current_session → new_chat_service → start_driver →
//! load_from_history → ensure_subscription），收敛成一段**顺序的 async 直线代码**，
//! 就地报错并聚合成 `Vec<(模块, 错误)>`，与全局启动门同一心智模型。
//!
//! 关键点：
//! - 本模块**不**创建任何 dioxus signal —— `ChatSignals` 必须在 hook 顶层由
//!   `use_signal_sync` 创建（遵守 rules of hooks）。本模块拿到其 Copy 句柄后，
//!   在内部填充历史并把事件订阅 guard 写入 `chat.subscription`；guard 因存进
//!   signal 而跨 render 存活（不会在函数返回时被 drop 退订）。
//! - 返回 `ReadySession`：session_id + 绑好该会话 store 且已 start_driver 的
//!   ChatService（`Arc`）。一次启动的完整产物，未来多会话=对每个 session 各调一次。
//! - `FlexBoot` 状态机与全局 `BootPhase` 对齐：`Loading(进度) → Ready / Failed`。

use std::sync::Arc;

use crate::components::chat::chat_flow::{ensure_subscription, ChatSignals};
use crate::context::{AiContext, PromptContext, StorageContext, ToolsContext};
use crate::pages::plan::shared::session::SessionManager;

use super::chat_service_factory::{new_chat_service, ChatSvc};

/// 启动进度回调：与全局 `boot.rs` 的 `OnProgress` 同款（可经 signal 回写进度）。
pub(crate) type OnBootPhase = Arc<dyn Fn(&'static str) + Send + Sync>;

/// 灵活模式会话启动状态机（页面控制器持有）。
#[derive(Clone)]
pub(crate) enum FlexBoot {
    /// 仍在启动；携带已完成的阶段名列表，供 UI 展示进度。
    Loading(Vec<&'static str>),
    /// 就绪：持有会话启动产物。
    Ready(Arc<ReadySession>),
    /// 失败：携带「阶段名 + 错误信息」清单。
    Failed(Vec<(String, String)>),
}

/// 一次会话启动的就绪产物。
#[derive(Clone)]
pub(crate) struct ReadySession {
    /// 已就绪会话 id（service 已绑该 session 的 store）。
    pub session_id: String,
    /// 已 start_driver、可收发消息的 ChatService。
    pub svc: Arc<ChatSvc>,
}

/// 一段顺序的「会话就绪」启动。仿 `boot.rs::bootstrap`：
/// 每完成一个阶段回调 `on_phase(阶段名)`，任一必需步骤失败则短路返回错误清单。
#[allow(clippy::too_many_arguments)] // 一次性收全会话启动依赖
pub(crate) async fn boot_flexible_session(
    storage: Arc<StorageContext>,
    plan_id: String,
    ai: Arc<AiContext>,
    tools: Arc<ToolsContext>,
    prompt: Arc<PromptContext>,
    mut chat: ChatSignals,
    session_mgr: Arc<SessionManager>,
    on_phase: OnBootPhase,
) -> Result<ReadySession, Vec<(String, String)>> {
    // 1. 定位/新建该 plan 的当前会话
    on_phase("session");
    let session = storage
        .ensure_current_session(&plan_id)
        .await
        .map_err(|e| vec![("定位当前会话".to_string(), e.to_string())])?;
    let session_id = session.id.clone();
    // 尽早广播当前会话到共享管理中心（flexible_state/step5 等旁路经 watch 定位）
    session_mgr.set_active(session_id.clone());

    // 2. 构造绑该会话 store 的 ChatService（未 start_driver），并 Arc 化（订阅/广播需 Arc）
    on_phase("service");
    let service = Arc::new(
        new_chat_service(storage, plan_id, session_id.clone(), ai, tools, prompt)
            .await
            .map_err(|e| vec![("构造 ChatService".to_string(), e.to_string())])?,
    );

    // 3. 启动后台 driver
    on_phase("driver");
    service
        .start_driver()
        .map_err(|e| vec![("启动 ChatService driver".to_string(), e.to_string())])?;

    // 4. 从服务端 store 恢复历史气泡
    on_phase("history");
    let history = service.history_store();
    if !history.is_empty() {
        tracing::info!("灵活模式: 从服务端加载 {} 条历史消息", history.len());
        chat.load_from_history(&history);
    }

    // 5. 注册事件订阅（guard 写入 chat.subscription signal，跨 render 存活）
    on_phase("subscribe");
    ensure_subscription(&mut chat, &service);

    Ok(ReadySession {
        session_id,
        svc: service,
    })
}
