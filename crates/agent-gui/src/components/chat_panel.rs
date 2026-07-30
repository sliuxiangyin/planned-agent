use dioxus::prelude::*;

#[component]
pub fn ChatPanel() -> Element {
    let mut messages = use_signal(Vec::<String>::new);
    let mut input_text = use_signal(String::new);

    let send_message = move |_: Event<MouseData>| {
        let text = input_text.read().clone();
        if !text.is_empty() {
            messages.write().push(format!("You: {}", text));
            input_text.set(String::new());
            // TODO: connect to Agent
            messages.write().push("Agent: (Agent not connected yet, placeholder reply)".to_string());
        }
    };

    rsx! {
        div {
            display: "flex",
            flex_direction: "column",
            height: "100%",

            h2 {
                margin: "0 0 16px 0",
                font_size: "18px",
                color: "#e94560",
                "Chat"
            }

            // Message list
            div {
                flex: "1",
                overflow_y: "auto",
                padding: "12px",
                background_color: "#1a1a2e",
                border_radius: "8px",
                border: "1px solid #0f3460",
                margin_bottom: "12px",

                for msg in messages.iter() {
                    div {
                        padding: "8px 12px",
                        margin_bottom: "8px",
                        background_color: "#16213e",
                        border_radius: "6px",
                        font_size: "14px",
                        line_height: "1.5",
                        "{msg}"
                    }
                }
            }

            // Input area
            div {
                display: "flex",
                gap: "8px",

                input {
                    flex: "1",
                    padding: "10px 14px",
                    border: "1px solid #0f3460",
                    border_radius: "6px",
                    background_color: "#16213e",
                    color: "#e0e0e0",
                    font_size: "14px",
                    placeholder: "Type a message... (Agent not connected)",
                    value: "{input_text}",
                    oninput: move |evt| input_text.set(evt.value()),
                }

                button {
                    padding: "10px 20px",
                    border: "none",
                    border_radius: "6px",
                    background_color: "#e94560",
                    color: "#fff",
                    font_size: "14px",
                    cursor: "pointer",
                    onclick: send_message,
                    "Send"
                }
            }
        }
    }
}
