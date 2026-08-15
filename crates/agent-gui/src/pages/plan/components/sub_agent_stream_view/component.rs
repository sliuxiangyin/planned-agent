//! 渲染子 agent 过程流视图。
//!
//! 旧的 `Message.sub_agent_streams` 字段已移除，子 agent 过程流
//! 现在通过 `SubAgentChatEvent` 旁路回调直接驱动 UI，本组件暂作空壳保留，
//! 待 `SubAgentStream` UI 状态接入后填充。

use dioxus::prelude::*;

#[component]
pub fn SubAgentStreamView() -> Element {
    rsx! {}
}
