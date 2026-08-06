//! Plan 页面消息列表组件：接收消息信号，渲染消息列表（含推理视图、流式光标、Markdown）。
//!
//! 从 `page` 中解耦，所有项以 `pub(super)` 对同级 `page` 暴露。

use dioxus::prelude::*;
use planned_agent_core::types::Message;

use crate::components::markdown::Markdown;
use crate::components::scroll_area::ScrollArea;

use super::components::reasoning_view::ReasoningView;
use super::types::{display_text, role_css_class};

#[component]
pub(super) fn MessageListView(
    messages: Signal<Vec<Message>, SyncStorage>,
    reasoning_texts: Signal<Vec<Option<String>>, SyncStorage>,
    streaming_idx: Signal<Option<usize>, SyncStorage>,
) -> Element {
    let sidx = *streaming_idx.read();

    rsx! {
        div { class: "chat-messages",
            ScrollArea {
                div { class: "chat-messages__list",
                    for (idx, msg) in messages.read().iter().enumerate() {
                        {
                            let is_streaming = sidx == Some(idx);
                            let text = display_text(msg);
                            let class = format!(
                                "chat-message chat-message--{} {}",
                                role_css_class(&msg.role),
                                if is_streaming { "chat-message--streaming" } else { "" }
                            );

                            let r_text: String = reasoning_texts
                                .read()
                                .get(idx)
                                .and_then(|o| o.clone())
                                .unwrap_or_default();
                            let has_reasoning = !r_text.is_empty();
                            let show_streaming_cursor =
                                is_streaming && text.is_empty() && !has_reasoning;

                            rsx! {
                                div {
                                    class: "{class}",
                                    if has_reasoning {
                                        ReasoningView {
                                            text: r_text,
                                            is_streaming: is_streaming,
                                        }
                                    }
                                    if show_streaming_cursor {
                                        "▍"
                                    } else if !text.is_empty() {
                                        Markdown { text: text.to_string() }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
