//! MCP 服务视图模块 —— 作为 `SettingsPage` 嵌套子视图使用
//!
//! 结构：
//! - `list_page`   — `McpListPage` 列表视图（不含顶栏）
//! - `editor_page` — `McpEditorPage` 编辑/添加表单视图（不含顶栏）
//!
//! 历史：原本作为顶级 page 由 `AppRouter` 直接渲染。后改为 nested layout：
//! 左侧 nav（`SettingsTab::McpService`）保持不动，右侧直接嵌套渲染本模块的视图。
//! 因此本模块**不**在 `AppRouter` 注册顶级路由，由 `SettingsPage` 内嵌使用。

mod editor_page;
mod list_page;

pub use editor_page::McpEditorPage;
pub use list_page::McpListPage;
