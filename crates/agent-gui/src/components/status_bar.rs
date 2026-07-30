use dioxus::prelude::*;

#[component]
pub fn StatusBar() -> Element {
    rsx! {
        footer {
            display: "flex",
            align_items: "center",
            justify_content: "space-between",
            padding: "8px 20px",
            background_color: "#16213e",
            border_top: "1px solid #0f3460",
            font_size: "12px",
            color: "#888",

            span { "Agent: disconnected" }
            span { "Tools: 0 loaded" }
            span { "Session: none" }
            span { "Idle" }
        }
    }
}
