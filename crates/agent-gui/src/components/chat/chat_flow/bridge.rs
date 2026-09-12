//! `ChatBridge` —— 唯一接触 `ChatService` 的桥接层。
//!
//! 组件层不再直接 import `ChatService`，只拿 `ChatBridge`（或 `Signal<ChatView>` +
//! 三个方法）。桥内部持有：
//! - `svc`：同进程的 `Arc<ChatService>`（将来可替换为 `HttpChatClient` 等其它实现）；
//! - `view`：`Signal<ChatView>`（唯一 UI 投影，事件经 [`reduce`] 就地更新）；
//! - `_guard`：`SubscriptionGuard`，Drop 时自动退订事件订阅。
//!
//! # 锁序约定（避免死锁）
//!
//! `send` / `confirm` 会「改 view」并「调 svc」；而 `svc` 内部 driver loop 在另一线程
//! 处理命令后 emit 事件，事件回调会 `view.write()`。因此这两处必须**先改 view、
//! 释放 write guard，再调 svc**，杜绝「持有 view 写锁时等待 svc」的嵌套锁。
//!
//! # `Signal` 是 `Copy`、`write()` 是 `&mut self`
//!
//! dioxus 0.7 中 `Signal::write()` / `set()` 的接收者是 `&mut self`（`WritableExt`），
//! 但 `Signal` 本身 `Copy`。故把 `view` 拷贝成 `mut` 局部变量再 `write()`，
//! 闭包也保持 `Fn`（只按值拷贝 `view`，不对捕获变量做 `&mut` 借用），
//! 满足 `on_chat_with_guard` 的 `Fn + Send + Sync + 'static` 约束。

use std::sync::Arc;

use dioxus::prelude::*;

use planned_agent::chat::{ChatEvent as ServiceChatEvent, SubscriptionGuard, SystemPrompt};
use planned_agent::ChatService;
use planned_agent_prompt_manager::FilePromptManager;

use super::reduce::{fmt_reply, reduce};
use super::types::{ActionReply, AgentEvent, PendingUI};
use super::view::ChatView;

/// 聊天桥：`ChatService` ↔ `Signal<ChatView>` 的唯一接线点。
pub struct ChatBridge {
    svc: Arc<ChatService<FilePromptManager>>,
    view: Signal<ChatView, SyncStorage>,
    _guard: Option<SubscriptionGuard>,
}

impl ChatBridge {
    /// 建立桥：注册事件订阅（事件 → `reduce` → `view`），guard 随桥存活。
    pub fn connect(
        svc: Arc<ChatService<FilePromptManager>>,
        view: Signal<ChatView, SyncStorage>,
    ) -> Self {
        let guard = {
            let view_copy = view;
            svc.on_chat_with_guard(move |ev: ServiceChatEvent| {
                let mut vc = view_copy;
                reduce(&mut *vc.write(), &ev);
            })
        };
        Self {
            svc,
            view,
            _guard: Some(guard),
        }
    }

    /// 暴露 UI 投影供组件读取。
    pub fn view(&self) -> Signal<ChatView, SyncStorage> {
        self.view
    }

    /// 发送用户消息：push user turn + `send_text` 入队。
    pub fn send(&self, text: String) {
        // 1. 改 view（短暂持有写锁）
        {
            let mut view = self.view;
            let mut v = view.write();
            v.clear_pending();
            v.push_user_turn(text.clone());
        }
        // 2. 调 svc（已释放 view 写锁）
        if let Err(e) = self.svc.send_text(text) {
            let mut view = self.view;
            let mut v = view.write();
            v.stop_streaming();
            v.append_to_last_assistant(&format!("*发送失败: {}*", e));
        }
    }

    /// 用户提交 `request_user_action` / 子 agent 挂起卡片。
    ///
    /// 按 `pending.run_id` 区分子 agent（`resume_sub_agent`）与主 agent（`confirm_user_action`）路径。
    /// `reply` 携带「提交 / 取消」语义：取消对应 `action_id = "cancel"` 且 choice 为空串，
    /// 与「未作答直接提交」（`action_id = "submit"` + 空串）区分开。
    pub fn confirm(&self, reply: ActionReply, pending: PendingUI) {
        let (choice, action_id) = match &reply {
            ActionReply::Submit(c) => (c.clone(), "submit"),
            ActionReply::Cancel => (String::new(), "cancel"),
        };
        let rendered = fmt_reply(&reply);
        // 1. 改 view（先落用户选择文本，再清 pending）
        {
            let mut view = self.view;
            let mut v = view.write();
            if let Some(run_id) = pending.run_id.clone() {
                if v.agent_views.contains_key(&run_id) {
                    v.push_agent_event(&run_id, AgentEvent::TextDelta(rendered.clone()));
                } else {
                    // fallback：agent_views 里找不到（历史加载后），写到父 agent 气泡
                    v.append_to_last_assistant(&rendered);
                }
            } else {
                v.append_to_last_assistant(&rendered);
                v.push_assistant_placeholder();
            }
            v.clear_pending();
            v.pending_tool_call_id = None;
        }
        // 2. 调 svc（已释放 view 写锁）
        if let Some(run_id) = pending.run_id {
            let input = serde_json::json!({ "choice": choice, "action_id": action_id });
            if let Err(e) = self.svc.resume_sub_agent(&run_id, input) {
                let mut view = self.view;
                let mut v = view.write();
                v.append_to_last_assistant(&format!("\n\n*子 agent 恢复出错: {}*", e));
                v.stop_streaming();
            }
        } else if let Err(e) =
            self.svc
                .confirm_user_action(&pending.tool_call_id, &choice, action_id)
        {
            let mut view = self.view;
            let mut v = view.write();
            v.append_to_last_assistant(&format!("\n\n*交互提交失败: {}*", e));
            v.stop_streaming();
        }
    }

    /// 停止当前对话（转发 `svc.stop()`）。
    pub fn stop(&self) {
        self.svc.stop();
    }

    /// 设置系统 prompt（转发 `svc.set_system_prompt`）。
    pub fn set_system_prompt(&self, system_prompt: Option<SystemPrompt>) {
        self.svc.set_system_prompt(system_prompt);
    }

    /// 重置服务端会话（转发 `svc.reset_session`）。
    pub fn reset_session(&self) -> anyhow::Result<()> {
        self.svc.reset_session()
    }
}
