//! 灵活模式页面控制器 —— 收口页面所有 signal / 异步初始化 / 事件逻辑。
//!
//! 把原本散落在 `page.rs` 组件体里的：
//! - 聊天/选项栏 signal
//! - ChatService 异步初始化（按传入的 session_id → 绑会话 store）
//! - 历史加载 + 事件订阅
//! - 可用模板列表加载
//! - 子 agent 注册 / 注销
//! - 事件处理方法（清空、模板切换、用户操作）
//!
//! 全部收进 `use_flexible_controller` 这一个自定义 hook 返回的 `FlexibleController`，
//! 让 `FlexiblePage` 组件退化为「状态 → 视图」的纯渲染层。
//!
//! 设计要点：
//! - `Signal<ChatView>` 是 `Copy`，controller 直接持有 `view` / `input_text` 句柄副本；
//!   需要 `&mut ChatView` 的方法用 `view.write()` 拿到写 guard 就地更新（状态经信号共享）。
//! - 所有 `use_*` 在该 hook 内无条件、顺序稳定调用（遵守 rules of hooks）。

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::chat::SystemPrompt;

use crate::components::chat::chat_flow::{ChatBridge, ChatView, PendingUI};
use crate::context::{require_resource, AiContext, PromptContext, StorageContext, ToolsContext};
use crate::shared::BootReporter;

use super::session_boot::{boot_flexible_session, FlexBoot};

/// 灵活模式控制器：持有全部状态 signal 与 ChatService，并提供事件处理方法。
#[derive(Clone, Copy)]
pub(crate) struct FlexibleController {
    /// 单一订阅桥的 UI 投影（展示缓冲；持久化由服务端 store 负责）。
    pub view: Signal<ChatView, SyncStorage>,
    /// 输入框文本（组件局部 UI 态，独立于 ChatView）。
    pub input_text: Signal<String, SyncStorage>,
    /// 是否启用思考模式
    thinking: Signal<bool, SyncStorage>,
    /// 温度值
    temperature: Signal<String, SyncStorage>,
    /// 当前系统提示模板
    template: Signal<Option<String>, SyncStorage>,
    /// 可用模板列表
    templates: Signal<Vec<String>, SyncStorage>,
    /// 会话启动状态机：Loading(进度) → Ready(ReadySession) / Failed。
    ///
    /// 取代旧的多份分散 signal（svc / svc_started / initialized / session_mgr）。
    boot: Signal<FlexBoot, SyncStorage>,
}

impl FlexibleController {
    /// 就绪后的 ChatService；未就绪/失败时为 None。
    /// 就绪后的单一订阅桥；未就绪/失败时为 None。
    pub(crate) fn bridge(&self) -> Option<Arc<ChatBridge>> {
        match self.boot.read().clone() {
            FlexBoot::Ready(session) => Some(session.bridge.clone()),
            _ => None,
        }
    }

    /// 当前会话启动状态（供页面按 Loading / Failed / Ready 分支渲染）。
    pub(crate) fn boot_phase(&self) -> FlexBoot {
        self.boot.read().clone()
    }

    // ── 只读访问器 ────────────────────────────────────────────────
    pub(crate) fn is_busy(&self) -> bool {
        self.view.read().is_busy()
    }
    pub(crate) fn thinking(&self) -> bool {
        *self.thinking.read()
    }
    pub(crate) fn temperature(&self) -> String {
        self.temperature.read().clone()
    }
    pub(crate) fn template(&self) -> String {
        self.template.read().clone().unwrap_or_default()
    }
    pub(crate) fn templates(&self) -> Vec<String> {
        self.templates.read().clone()
    }

    // ── 事件处理方法 ─────────────────────────────────────────────
    /// 清空会话（停止 + 重置服务端会话 + 清空气泡）。
    pub(crate) fn clear_session(&self) {
        if let Some(bridge) = self.bridge() {
            bridge.stop();
            if let Err(e) = bridge.reset_session() {
                tracing::error!("清空会话重置失败: {}", e);
            }
        }
        let mut view = self.view;
        view.write().clear();
    }

    /// 用户提交 request_user_action / 子 agent 卡片后的回调。
    pub(crate) fn on_user_action(&self, choice: String, pending: PendingUI) {
        let Some(bridge) = self.bridge() else { return };
        bridge.confirm(choice, pending);
    }

    /// 切换系统提示模板（切换即停当前会话并重置）。
    pub(crate) fn apply_template(&self, name: String) {
        if name.is_empty() {
            return;
        }
        if let Some(bridge) = self.bridge() {
            bridge.stop();
            bridge.set_system_prompt(Some(SystemPrompt::Template(name.clone())));
            if let Err(e) = bridge.reset_session() {
                tracing::error!("重置会话失败: {}", e);
            }
        }
        let mut view = self.view;
        let mut v = view.write();
        v.clear_pending();
        v.pending_tool_call_id = None;
        let mut template = self.template;
        template.set(Some(name));
    }

    pub(crate) fn set_thinking(&self, v: bool) {
        let mut thinking = self.thinking;
        thinking.set(v);
    }
    pub(crate) fn set_temperature(&self, v: String) {
        let mut temperature = self.temperature;
        temperature.set(v);
    }
}

/// 创建/复用 flexible 页面控制器（每个常驻宿主一个句柄）。组件须在顶层无条件调用。
///
/// `session_id` 由宿主组件以固定值传入（不在本 hook 内跟随切换）：
/// 多会话保活下每个会话一个 host，各自 boot 一次、切走不重建。
///
/// `templates` 由壳持有（plan 级共享，所有会话同一列表），经宿主透传；本 hook 不再自建。
pub(crate) fn use_flexible_controller(
    plan_id: String,
    session_id: String,
    templates: Signal<Vec<String>, SyncStorage>,
) -> FlexibleController {
    // ── 纯内存聊天状态（单一订阅桥的 UI 投影 + 独立输入框态）──
    let view = use_signal_sync(ChatView::default);
    let input_text = use_signal_sync(String::new);

    // ── option 栏状态 ──
    let thinking = use_signal_sync(|| true);
    let temperature = use_signal_sync(|| "0.7".to_string());
    let template = use_signal_sync(|| Some("flexible/flexible_step1".to_string()));

    // ── 会话启动状态机（boot.rs 风格）：Loading → Ready / Failed ──
    let boot = use_signal_sync(|| FlexBoot::Loading(Vec::new()));

    // ── 依赖 context（启动门保证全部就绪，require_resource 直接返回 Arc）──
    let storage_ctx = require_resource::<StorageContext>();
    let ai_ctx = require_resource::<AiContext>();
    let tools_ctx = require_resource::<ToolsContext>();
    let prompt_ctx = require_resource::<PromptContext>();

    // ── 供异步 boot 闭包捕获的 owned clone ──
    let plan_id_c = plan_id.clone();
    let ai_ctx_c = ai_ctx.clone();
    let tools_ctx_c = tools_ctx.clone();
    let prompt_ctx_c = prompt_ctx.clone();
    let storage_ctx_c = storage_ctx.clone();

    // ── 会话异步启动（一次）：建 session → new_chat_service → driver → 历史 → 订阅 ──
    use_effect(move || {
        let storage = storage_ctx_c.clone();
        let plan_id = plan_id_c.clone();
        let ai_ctx = ai_ctx_c.clone();
        let tools_ctx = tools_ctx_c.clone();
        let prompt_ctx = prompt_ctx_c.clone();

        // 无会话（plan 数据未就绪 / props 传空）时不启动，避免用空 id 去 boot。
        // session_id 是宿主传入的固定值（非 signal）→ 本 effect 无响应式依赖，
        // 仅在本宿主首次挂载时执行一次（切走不重建 → 保活）。
        let session_id = session_id.clone();
        if session_id.is_empty() {
            return;
        }

        // ChatView 是 Copy：把句柄副本交给 boot 异步填充历史/建立订阅桥
        let view_boot = view;
        // 进度/结果写回器：把「累积进度 + 写 Ready/Failed」的 signal 样板收口
        // （见 `BootReporter`，与全局 bootstrap 共用同一套逻辑）。
        let reporter = BootReporter::new(boot);
        spawn(async move {
            match boot_flexible_session(
                storage,
                plan_id,
                ai_ctx,
                tools_ctx,
                prompt_ctx,
                view_boot,
                session_id.clone(),
                reporter.on_progress(),
            )
            .await
            {
                Ok(session) => reporter.finish(session),
                Err(errors) => {
                    tracing::error!("灵活模式会话启动失败: {:?}", errors);
                    reporter.fail(errors);
                }
            }
        });
    });

    FlexibleController {
        view,
        input_text,
        thinking,
        temperature,
        template,
        templates,
        boot,
    }
}
