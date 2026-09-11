# 灵活模式 · 多会话保活设计（切换不打断后台会话）

> 状态：**部分实施**（阶段 1·A「壳 + 常驻宿主」已落地；阶段 2·B **plan 级注册上移已落地**——`use_plan_agent_registrations`/`use_plan_templates` 上移到壳 `FlexiblePage`，controller 不再重复注册；阶段 4·D **工具侧 + 注入侧均已落地**——`flexible_tool.rs` 两工具从 `arguments.session_id` 读、step5 callback 移除、`chat_service_factory` 以 `SystemPrompt::Rendered` 注入 session_id、模板已加「会话上下文」；仅「切模板保留 session_id」为已知项）
> 变更：§3 D / §4.4 已改为「父 prompt 注入 session_id + 工具参数传参」方案，**弃用 task-local**（理由见 §3 D）
> 变更：`ChatConfig.system_prompt_template` → `system_prompt: Option<SystemPrompt>`（`Template(path)` / `Rendered(String)`），原 `context` 字段移除；D 节改用 `SystemPrompt::Rendered` 注入 session_id
> 目标读者：`crates/agent-gui/src/pages/plan/flexible/` 维护者
> 关联：`docs/chat-flexible-流程审查与优化建议.md`、`.qoder/flexible/灵活模式.md`

## 1. 背景与目标

### 1.1 需求

支持同一 plan 下**多个会话并行**，例如 A、B 两个会话：

1. A 发送消息 → LLM 流在 A 的视图里输出。
2. 切到 B → 看到 B 自己的会话；**同时 A 依旧在后台接收 LLM 流并持续持久化**。
3. 切回 A → 看到 A 的最新（含进行中）展示。

一句话：**会话保活**——每个会话各自持有自己的 LLM 流与持久化，前端只决定"切到哪个就显示哪个的投影"，切走**不打断**正在跑的会话。

### 1.2 现状（为什么不满足）

- `FlexiblePage`（`flexible/page.rs:26`）只有**一份** `view` + 一份 `boot`。
- `use_flexible_controller` 的 `use_effect`（`flexible/controller.rs:193`）监听 `session_id`，一旦切换就**重新 boot**；`boot_flexible_session` 在 `flexible/session_boot.rs:72` 用 `*view.write() = view_from_history(...)` 把整份视图**覆盖**。
- 更关键：boot 覆盖 `boot` 信号时，旧的 `FlexBoot::Ready(ReadySession { bridge })`（`session_boot.rs:34`）失去唯一引用 → `Arc<ChatBridge>` 被 drop → `ChatBridge` 的订阅 guard 与内部 `Arc<ChatService>`（`chat_flow/bridge.rs:35`）一起释放 → `driver_loop` 用的是 `Arc::downgrade(state)`（`planned-agent/src/chat/service/service.rs:324`），强引用归零后 **driver 直接退出**。这就是"切走即停"（`controller.rs:112-125` 的 `switch_session` 注释也承认了这一点）。

### 1.3 关键洞察：后台持久化其实"天然成立"

- 写库是 `ChatService` 内部的 `ChatMessageStore → repo`（`flexible/chat_flexible_message_storage.rs`）完成的，**与 UI/订阅者无关**。
- `driver_loop` 独立于订阅者；只要 `Arc<ChatService>` 有强引用，driver 就持续运行、持续落库。

所以「A 后台继续跑 + 持久化」只需要**保证切走不把 `ChatService` 弄死**。问题纯在 UI 层：view 是单份的、bridge 被销毁了。

## 2. 目标模型：会话保活（keep-alive）

```
FlexiblePage（壳）
 ├─ 维护「已打开会话集合」 open_sessions: Vec<session_id>（LRU，容量 N，见决策 1）
 ├─ 渲染 n 个常驻宿主：FlexibleSessionHost { session_id, active }
 │   ├─ A (active=false 时也挂载)  → 自己的 view + ChatBridge + ChatService + driver 仍在跑
 │   └─ B (active=true)            → 渲染 ChatPanel
 └─ 切换 SessionManager.set_active → 只改谁 active，不卸载任何人
```

**为什么这样就满足效果**：

- 保活宿主把 `Arc<ChatBridge>`（内含 `Arc<ChatService>`）一直留在组件里，切走不 drop → **A 的 driver 继续跑**。
- 持久化在 driver 内完成，**与 UI 无关**，A 后台照常落库。
- 每个 host 有自己的 `Signal<ChatView>`（hook 顶层创建，生命周期 = 挂载期），事件回调持续写它；切回 A 读的是**同一个 view**，天然最新（含进行中的 streaming 气泡）。

> 选择该方案而非「集中式 registry + 动态 `Signal<ChatView>`」的原因：完全贴合 dioxus hook 规范（signal 由 `use_signal_sync` 在组件顶层创建），**无需**手搓动态 `Signal::new` 的 owner 生命周期管理。

## 3. 改动清单

### A. 组件层（`crates/agent-gui/src/pages/plan/flexible/`）

| 文件 | 改动 |
|---|---|
| `page.rs` | **拆分**：`FlexiblePage` 变「壳」（管 `open_sessions` + active + 渲染 hosts）；新增 `FlexibleSessionHost` 组件（原来的 controller 调用 + ChatPanel 渲染）。 |
| `controller.rs` | `use_flexible_controller` 签名 `session_id: Signal<String>` → `session_id: String`（host 内固定不变，去掉 `use_listen_session_manager`，监听上移到壳）；删除 `switch_session` 占位（`controller.rs:112-125`）。 |
| `sessions_panel/component.rs` | 不变（`set_active` 已是唯一写入入口，`sessions_panel/component.rs:308`）。 |
| `shared/session.rs` | 不变。 |

原则：**一会话一 host**。

### B. 「plan 级共享注册」上移到壳

现状 `controller.rs:252` 的 `use_hook` 在每个 host 都会跑一次，会导致 `flexible_step1..5`、`flexible_state`、`flexible_rua_demo` / `flexible_max_rounds_demo` 等**重复注册**（`register_tool` 是同名覆盖，会造成会话间互相踩）。

- **模板列表加载**、**子 agent 注册 / 注销**：上移到壳里的一次性 hook（`register` 一次，`use_drop` 注销一次）。
- **`flexible_state` executor / `step5` 落库**：改为在调用时由父 agent 传入 `session_id`（见 D 节），工具本身不再绑定会话。

**实现计划（阶段 2·B）**：

- **上移对象**（当前在 `use_flexible_controller` 内、每个 host 执行一次）：
  1. 两个 custom tool 注册：`flexible_state`、`save_flexible_template`—— executor 仅依赖 `plan_id`、与 session 无关 → 可共享；
  2. 子 agent 注册：`flexible_step1..5`（+ 测试用 `flexible_step_rua_demo` / `flexible_step_max_rounds_demo`）；
  3. `use_drop` 注销；
  4. 模板列表加载（`list_prompts` → `templates`）。
- **新增「自定义 hook」**：`fn use_plan_agent_registrations(plan_id: String)`（dioxus 惯例：`use_` 前缀 fn 内可用 hooks），在壳 `FlexiblePage` 体内调用一次；内部 `require_resource` 取 `Storage/Ai/Tools/Prompt`，`use_hook` 注册 + `use_drop` 注销。
- **模板列表上移后**：`templates` signal 改由壳持有，经 props 传给 `FlexibleSessionHost` → `use_flexible_controller`；controller 不再自建 `templates`、不再有加载 effect。`template`（当前选中项）仍 per-host。
- **依赖与顺序**：注册需 `storage_ctx`（构造 `PlansFlexibleService`）—— 壳渲染时启动门保证资源就绪，与原 controller 同假设。

### C. host 内的会话态

`view` / `input_text` / `thinking` / `temperature` / `template` / `boot` 全部 **per-host**（各自独立）。这正是多会话想要的：A 的输入草稿、模板、忙碌态互不影响。

### D. 副作用工具的会话隔离

原问题：`flexible_state_tool.rs` 与 `step5_callback.rs` 都读 `session_rx.borrow()`（=「当前会话」watch 槽）。并发多会话下，A 的工具会写到 B 的库。

**当前实现状态**：
- ✅ **工具侧已落地**：旧 `flexible_state_tool.rs` + `step5_callback.rs` 已删除，合并为 `flexible_tool.rs`（`flexible_state` + `save_flexible_template`）。二者从 `arguments.session_id` 读（`read_session_id`），不再持 watch 槽；step5 的 `register_sub_agent(..., None)` 已取消回调，原 callback 的 JSON 校验迁入 `SaveTemplateExecutor::execute`；`controller.rs` 已注册两工具并把 `save_flexible_template` 加入白名单。
- ✅ **注入侧已落地**：`chat_service_factory.rs::new_chat_service` 先 `PromptManager::render("flexible/flexible_global_system")`、再拼接「会话上下文」(session_id) 段，以 `SystemPrompt::Rendered` 注入；`flexible_global_system.toml` 已加「会话上下文」说明 + `save_flexible_template` 工具条目，第 9 步路由改为调用该工具。

**采用方案：父 prompt 注入 session_id + 工具参数传参（不改核心库）。**

任务链路全部落在 GUI 侧（工具是 custom tool、prompt 是 GUI 模板），无核心库改动：

1. ✅ **session_id 注入父 prompt（已实现）**：`ChatConfig.system_prompt` 现为 `Option<SystemPrompt>`（`driver/prompt.rs:23-36`）：`Template(path)` 走 `PromptManager`（空 `PromptContext`，带不了变量）、`Rendered(String)` 直接注入。**带 session_id 走 `SystemPrompt::Rendered`**——GUI 在 `new_chat_service`（`chat_service_factory.rs`）先渲染 `flexible_global_system` 模板、再拼接 session_id 说明，作为 `Rendered` 传入。**每个会话自己的 config → 天然 per-session 隔离**。
2. ✅ **落库改为父 agent 显式调用的工具（已实现）**：新增 `save_flexible_template` custom tool，executor 从 `arguments.session_id` 读取并调 `PlansFlexibleService::save_snapshot`。父 prompt 指示：step5 返回 JSON 后调用该工具，`session_id` 照抄 system prompt 给出的值。
3. ✅ **step5 不再用 `SubAgentResultCallback`（已实现）**：`register_sub_agent(..., "flexible_step5", ..., None)`；原 callback 里的 JSON 校验逻辑搬进 `save_flexible_template`。
4. ✅ **`flexible_state` 一并改（已实现）**：同样从 `arguments.session_id` 读（prompt 指示父 agent 每次调用都带上）。

> **为何不用 task-local**：`chat/driver/round/handlers.rs::execute_backend_tool_call` 是核心工具执行路径，加 `task_local` + `ChatConfig.scope_id` + 子 agent 继承，为「传一个 id」动执行内核，成本大于收益。本方案把改动全部收敛在 GUI 侧。

**代价（需实测）**：
- `session_id` 依赖 LLM 从 prompt 照抄回填——参数设为 `required` + schema 描述强约束「原样照抄」，executor 再校验（非法返回可读 error，让父 agent 重试）。
- 原 step5 callback 的 JSON 结构校验 + Retry（`collect.rs:88-131`）要迁移到 `save_flexible_template`，行为有变。
- 多一轮 LLM tool round。

## 4. 代码骨架

### 4.1 `page.rs` —— 壳

```rust
#[derive(Props, Clone, PartialEq)]
pub struct FlexiblePageProps { pub plan_id: String, pub session_id: String }

#[component]
pub fn FlexiblePage(props: FlexiblePageProps) -> Element {
    // 当前会话（dioxus 桥）：SessionManager 驱动，UI 响应式读
    let session_mgr = use_context::<Arc<SessionManager>>();
    let active = session_mgr.current(); // Signal<Option<String>>
    let active_id = active.read().clone().unwrap_or_else(|| props.session_id.clone());

    // 已打开会话集合：首帧含 props.session_id；每出现新 current 就 ensure 加入，
    // 超容量 N 时按 LRU 移除「最久未使用且非 active」的会话（见 §5 决策 1）
    let mut open = use_signal_sync({
        let init = props.session_id.clone();
        move || if init.is_empty() { vec![] } else { vec![init] }
    });
    let mut open_ensure = open; // Copy 句柄
    let active_listen = active;
    use_effect(move || {
        if let Some(id) = active_listen.read().clone() {
            if !id.is_empty() {
                let mut o = open_ensure.write();
                if !o.contains(&id) { o.push(id); }
            }
        }
    });

    // ⚠️ plan 级共享注册（B 节）：一次性 hook + use_drop 注销
    // register_flexible_sub_agents(...) 等从 controller.rs:252 上移到此处

    rsx! {
        div { class: "flexible-page",
            for sid in open.read().iter().cloned() {
                FlexibleSessionHost {
                    key: "{sid}",
                    plan_id: props.plan_id.clone(),
                    session_id: sid.clone(),
                    active: sid == active_id,
                }
            }
        }
    }
}
```

### 4.2 `page.rs` —— 常驻宿主

```rust
#[component]
fn FlexibleSessionHost(plan_id: String, session_id: String, active: bool) -> Element {
    // ⚠️ 所有 hooks 必须在任何 early-return 之前调用（后台 host 也要 boot / 保持订阅）
    let ctl = use_flexible_controller(plan_id.clone(), session_id.clone());

    // 非 active：不渲染 ChatPanel（不订阅 view → 后台写 view 不触发重渲染），但 hooks/订阅照常存活
    if !active {
        return rsx! { div { class: "flexible-host--background" } };
    }

    // active：与今天的 FlexiblePage 尾部一致
    let ready_session = match ctl.boot_phase() {
        FlexBoot::Loading(_) => return rsx! { div { class: "p-4", "灵活模式初始化中…" } },
        FlexBoot::Failed(errs) => return render_boot_error(errs),
        FlexBoot::Ready(s) => s,
    };
    let bridge = ready_session.bridge.clone();
    rsx! {
        PageHeader { /* 同现状；clear_session 只作用于本 host */ }
        ChatPanel {
            view: ctl.view, input_text: ctl.input_text, bridge,
            on_user_action: move |(c, p)| ctl.on_user_action(c, p),
            /* …其余 props 同现状（template/thinking/temperature/on_clear）… */
        }
    }
}
```

### 4.3 `controller.rs` —— 签名与注册调整

```rust
pub(crate) fn use_flexible_controller(plan_id: String, session_id: String) -> FlexibleController {
    // view/input_text/thinking/temperature/template/boot 均不变（每个 host 一份）

    // 去掉：let session_mgr = use_signal_sync(...);   // 不再需要 watch 当前会话

    // boot：session_id 固定 → 只启动一次
    let storage = /* … */;
    use_effect(move || {
        let sid = session_id.clone();               // 固定
        if sid.is_empty() { return; }
        spawn(async move { /* 同现状 boot_flexible_session(...) */ });
    });

    // 移除大段 register_sub_agent / register_custom_tool（上移到 FlexiblePage 壳的一次性 hook）
    // flexible_state executor 构造不再传 session_rx
    FlexibleController { /* … */ }
}
```

### 4.4 会话隔离：父 prompt 注入 + 工具传参（零核心库改动）

> 实现状态：①②③④ 均已落地（`chat_service_factory.rs` 注入 + `flexible_tool.rs` 工具）。下文为落地示意骨架，细节以源码为准。

```rust
// ① GUI：构造本会话 ChatConfig 时注入 session_id
// SystemPrompt::Rendered 直接注入已拼好的字符串；Template 走 PromptManager（空 ctx，带不了变量）
// flexible/chat_service_factory.rs::new_chat_service
let base = prompt_manager
    .render("flexible/flexible_global_system", &PromptContext::new())
    .await?;                                    // 或直接读模板文件内容
let sp = format!(
    "{base}\n\n## 会话上下文\n本会话 session_id = {session_id}。\n\
     凡工具参数含 session_id 的（flexible_state、save_flexible_template），\
     必须原样填入该值，不得改写、不得省略。"
);
ChatConfig {
    system_prompt: Some(SystemPrompt::Rendered(sp)),
    allowed_tools: Some(vec![/* …, "save_flexible_template" */]),
    ..Default::default()
}
```

```toml
# ② flexible/flexible_global_system.toml：把「第 9 步路由」改为
#   step5 返回 JSON 后 → 调 save_flexible_template
#   （session_id 照抄会话上下文中的值，template 传 step5 完整 JSON）
#   → 成功后 flexible_state save current_step="templated"
# 注：不再用 {{ context }} 占位符（session_id 由 ① 的 Rendered 拼接注入）
```

```rust
// ③ 新增保存工具（GUI custom tool，仿 flexible_state）
pub struct SaveTemplateExecutor { plan_id: String, service: Arc<PlansFlexibleService> }

#[async_trait]
impl ToolExecutor for SaveTemplateExecutor {
    async fn execute(&self, _n: &str, args: Value) -> Result<ToolResult> {
        let session_id = args.get("session_id").and_then(Value::as_str)
            .ok_or_else(|| anyhow!("缺少 session_id：请原样传入 system prompt 中的会话 ID"))?;
        let template = &args["template"];               // step5 的 JSON
        // 校验 steps/execution_plan 存在且一一对应（从 step5_callback 迁来）；
        // 不合格 → 返回 is_error ToolResult，让父 agent 重传 / 重跑 step5
        self.service.save_snapshot(&self.plan_id, session_id, /* … */).await?;
        Ok(/* saved */)
    }
    fn supported_tools(&self) -> Vec<String> { vec!["save_flexible_template".into()] }
}
```

```rust
// ④ 取消 step5 的 callback（已实现）
register_sub_agent(/* step5 … */, None);   // 原 step5_callback 传 None
// flexible_state：已改从 args 读 session_id（见 flexible_tool.rs）
```

## 5. 决策记录

1. **已开会话回收策略 = 保留最近 N 个（LRU）** ✅ 已定
   - `open_sessions` 容量上限 `N`（建议常量 5）；超出时移除「**最久未使用**且**非 active**」的会话。
   - 「移除」= 从 `open_sessions` 删除 → host 卸载 → `Arc<ChatService>` 释放 → driver 退出（真正的回收）；移除前先 `bridge.stop()`。
   - 硬约束：**永不移除当前 active 会话**。
   - LRU 计时：`SessionManager` 切换时更新目标会话的「最近访问」时间戳。
2. **D 节机制 = 父 prompt 注入 session_id + 工具参数传参** ✅ 已定（弃用 task-local，理由见 §3 D）
   - `flexible_state` 与 `save_flexible_template` 的 `session_id` 均由父 agent 从 system prompt 照抄、作为工具参数传入。

## 6. 风险与缓解

| 风险 | 缓解 |
|---|---|
| host 全量渲染开销 | 后台 host early-return 不渲染 `ChatPanel`（不订阅 view）；仅首次挂载会 boot（预期内） |
| 子 agent / 工具重复注册 | 注册上移到壳的一次性 hook；`use_drop` 注销也在壳里，避免某 host 卸载连带注销全局工具 |
| **LLM 回填 session_id 不可靠**（写错库主键） | 参数 `required` + schema 描述强约束「原样照抄 system prompt 中的会话 ID」；executor 校验非法即返回可读 error，让父 agent 重试（不静默写错） |
| **step5 校验/重试能力迁移** | ✅ 已迁入 `flexible_tool.rs::SaveTemplateExecutor`（原 `step5_callback` 的结构校验改为返回 `is_error`，由父 agent 决定重跑）；需实测行为等价 |
| 副作用工具串会话 | ✅ 工具侧从 `arguments.session_id` 读 + 注入侧 system prompt 已携带 session_id；仍需并发多会话实测落库归属 |
| `run_id` 与「会话 id」混淆 | `run_id` 是子 agent 挂起-恢复用；会话 id 是落库归属，二者不混用 |
| 已开会话无限增长 | 按决策 1 实现 LRU（保留最近 N），移除前 `stop()` 对应 `ChatService` |
| **切模板会丢掉 session_id** | `apply_template` 走 `set_system_prompt(Some(SystemPrompt::Template(name)))`（`controller.rs:134`），会覆盖构造时 `Rendered` 里的 session_id；切换模板须同样用 `SystemPrompt::Rendered` 重新拼接 session_id |

## 7. 实施顺序（本文件确认后推进）

1. **A + B**：拆分 `page.rs`（壳 + host）、controller 签名调整、注册上移；先让「切走不停、切回正常展示」跑通（D 节暂用旧 watch 兜底）。
2. **C**：确认 per-host 输入框/配置态行为正确。
3. **D**：✅ 已完成——工具侧（`save_flexible_template` + `flexible_state` 改 args 传参 + 取消 step5 callback）+ 注入侧（`SystemPrompt::Rendered` 携带 session_id + 模板「会话上下文」/第 9 步路由）。
4. **回收策略**：按决策 1 实现 LRU（保留最近 N）。
5. 每步 `cargo check` + 实测：A 流中切 B 再切回，验证 A 的后台流与落库完整。

## 8. 实施 TODO（勾选清单）

> 每完成一项勾选；阶段末做一次 `cargo check -p agent-gui`。

### 阶段 0 · 准备

- [x] 决策 1：`open_sessions` 回收策略 = **保留最近 N（LRU）**
- [x] 决策 2：D 节机制 = **父 prompt 注入 session_id + 工具参数传参**（弃用 task-local）
- [ ] 建立基线：`cargo check -p agent-gui` 当前通过

### 阶段 1 · A —— 拆分「壳 + 常驻宿主」 ✅ 代码已完成（`cargo check` 通过）

- [x] `flexible/page.rs`：新增 `FlexibleSessionHost` 组件（提取原 controller 调用 + `PageHeader`/`ChatPanel` 渲染）
- [x] `flexible/page.rs`：`FlexiblePage` 改为壳 —— `open_sessions` signal + `active` 来自 `SessionManager` + `for` 渲染 hosts（带 `key: "{sid}"`）
- [x] `flexible/page.rs`：壳内的 ensure effect —— 新 `current` 若不在 `open_sessions` 则加入（只增不减）
- [x] `flexible/controller.rs`：`use_flexible_controller` 签名 `Signal<String>` → `String`
- [x] `flexible/controller.rs`：去掉 `use_listen_session_manager`（监听上移到壳）
- [x] `flexible/controller.rs`：删除 `switch_session` 占位（`controller.rs:112-125`）
- [x] 宿主内 early-return 之前必须已完成全部 hooks 调用（后台 host 也要 boot）
- [ ] 验证：多会话切换保活（GUI 手动：A 流式中切 B、再切回 A 看 A 最新）；`cargo check` + code review 已通过

### 阶段 2 · B —— plan 级注册上移到壳

- [x] 新增自定义 hook `use_plan_agent_registrations(plan_id: String)`：`use_hook` 内注册 `flexible_state` + `save_flexible_template` + `flexible_step1..5`（+ 测试用 2 个），`use_drop` 注销
- [x] 从 `use_flexible_controller` 移除注册 `use_hook`与 `use_drop`
- [x] 模板列表加载（原 `controller.rs`）上移到壳：壳持有 `templates` + 加载 effect（`use_plan_templates`）
- [x] `use_flexible_controller` 签名增加 `templates: Signal<Vec<String>, SyncStorage>`，移除本地 `templates` signal
- [x] `FlexiblePage` 调 `use_plan_agent_registrations(plan_id)`；`FlexibleSessionHost` 接收 `templates` prop 并透传
- [ ] 验证：多 host 下注册仅一次（切换/新增会话不重复注册、任一 host 卸载不注销全局工具）——`cargo check` 通过；运行期单次性由「`use_hook` 在壳」结构保证（待 GUI 手测）

### 阶段 3 · 多会话联调（C）

- [ ] 验证 A 流式输出中切到 B：B 显示自身会话、不卡顿
- [ ] 验证 A 在后台继续接收 LLM 流
- [ ] 验证 A 在后台继续落库（查 `chat_messages` 有新行）
- [ ] 验证切回 A：展示最新内容（含进行中的 streaming 气泡）
- [ ] 验证 per-host `input_text` / `template` / `thinking` / `temperature` 隔离

### 阶段 4 · D —— 副作用工具会话隔离（父 prompt 注入 + 工具传参）｜工具侧 ✅ / 注入侧 ✅

- [x] `flexible/chat_service_factory.rs`：`new_chat_service` 用 `SystemPrompt::Rendered(渲染后的 prompt + session_id 说明)` 注入
- [x] `flexible_global_system.toml`：加「会话上下文」说明（含 session_id）+ `save_flexible_template` 工具条目 + 第 9 步路由改为调 `save_flexible_template`
- [x] 新增 `flexible/flexible_tool.rs`：`flexible_state` + `save_flexible_template` 两个 custom tool（从 `args.session_id` 读 + 迁入 step5 校验 + `save_snapshot`）—— 合并替代旧 `flexible_state_tool.rs`，`step5_callback.rs` 已删除
- [x] 注册 `save_flexible_template` 到 registry 并加入协调器 `allowed_tools`
- [x] `register_sub_agent(flexible_step5 …, None)`：取消 step5 callback
- [x] `flexible_state` 改从 `arguments.session_id` 读（去掉 `session_rx`）
- [x] 协调器 prompt：指示每次调用 `flexible_state` / `save_flexible_template` 都原样带上 session_id
- [ ] 验证：并发多会话下 `flexible_state` / 模板落库归属正确（不串会话）
- [ ] 实测 step5 校验/重试迁移后的行为等价性

### 阶段 5 · 回收策略接线（依赖阶段 0 决策）

- [ ] 按待决策点 1 选定方案实现（常驻 / 最近 N 回收 + stop 对应 `ChatService`）

### 收尾

- [ ] `cargo check`（含 `agent-gui`、`planned-agent`、`planned-agent-core`）全绿
- [ ] 更新记忆 `[[plans-flexible-session-refactor]]`，记录本改造进度

