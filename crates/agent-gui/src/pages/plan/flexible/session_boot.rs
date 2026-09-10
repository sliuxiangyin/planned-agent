//! 灵活模式「会话启动门」—— 仿 `crate::boot` 的一次会话就绪流程。
//!
//! 目的：把 `use_flexible_controller` 里原本靠多个 `use_effect` + signal 值互相串联
//! 才拼起来的初始化（new_chat_service → start_driver →
//! view_from_history → ChatBridge::connect），收敛成一段**顺序的 async 直线代码**，
//! 就地报错并聚合成 `Vec<(模块, 错误)>`，与全局启动门同一心智模型。
//!
//! 关键点：
//! - 本模块**不**创建任何 dioxus signal —— `Signal<ChatView>` 必须在 hook 顶层由
//!   `use_signal_sync` 创建（遵守 rules of hooks）。本模块拿到其 Copy 句柄后，
//!   在内部用 `view_from_history` 填充历史，再 `ChatBridge::connect` 建立订阅桥；
//!   订阅 guard 因存进桥结构体（随 `ReadySession.bridge` 存活）而跨 render 存活。
//! - 返回 `ReadySession`：session_id + 绑好该会话 store 且已 start_driver 的
//!   ChatService（`Arc`）。一次启动的完整产物，未来多会话=对每个 session 各调一次。
//! - `FlexBoot` 状态机与全局 `BootPhase` 对齐：`Loading(进度) → Ready / Failed`。

use std::sync::Arc;

use dioxus::prelude::*;

use crate::components::chat::chat_flow::{view_from_history, ChatBridge, ChatView};
use crate::context::{AiContext, PromptContext, StorageContext, ToolsContext};
use crate::shared::{BootPhase, OnProgress};

use super::chat_service_factory::new_chat_service;

/// 灵活模式会话启动状态机：复用全局 `BootPhase`，`Ready` 载荷为 `ReadySession`。
///
/// 保留 `FlexBoot` 名称以最小化调用点改动（`FlexBoot::Loading` 等仍是合法变体路径）。
pub(crate) type FlexBoot = BootPhase<ReadySession>;

/// 一次会话启动的就绪产物。
#[derive(Clone)]
pub(crate) struct ReadySession {
    /// 单一订阅桥：事件 → `reduce` → `view`，guard 随桥存活。
    /// `ChatBridge` 内部持有 `Arc<ChatService>`，会话管理操作（stop/reset/template）经桥转发。
    pub bridge: Arc<ChatBridge>,
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
    mut view: Signal<ChatView, SyncStorage>,
    session_id: String,
    on_phase: OnProgress,
) -> Result<ReadySession, Vec<(String, String)>> {
    // 1. 构造绑该会话 store 的 ChatService（未 start_driver），并 Arc 化（订阅/广播需 Arc）
    on_phase("service");
    let service = Arc::new(
        new_chat_service(storage, plan_id, session_id, ai, tools, prompt)
            .await
            .map_err(|e| vec![("构造 ChatService".to_string(), e.to_string())])?,
    );

    // 2. 启动后台 driver
    on_phase("driver");
    service
        .start_driver()
        .map_err(|e| vec![("启动 ChatService driver".to_string(), e.to_string())])?;

    // 3. 从服务端 store 恢复历史气泡
    on_phase("history");
    let history = service.history_store();
    tracing::info!("灵活模式: 从服务端加载 {} 条历史消息", history.len());
    // 无条件按目标会话重载视图：空历史也要清空，避免切到新会话后残留上一个会话的气泡。
    *view.write() = view_from_history(&history);

    // 4. 建立单一订阅桥（事件 → reduce → view），guard 随 bridge 存活
    on_phase("subscribe");
    let bridge = Arc::new(ChatBridge::connect(service, view));

    Ok(ReadySession { bridge })
}
