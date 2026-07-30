use dioxus::prelude::*;

#[component]
pub fn PlanTimeline() -> Element {
    rsx! {
        div {
            h2 {
                margin: "0 0 16px 0",
                font_size: "18px",
                color: "#e94560",
                "Plan Timeline"
            }
            div {
                padding: "40px",
                text_align: "center",
                color: "#666",
                font_size: "14px",
                line_height: "1.8",

                p { "Plan view is under development..." }
                p {
                    font_size: "12px",
                    "When the Agent generates an execution plan, each step"
                    br {}
                    "and its execution status will be displayed here as a timeline."
                }
            }
        }
    }
}
