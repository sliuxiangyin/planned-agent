use dioxus::prelude::*;

#[component]
pub fn TraceViewer() -> Element {
    rsx! {
        div {
            h2 {
                margin: "0 0 16px 0",
                font_size: "18px",
                color: "#e94560",
                "Trace Logs"
            }
            div {
                padding: "40px",
                text_align: "center",
                color: "#666",
                font_size: "14px",
                line_height: "1.8",

                p { "Trace view is under development..." }
                p {
                    font_size: "12px",
                    "The Agent Think/Act/Observe loop logs"
                    br {}
                    "and detailed tool call information will be displayed here in real-time."
                }
            }
        }
    }
}
