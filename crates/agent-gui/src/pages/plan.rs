use crate::components::button::{Button, ButtonVariant};
use crate::components::resizable_panel::ResizablePanel;
use crate::components::scroll_area::ScrollArea;
use crate::components::textarea::Textarea;
use dioxus::prelude::*;

/// 聊天消息
#[derive(Clone, PartialEq)]
struct Message {
    id: usize,
    content: String,
    role: MessageRole,
    /// 是否正在流式输出
    streaming: bool,
}

#[derive(Clone, PartialEq)]
enum MessageRole {
    User,
    Assistant,
}

#[component]
pub fn PlanPage() -> Element {
    // ── 聊天状态 ──
    let messages = use_signal(|| {
        vec![Message {
            id: 1,
            content: "欢迎来到 Plan 页面！".into(),
            role: MessageRole::Assistant,
            streaming: false,
        }]
    });
    let mut input_text = use_signal(|| String::new());
    let next_id = use_signal(|| 2usize);

    // ── 发送消息 ──
    fn send_message(
        mut input_text: Signal<String>,
        mut messages: Signal<Vec<Message>>,
        mut next_id: Signal<usize>,
    ) {
        let text = input_text().trim().to_string();
        if text.is_empty() {
            return;
        }
        let id = next_id();
        messages.write().push(Message {
            id,
            content: text,
            role: MessageRole::User,
            streaming: false,
        });
        input_text.set(String::new());
        next_id.set(id + 1);
    }

    // ── 右侧聊天面板 ──
    let chat_panel = rsx! {
        div { class: "chat-panel",

            // 消息展示区
            div { class: "chat-messages",
                ScrollArea {
                    div { class: "chat-messages__list",
                        for msg in messages.read().iter() {
                            div {
                                class: "chat-message chat-message--{msg.role.css_class()} {msg.streaming_class()}",
                                "{msg.content}"
                            }
                        }
                    }
                }
            }

            // 输入发送区
            div { class: "chat-input-area",
                Textarea {
                    placeholder: "输入消息...",
                    value: "{input_text}",
                    oninput: move |e: FormEvent| input_text.set(e.value()),
                    onkeydown: move |e: KeyboardEvent| {
                        if e.data.key() == keyboard_types::Key::Enter && !e.data.modifiers().shift() {
                            e.prevent_default();
                            send_message(input_text, messages, next_id);
                        }
                    },
                }
                Button {
                    variant: ButtonVariant::Primary,
                    onclick: move |_: MouseEvent| send_message(input_text, messages, next_id),
                    "发送"
                }
            }
        }
    };

    rsx! {
        div { class: "plan-page",
            ResizablePanel {
                initial_left_percent: 70.0,
                min_left_percent: 25.0,
                max_left_percent: 75.0,
                left: rsx! {
                    div { class: "plan-left-panel",
                        span { class: "plan-left-panel__label",
                            "左侧区域（待开发）"
                        }
                    }
                },
                right: chat_panel,
            }
        }
    }
}

impl MessageRole {
    fn css_class(&self) -> &'static str {
        match self {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
        }
    }
}

impl Message {
    fn streaming_class(&self) -> &'static str {
        if self.streaming { "chat-message--streaming" } else { "" }
    }
}