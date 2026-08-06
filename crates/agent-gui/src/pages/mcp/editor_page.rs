//! MCP 服务编辑/添加表单视图 —— 嵌套于 `SettingsPage` 右侧内容区
//!
//! 注意：与 `McpListPage` 同，不在 `AppRouter` 注册顶级路由；
//! 由 `SettingsPage` 在 McpService tab 内通过内部视图状态切换显示。
//!
//! - `editing_name = None`  → 添加模式
//! - `editing_name = Some(name)` → 编辑模式（按同名查找并填充表单）
//!
//! 保存 / 取消通过 `on_saved` / `on_back` 事件回到 `McpListPage`。
//!
//! ## 保存后自动刷新
//!
//! 保存成功后：
//! 1. 同步执行 `add_server` / `update_server`（持久化配置）
//! 2. 标记状态为 `Pending`（等待连接），用户回到列表看到"等待连接"
//! 3. 用户需手动点击"刷新工具"触发首次连接
//!
//! ## 刷新中禁用交互
//!
//! 保存期间 (`is_saving = true`)：
//! - 所有按钮（保存 / 取消 / 返回）禁用
//! - 所有输入框禁用
//! - 保存按钮文字变为 "保存中..."

use dioxus::prelude::*;
use planned_agent_mcp_rmcp::McpConfigManager;
use planned_agent_mcp_rmcp::config::McpServerEntry;
use planned_agent_mcp_rmcp::storage::ServerStatus;
use std::sync::Arc;

use crate::components::page_header::PageHeader;
use crate::context::{McpChangeNotifier, McpContext, ToolsContext};

/// 合法分类列表（与 `context::mcp::parse_categories` 中的映射保持一致）
const VALID_CATEGORIES: &[&str] = &[
    "Browser", "File", "Text", "Data", "System", "Device", "Dev", "Utility",
];

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
    // 从 McpContext.bundle 取出 config_manager（同一实例 / 同一后端）
    let config_mgr: Option<McpConfigManager> = use_context::<Resource<Option<std::sync::Arc<McpContext>>>>()
        .read()
        .as_ref()
        .and_then(|x| x.as_ref())
        .map(|c| c.bundle.config_manager().clone());

    // 完整 McpContext Arc（用于保存后自动刷新工具）
    let mcp_ctx_arc: Option<Arc<McpContext>> = use_context::<Resource<Option<std::sync::Arc<McpContext>>>>()
        .read()
        .as_ref()
        .and_then(|x| x.clone());

    // ToolsContext（refresh_tools 需要）
    let tools_ctx_arc: Option<Arc<ToolsContext>> = use_context::<Resource<Option<std::sync::Arc<ToolsContext>>>>()
        .read()
        .as_ref()
        .and_then(|x| x.clone());

    // MCP 变更通知器（让 list_page 监听）
    let notifier = use_context::<McpChangeNotifier>();

    // ── 保存中状态（保存 + 自动刷新期间为 true） ────────────────────────
    let mut is_saving = use_signal(|| false);

    // ── 表单字段（仅在组件挂载时初始化一次） ─────────────────────────────
    let mut form_name = use_signal({
        let editing_name = editing_name.clone();
        let cfg_mgr = config_mgr.clone();
        move || {
            cfg_mgr
                .as_ref()
                .map(|m| lookup_initial(&editing_name, m, String::new(), |s| s.name.clone()))
                .unwrap_or_default()
        }
    });
    let mut form_command = use_signal({
        let editing_name = editing_name.clone();
        let cfg_mgr = config_mgr.clone();
        move || {
            cfg_mgr
                .as_ref()
                .map(|m| lookup_initial(&editing_name, m, String::new(), |s| {
                    s.server_command.clone()
                }))
                .unwrap_or_default()
        }
    });
    let mut form_args = use_signal({
        let editing_name = editing_name.clone();
        let cfg_mgr = config_mgr.clone();
        move || {
            cfg_mgr
                .as_ref()
                .map(|m| lookup_initial(&editing_name, m, String::new(), |s| s.server_args.join(" ")))
                .unwrap_or_default()
        }
    });
    let mut form_timeout = use_signal({
        let editing_name = editing_name.clone();
        let cfg_mgr = config_mgr.clone();
        move || {
            cfg_mgr
                .as_ref()
                .map(|m| lookup_initial(&editing_name, m, 30u64, |s| s.timeout_secs.unwrap_or(30)))
                .unwrap_or_default()
        }
    });
    let mut form_max_retries = use_signal({
        let editing_name = editing_name.clone();
        let cfg_mgr = config_mgr.clone();
        move || {
            cfg_mgr
                .as_ref()
                .map(|m| lookup_initial(&editing_name, m, 3u32, |s| s.max_retries.unwrap_or(3)))
                .unwrap_or_default()
        }
    });
    // ── 分类：多选 chip，直接用 Vec<String> 表达（替代之前的"逗号分隔字符串"） ──
    let mut form_categories = use_signal({
        let editing_name = editing_name.clone();
        let cfg_mgr = config_mgr.clone();
        move || {
            cfg_mgr
                .as_ref()
                .map(|m| {
                    // 读出存储的 Vec<String>（有些老 config 里没有分类字段，取空 Vec）
                    lookup_initial(&editing_name, m, Vec::<String>::new(), |s| {
                        s.categories.clone().unwrap_or_default()
                    })
                })
                .unwrap_or_default()
        }
    });

    // ── 校验状态：名称 + 命令 必须非空 ──
    let name_valid = !form_name.read().trim().is_empty();
    let cmd_valid = !form_command.read().trim().is_empty();
    let is_valid = name_valid && cmd_valid;

    rsx! {
        // Editor 自带局部 header：左上角"← 返回列表"按钮回到 McpList，
        // 使用通用 PageHeader 组件，class="page-header--nested" 切换为 40px 嵌入版。
        // 左侧 nav 与外层 Settings 顶栏仍保留 —— 这是二级深度任务的局部标识。
        PageHeader {
            title: if editing_name.is_some() {
                "编辑 MCP 服务器".to_string()
            } else {
                "添加 MCP 服务器".to_string()
            },
            on_back: Some(on_back),
            back_label: Some("← 返回列表".to_string()),
            back_disabled: Some(*is_saving.read()),
            class: Some("dx-page-header--nested".to_string()),
        }

        // 复用 settings.css 中的 .settings-mcp-form 系列样式（SettingsPage 已加载）。
        // 标题已由上方 header h1 表达，故此处去掉重复的 h4。
        div { class: "settings-mcp-form",
            div { class: "settings-mcp-form__field",
                label {
                    "名称"
                    span { class: "settings-mcp-form__required", "*" }
                }
                input {
                    class: "settings-mcp-form__input",
                    r#type: "text",
                    placeholder: "playwright",
                    value: "{form_name}",
                    disabled: *is_saving.read(),
                    oninput: move |e: FormEvent| form_name.set(e.value()),
                }
                {
                    if !name_valid {
                        rsx! { span { class: "settings-mcp-form__hint", "名称不能为空" } }
                    } else {
                        rsx! { span { class: "settings-mcp-form__hint settings-mcp-form__hint--ok", "✓" } }
                    }
                }
            }

            div { class: "settings-mcp-form__field",
                label {
                    "启动命令"
                    span { class: "settings-mcp-form__required", "*" }
                }
                input {
                    class: "settings-mcp-form__input",
                    r#type: "text",
                    placeholder: "npx",
                    value: "{form_command}",
                    disabled: *is_saving.read(),
                    oninput: move |e: FormEvent| form_command.set(e.value()),
                }
                {
                    if !cmd_valid {
                        rsx! { span { class: "settings-mcp-form__hint", "启动命令不能为空" } }
                    } else {
                        rsx! { span { class: "settings-mcp-form__hint settings-mcp-form__hint--ok", "✓" } }
                    }
                }
            }

            div { class: "settings-mcp-form__field",
                label { "参数（空格分隔）" }
                input {
                    class: "settings-mcp-form__input",
                    r#type: "text",
                    placeholder: "@playwright/mcp@latest",
                    value: "{form_args}",
                    disabled: *is_saving.read(),
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
                        disabled: *is_saving.read(),
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
                        disabled: *is_saving.read(),
                        oninput: move |e: FormEvent| {
                            if let Ok(v) = e.value().parse() {
                                form_max_retries.set(v);
                            }
                        },
                    }
                }
            }

            div { class: "settings-mcp-form__field",
                label { "分类（可多选）" }
                div { class: "settings-mcp-chip-group",
                    for cat in VALID_CATEGORIES.iter() {
                        {
                            let cat_value = (*cat).to_string();
                            let is_selected = form_categories.read().iter().any(|c| c == &cat_value);
                            rsx! {
                                button {
                                    key: "{cat_value}",
                                    r#type: "button",  // 防止表单提交语义
                                    class: format!(
                                        "settings-mcp-chip {}",
                                        if is_selected { "settings-mcp-chip--selected" } else { "" }
                                    ),
                                    disabled: *is_saving.read(),
                                    onclick: {
                                        let cat_value = cat_value.clone();
                                        move |_| {
                                            let mut current = form_categories.read().clone();
                                            if let Some(pos) = current.iter().position(|c| c == &cat_value) {
                                                current.remove(pos);
                                            } else {
                                                current.push(cat_value.clone());
                                            }
                                            form_categories.set(current);
                                        }
                                    },
                                    "{cat}"
                                }
                            }
                        }
                    }
                }
                span { class: "settings-mcp-form__hint settings-mcp-form__hint--ok",
                    if form_categories.read().is_empty() { "未选择分类（将默认归类为 Utility）" }
                    else { "✓ 已选 {form_categories.read().len()} 个分类" }
                }
            }

            div { class: "settings-mcp-form__actions",
                button {
                    class: "settings-mcp-form__btn settings-mcp-form__btn--save",
                    disabled: !is_valid || config_mgr.is_none() || *is_saving.read(),
                    onclick: {
                        let editing_name = editing_name.clone();
                        let mcp_ctx_arc = mcp_ctx_arc.clone();
                        let tools_ctx_arc = tools_ctx_arc.clone();
                        let notifier = notifier;
                        move |_| {
                            // 1. 前置条件
                            let Some(mcp) = mcp_ctx_arc.clone() else {
                                tracing::warn!("McpContext 未就绪，无法保存");
                                return;
                            };
                            let Some(tools) = tools_ctx_arc.clone() else {
                                tracing::warn!("ToolsContext 未就绪，无法保存");
                                return;
                            };

                            // 2. 前置校验（即便按钮已被禁用，也兜底防 race）
                            let name = form_name.read().trim().to_string();
                            let cmd = form_command.read().trim().to_string();
                            if name.is_empty() || cmd.is_empty() {
                                tracing::warn!("MCP 服务器名称/命令不能为空");
                                return;
                            }

                            // 3. 标记保存中（禁用所有交互）
                            is_saving.set(true);

                            // 4. 构建 entry（注意：refresh 用 entry.name 而非 editing_name）
                            let cats_selected = form_categories.read().clone();
                            let entry = McpServerEntry {
                                name: name.clone(),
                                server_command: cmd.clone(),
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
                                categories: if cats_selected.is_empty() {
                                    None
                                } else {
                                    Some(cats_selected)
                                },
                                tools_filter: None,
                                cached_tools: vec![],
                            };
                            let new_name = entry.name.clone();

                            // 5. 同步保存到 config（add 或 update）
                            let cfg_mgr = mcp.bundle.config_manager();
                            let save_result = if let Some(ref old_name) = editing_name {
                                cfg_mgr.update_server(old_name, entry)
                            } else {
                                cfg_mgr.add_server(entry)
                            };

                            match save_result {
                                Ok(_) => {
                                    // 6. 标记为 Pending（等待连接，默认初始状态）
                                    let _ = mcp.bundle.record_status(
                                        &new_name,
                                        ServerStatus::pending(ServerStatus::now()),
                                    );

                                    // 7. 通知 list_page reload（让用户切回时立刻看到"等待连接"）
                                    notifier.bump();

                                    // 8. 返回列表（不自动刷新，用户手动点击"刷新工具"触发连接）
                                    on_saved.call(());
                                }
                                Err(e) => {
                                    tracing::warn!("保存 MCP 服务器失败: {}", e);
                                    is_saving.set(false);  // 解锁，让用户修正后重试
                                }
                            }
                        }
                    },
                    if *is_saving.read() { "保存中..." } else { "保存" }
                }
                button {
                    class: "settings-mcp-form__btn settings-mcp-form__btn--cancel",
                    disabled: *is_saving.read(),
                    onclick: move |_| on_back.call(()),
                    "取消"
                }
            }
        }
    }
}
