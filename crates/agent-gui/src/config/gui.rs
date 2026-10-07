//! GUI 专属设置（`[gui]`：窗口 / 主题 / 默认视图）。

use serde::{Deserialize, Serialize};

/// GUI 专属设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiSettings {
    /// 窗口标题
    #[serde(default = "default_window_title")]
    pub window_title: String,

    /// 窗口宽度
    #[serde(default = "default_window_width")]
    pub window_width: u32,

    /// 窗口高度
    #[serde(default = "default_window_height")]
    pub window_height: u32,

    /// 主题: "dark" | "light"
    #[serde(default = "default_theme")]
    pub theme: String,

    /// 默认视图: "chat" | "plan" | "trace"
    #[serde(default = "default_view")]
    pub default_view: String,
}

fn default_window_title() -> String {
    "Planned Agent".to_string()
}
fn default_window_width() -> u32 {
    1200
}
fn default_window_height() -> u32 {
    800
}
fn default_theme() -> String {
    "dark".to_string()
}
fn default_view() -> String {
    "chat".to_string()
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            window_title: default_window_title(),
            window_width: default_window_width(),
            window_height: default_window_height(),
            theme: default_theme(),
            default_view: default_view(),
        }
    }
}
