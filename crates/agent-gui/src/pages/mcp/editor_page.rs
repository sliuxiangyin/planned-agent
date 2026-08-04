//! MCP 服务编辑/添加表单视图 —— 嵌套于 `SettingsPage` 右侧内容区
//!
//! 注意：与 `McpListPage` 同，不在 `AppRouter` 注册顶级路由；
//! 由 `SettingsPage` 在 McpService tab 内通过内部视图状态切换显示。
//!
//! - `editing_name = None`  → 添加模式
//! - `editing_name = Some(name)` → 编辑模式（按同名查找并填充表单）
//!
//! 保存 / 取消通过 `on_saved` / `on_back` 事件回到 `McpListPage`。

use dioxus::prelude::*;
use planned_agent_mcp_rmcp::McpConfigManager;
use planned_agent_mcp_rmcp::config::McpServerEntry;

/// 从配置读取与 `editing_name` 匹配的 entry 并通过 `extract` 提取一个字段；
/// 找不到 / 解析失败时返回 `default`。该函数仅在 `use_signal` 初始化闭包里调用一次。
fn lookup_initial<T, F>(
    editing_name: &Option<String>,
    cfg_mgr: &McpConfigManager,
    default: T,
    extract: F,
) -> T
where
    F: FnOnce(&McpServerEntry) -> T,
{
    let Some(name) = editing_name.as_ref() else {
        return default;
    };
    let Ok(cfg) = cfg_mgr.load_config() else {
        return default;
    };
    let Some(entry) = cfg.servers.iter().find(|s| &s.name == name) else {
        return default;
    };
    extract(entry)
}

#[component]
pub fn McpEditorPage(
    editing_name: Option<String>,
    on_back: EventHandler<()>,
    on_saved: EventHandler<()>,
) -> Element {
    let config_mgr = McpConfigManager::new(McpConfigManager::DEFAULT_PATH);

    // ── 表单字段（仅在组件挂载时初始化一次） ─────────────────────────────
    let mut form_name = use_signal({
        let editing_name = editing_name.clone();
        let cfg_mgr = config_mgr.clone();
        move || {
            lookup_initial(&editing_name, &cfg_mgr, String::new(), |s| s.name.clone())
        }
    });
    let mut form_command = use_signal({
        let editing_name = editing_name.clone();
        let cfg_mgr = config_mgr.clone();
        move || {
            lookup_initial(&editing_name, &cfg_mgr, String::new(), |s| {
                s.server_command.clone()
            })
        }
    });
    let mut form_args = use_signal({
        let editing_name = editing_name.clone();
        let cfg_mgr = config_mgr.clone();
        move || lookup_initial(&editing_name, &cfg_mgr, String::new(), |s| s.server_args.join(" "))
    });
    let mut form_timeout = use_signal({
        let editing_name = editing_name.clone();
        let cfg_mgr = config_mgr.clone();
        move || lookup_initial(&editing_name, &cfg_mgr, 30u64, |s| s.timeout_secs.unwrap_or(30))
    });
    let mut form_max_retries = use_signal({
        let editing_name = editing_name.clone();
        let cfg_mgr = config_mgr.clone();
        move || lookup_initial(&editing_name, &cfg_mgr, 3u32, |s| s.max_retries.unwrap_or(3))
    });
    let mut form_categories = use_signal({
        let editing_name = editing_name.clone();
        let cfg_mgr = config_mgr.clone();
        move || {
            lookup_initial(&editing_name, &cfg_mgr, String::new(), |s| {
                s.categories
                    .as_ref()
                    .map(|c| c.join(", "))
                    .unwrap_or_default()
            })
        }
    });

    rsx! {
        // Editor 自带局部 header：左上角"← 返回列表"按钮回到 McpList，
        // 复用 settings.css 中的 .settings-topbar / .settings-back-btn 类。
        // 左侧 nav 与外层 Settings 顶栏仍保留 —— 这是二级深度任务的局部标识。
        header { class: "settings-topbar settings-topbar--nested",
            button {
                class: "settings-back-btn",
                onclick: move |_| on_back.call(()),
                "← 返回列表"
            }
            h1 { class: "settings-topbar__title",
                if editing_name.is_some() {
                    "编辑 MCP 服务器"
                } else {
                    "添加 MCP 服务器"
                }
            }
        }

        // 复用 settings.css 中的 .settings-mcp-form 系列样式（SettingsPage 已加载）。
        // 标题已由上方 header h1 表达，故此处去掉重复的 h4。
        div { class: "settings-mcp-form",
            div { class: "settings-mcp-form__field",
                label { "名称" }
                input {
                    class: "settings-mcp-form__input",
                    r#type: "text",
                    placeholder: "playwright",
                    value: "{form_name}",
                    oninput: move |e: FormEvent| form_name.set(e.value()),
                }
            }

            div { class: "settings-mcp-form__field",
                label { "启动命令" }
                input {
                    class: "settings-mcp-form__input",
                    r#type: "text",
                    placeholder: "npx",
                    value: "{form_command}",
                    oninput: move |e: FormEvent| form_command.set(e.value()),
                }
            }

            div { class: "settings-mcp-form__field",
                label { "参数（空格分隔）" }
                input {
                    class: "settings-mcp-form__input",
                    r#type: "text",
                    placeholder: "@playwright/mcp@latest",
                    value: "{form_args}",
                    oninput: move |e: FormEvent| form_args.set(e.value()),
                }
            }

            div { class: "settings-mcp-form__row",
                div { class: "settings-mcp-form__field",
                    label { "超时(秒)" }
                    input {
                        class: "settings-mcp-form__input settings-mcp-form__input--small",
                        r#type: "number",
                        value: "{form_timeout}",
                        oninput: move |e: FormEvent| {
                            if let Ok(v) = e.value().parse() {
                                form_timeout.set(v);
                            }
                        },
                    }
                }
                div { class: "settings-mcp-form__field",
                    label { "重试次数" }
                    input {
                        class: "settings-mcp-form__input settings-mcp-form__input--small",
                        r#type: "number",
                        value: "{form_max_retries}",
                        oninput: move |e: FormEvent| {
                            if let Ok(v) = e.value().parse() {
                                form_max_retries.set(v);
                            }
                        },
                    }
                }
            }

            div { class: "settings-mcp-form__field",
                label { "分类（逗号分隔，如: Browser, File）" }
                input {
                    class: "settings-mcp-form__input",
                    r#type: "text",
                    placeholder: "Browser",
                    value: "{form_categories}",
                    oninput: move |e: FormEvent| form_categories.set(e.value()),
                }
            }

            div { class: "settings-mcp-form__actions",
                button {
                    class: "settings-mcp-form__btn settings-mcp-form__btn--save",
                    onclick: {
                        let cfg_mgr = config_mgr.clone();
                        let editing_name = editing_name.clone();
                        move |_| {
                            let entry = McpServerEntry {
                                name: form_name.read().clone(),
                                server_command: form_command.read().clone(),
                                server_args: form_args
                                    .read()
                                    .split_whitespace()
                                    .map(|s| s.to_string())
                                    .filter(|s| !s.is_empty())
                                    .collect(),
                                transport: "stdio".into(),
                                timeout_secs: Some(*form_timeout.read()),
                                max_retries: Some(*form_max_retries.read()),
                                is_default: false,
                                categories: {
                                    let cats: Vec<String> = form_categories
                                        .read()
                                        .split(',')
                                        .map(|s| s.trim().to_string())
                                        .filter(|s| !s.is_empty())
                                        .collect();
                                    if cats.is_empty() {
                                        None
                                    } else {
                                        Some(cats)
                                    }
                                },
                                tools_filter: None,
                                cached_tools: vec![],
                            };

                            let result = if let Some(ref name) = editing_name {
                                cfg_mgr.update_server(name, entry)
                            } else {
                                cfg_mgr.add_server(entry)
                            };

                            match result {
                                Ok(_) => on_saved.call(()),
                                Err(e) => {
                                    tracing::warn!("保存 MCP 服务器失败: {}", e);
                                }
                            }
                        }
                    },
                    "保存"
                }
                button {
                    class: "settings-mcp-form__btn settings-mcp-form__btn--cancel",
                    onclick: move |_| on_back.call(()),
                    "取消"
                }
            }
        }
    }
}
