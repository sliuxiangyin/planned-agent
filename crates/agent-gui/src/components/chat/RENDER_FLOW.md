# chat 模块渲染流程文档

> 覆盖 `crates/agent-gui/src/components/chat/` 全部实现：从服务端事件到最终 UI 的完整数据流。
> 阅读顺序建议：模块总览 → 数据模型 → turn 生命周期 → 事件消费 → 渲染 → Tool 生命周期 → 历史加载 → 交互卡片。

---

## 1. 模块总览

```
chat/
├── mod.rs                    # 模块入口，pub use ChatPanel / AgentView
├── chat_flow/                # 消息流转（数据层 + 业务编排）
│   ├── types.rs              #   纯数据类型：Bubble / ToolViewData / AgentViewData / PendingUI / AgentEvent
│   ├── view.rs               #   ChatView 值类型 + 值操作方法 + 状态查询
│   ├── reduce.rs             #   纯 reducer：reduce（增量）+ view_from_history（全量）
│   └── bridge.rs             #   ChatBridge：唯一接触 ChatService 的桥接层
├── agent_view/               # 子 agent 输出的嵌入式卡片
│   ├── component.rs          #   AgentView 组件（Bot 图标 + 渐变线 + Markdown 渲染）
│   └── style.css
├── chat_panel/               # 完整聊天面板（消息列表 + 输入区 + composer 工具栏）
│   ├── component.rs          #   ChatPanel 组件（只读渲染，ToolView/AgentView 分流）
│   └── style.css
├── chat_ui_actions_view/     # Agent 交互卡片（reasonix 式并列 questions 问题卡）
├── reasoning_view/           # Assistant「深度思考」折叠面板
└── tool_view/                # Tool 调用详情折叠卡片
```

| 模块 | 职责 | 关键导出 |
|---|---|---|
| `chat_flow::view` | 会话 UI 投影值类型 + 值操作/查询 | `ChatView` |
| `chat_flow::reduce` | 事件/历史 → `ChatView` 的纯翻译 | `reduce`、`view_from_history` |
| `chat_flow::bridge` | `ChatService` ↔ `Signal<ChatView>` 唯一接线点 | `ChatBridge` |
| `chat_flow::types` | 纯数据类型（无逻辑） | `Bubble`、`PendingUI`、`ToolViewData` 等 |
| `agent_view` | 子 agent 输出渲染 | `AgentView` |
| `chat_panel` | 纯 UI 布局，不持有业务逻辑 | `ChatPanel`、`template_label` |
| `chat_ui_actions_view` | `request_user_action` 交互卡片 | `ChatUIActionsView` |
| `reasoning_view` | 推理内容折叠面板 | `ReasoningView` |
| `tool_view` | 工具调用折叠卡片 | `ToolView` |

**分层原则**：`chat_flow` 只操作 `ChatView` 值、不渲染；`chat_panel` 只渲染、不碰服务；页面层（如 `pages/plan/flexible/`）负责构造 `ChatService`、`ChatBridge`，把 `Signal<ChatView>` 与桥传入 `ChatPanel`。

**单一订阅桥**：组件层不直接 `import ChatService`，只依赖 `ChatBridge` + `Signal<ChatView>`。事件由桥内部的唯一订阅回调 `reduce` 进 `view`，UI 只读这一个投影。

---

## 2. 数据模型

### 2.1 `ChatView`（view.rs）

单一会话 UI 投影，把原先散落在 6 个 `Signal` 里的会话状态收敛为**一个可整体读写的值类型**：

```rust
#[derive(Clone, PartialEq, Default)]
pub struct ChatView {
    pub bubbles: Vec<Bubble>,                          // 历史气泡（已完成的 turn）
    pub active: Vec<Bubble>,                           // 当前 turn 气泡组（流式增量更新）
    pub agent_views: HashMap<String, AgentViewData>,   // 子 agent 流式数据（key = tool_call_id）
    pub pending_ui: Option<PendingUI>,                 // 交互卡片
    pub pending_tool_call_id: Option<String>,          // request_user_action 的 tool_call_id
}
```

**不含** `input_text`（输入框文本，组件局部 UI 态，由 `chat_panel` 自己维护）与 `subscription`（事件订阅 guard，由 `ChatBridge` 持有）。

**核心设计**：流式事件只增量更新 `active`（O(当前 turn 轮数)），`Done` 时整组并入 `bubbles`。`active` 通常只有 1~3 条气泡，历史 `bubbles` 完全不动——这就是性能优化的本质。

`ChatView` 是纯值类型（无 dioxus signal 依赖），可脱离 dioxus 单测。值操作方法（`push_user_turn` / `append_streaming_text` / `tool_call_start` …）与只读查询方法（`is_streaming` / `is_busy` …）都定义在 `impl ChatView` 上。

**持久化由服务端 `History` + `ChatHistoryStore` 处理**——GUI 不再持有 `messages`、`sequence_order` 等持久化相关字段。

### 2.2 `Bubble`（types.rs）

扁平化气泡 = 消息数据 + 渲染数据合体，单一数据源：

```rust
pub struct Bubble {
    pub is_assistant: bool,           // false = user 气泡，true = assistant 气泡
    pub text: String,                 // 可显示文本
    pub reasoning: String,            // 思考链内容（assistant 独有）
    pub is_streaming: bool,           // 是否流式中（驱动光标 / 脉冲动画）
    pub tool_calls: Vec<ToolViewData>, // 工具面板（仅 assistant 使用）
}
```

### 2.3 工具相关：`ToolViewData`（单一数据源）

`ToolViewData` 是**唯一**工具数据源，`name`/`arguments`/`phase`/`result`/`is_error`/`is_sub_agent` 全部内聚，事件直接驱动（`tool_call_start` 建、`append_args`/`complete`/`executed` 就地更新）：

```rust
pub struct ToolViewData {
    pub tool_call_id: String,
    pub name: String,
    pub arguments: String,            // 流式累积 / complete 覆写 pretty JSON
    pub phase: ToolCallPhase,         // Pending / Running / Completed / Error
    pub result: Option<serde_json::Value>,
    pub is_error: bool,
    pub is_sub_agent: bool,           // true → 渲染 AgentView 而非 ToolView
}
```

> 已删除旧的双源结构 `Message.tool_calls`（name/arguments）+ `ToolCallEntry`（phase/result），
> 消除了「多副本同步错位」问题。

### 2.4 `PendingUI`（交互卡片）

```rust
pub struct PendingUI {
    pub message: String,           // 引导文本
    pub questions: Vec<UIQuestion>,// 并列问题列表（≤4，彼此独立）
    pub tool_call_id: String,      // confirm_user_action 回填用
    pub run_id: Option<String>,    // Some = 子 agent 挂起（resume 路径）；None = 主 agent
}
```

`UIQuestion`（core 层 `crates/core/src/events/ui_action.rs`）是唯一交互原语「问题 + options」，不再有平铺的 `UIAction`/`type` 枚举（Confirm/Select/Input/MultiSelect 均废弃）：

```rust
pub struct UIQuestion {
    pub header: String,          // 短标签（≤4 字，同批内唯一），答案回传键
    pub question: String,        // 问题全文
    pub options: Vec<UIOption>,  // 用户可点的选项（2..5 个）
    pub multi: bool,             // false=单选（默认）；true=多选，前端自动补「提交」
    pub allow_input: bool,       // 默认 true=带「自定义回答」输入框
}
pub struct UIOption {
    pub label: String,            // 人看的文本
    pub description: Option<String>, // 可选 tooltip
    pub value: Option<String>,    // 机器用的实际值，缺省回 label
}
```

---

## 3. turn 生命周期

一次「发送 → 完成」= 一个 turn = `active` 气泡组：

```
发送 "帮我看看目录"
  ├─ active.push(user 气泡)
  ├─ active.push(assistant 占位气泡, is_streaming=true)
  │
  │   （流式事件增量更新 active 的最后一条 streaming 气泡）
  │   TextDelta → text += chunk
  │   ToolCallStart → tool_calls.push(ToolViewData{Pending})
  │   ...（工具执行，ToolExecuted 回填 result）
  │
  ├─ RoundEnd → stop_streaming（active 内全部 is_streaming=false）
  │   （下一轮 RoundStart → 再 push 一条 assistant 占位气泡）
  │
  └─ Done → finish_turn()：bubbles.append(active)，active.clear()
```

关键点：
- **turn 内所有气泡留在 `active`**，`Done` 前随时可改（覆盖 `ToolExecuted` 迟到回填）。
- **`tool` 不是独立气泡**，作为工具面板挂在「发起它的 assistant 气泡」里。
- **组内是多条 assistant 气泡**（每轮 tool 调用后开一条），不是一整条。

这些值操作方法（`push_user_turn` / `push_assistant_placeholder` / `finish_turn` / `stop_streaming` …）定义在 `ChatView` 上，由 `reduce`（增量）或 `bridge.send`（发送时预建 turn）调用。

---

## 4. 事件 → 状态消费（reduce.rs `reduce`）

`reduce(view: &mut ChatView, ev: &ServiceChatEvent)` 是唯一的事件翻译入口，由 `ChatBridge::connect` 的订阅回调调用（`reduce(&mut *view.write(), &ev)`）。

| 服务端事件 | ChatView 动作 | 说明 |
|---|---|---|
| `Chat(TextDelta)` | `append_streaming_text` | 追加到 `active` 最后一条 streaming 气泡 |
| `Chat(ReasoningDelta)` | `append_streaming_reasoning` | 同上，追加 reasoning |
| `Chat(RoundStart)` | 若未 streaming 则 `push_assistant_placeholder` | 防御性（send 已预建占位） |
| `Chat(ToolCallStart)` | `tool_call_start(id, name, is_sub_agent)` | 建 `ToolViewData`；`is_sub_agent` 时同时建 `AgentViewData` |
| `Chat(ToolCallArgsDelta)` | `tool_call_append_args` | 追加 `arguments` |
| `Chat(ToolCallComplete)` | `tool_call_complete` | `arguments` 覆写 + `phase=Running` |
| `Chat(ToolExecuted)` | `tool_call_executed` + `finish_agent_view` | `phase=Completed/Error` + `result`；子 agent 同步更新 `AgentView` |
| `Chat(SubChat)` | `push_agent_event` | 子 agent 流式事件（TextDelta/ReasoningDelta）攒入 `agent_views` |
| `Chat(RoundEnd)` | `stop_streaming` | 轮次结束 |
| `Chat(UIActionRequest)` | `set_pending` | 弹出交互卡片 |
| `Done` | `stop_streaming` + **`finish_turn`** + `clear_pending` | turn 收尾并入历史 |
| `Error(e)` | `render_error` + `stop_streaming` + **`finish_turn`** + `clear_pending` | 收尾 |
| `HistoryUpdated` | `reconcile_with_snapshot` | 保持注释（启用时用快照校准 bubbles） |

**发送链路**（`bridge.send`）：`clear_pending` → `push_user_turn`（push User + 预建 streaming Assistant 占位）→ `svc.send_text(text)`。

`reduce` 是纯函数（只改 `&mut ChatView`，不触碰 dioxus signal），可脱离 dioxus 单测。

---

## 5. 渲染管线（chat_panel/component.rs）

### 5.1 总览

```
Signal<ChatView>（组件 read 一次）
        │
        ├─ view.bubbles (历史气泡)  +  view.active (当前 turn 气泡)
        │        └────────── bubbles.iter().chain(active.iter()) ──────────┐
        │                                                                   ▼
        │                                              render_assistant_bubble / render_user_bubble
        │                                                                   │
        │                                                                   ▼
        │                                    ┌─ ToolViewData.is_sub_agent ─┼─ false → ToolView
        │                                    └─ true → AgentView（读 view.agent_views[tool_call_id]）
        └─ view.pending_ui (交互卡片，输入框上方渲染 ChatUIActionsView)
                                    + ReasoningView + Markdown(text) + 光标
```

组件对 `view` 做一次 `read()`，再借出 `bubbles` / `active` / `agent_views` / `pending_ui` 字段，不做任何构建。发送/停止通过 `bridge.send()` / `bridge.stop()`；输入框文本由独立的 `input_text: Signal<String>` 维护。

### 5.2 单条 assistant 气泡渲染顺序（与 ChatGPT/Claude 一致）

```
1. ReasoningView（如有推理内容，流式时脉冲动画）
2. Markdown(text)（工具调用前说的话，显示在工具面板之前）
3. tool_calls 分流：
   - is_sub_agent == false → ToolView（折叠面板：Input/Output）
   - is_sub_agent == true  → AgentView（嵌入式卡片：Bot 图标 + 渐变线 + Markdown）
4. 光标 "▍"（仅当 streaming 且 text/reasoning/tool_calls 全空）
```

### 5.3 输入区 + Composer 工具栏

- `Textarea`：Enter 发送（Shift+Enter 换行），busy 时禁用
- 工具栏：模板选择、思考模式、温度选择
- 发送/停止按钮：busy → 停止（`bridge.stop()`）；空闲 → 发送（`bridge.send()`）

---

## 6. 子 Agent 事件流（SubChat）

### 6.1 事件转发规则（`sub_agent/mod.rs::collect_until_outcome`）

| 子 agent 事件 | 转发方式 | 说明 |
|---|---|---|
| `UIActionRequest` | `Chat(CoreChatEvent::UIActionRequest)` | 主 agent GUI 直接处理交互卡片 |
| `RoundStart` / `RoundEnd` | 不转发 | 避免主 agent 创建多余气泡 |
| `TextDelta` / `ReasoningDelta` | `Chat(CoreChatEvent::SubChat { tool_call_id, event })` | 路由到 `agent_views[tool_call_id]` |
| `ToolCall*` / `ToolExecuted` | `Chat(CoreChatEvent::SubChat { tool_call_id, event })` | 子 agent 内部工具调用 |

### 6.2 数据流

```
子 agent 内部事件
  │
  ├─ UIActionRequest → Chat(UIActionRequest) → 主 agent GUI 弹交互卡片
  ├─ RoundStart/End  → 不转发
  └─ 其余           → Chat(SubChat { tool_call_id, event })
                          │
                          ▼
                    reduce.rs: view.push_agent_event(tool_call_id, AgentEvent)
                          │
                          ▼
                    view.agent_views[tool_call_id].events += AgentEvent::TextDelta/ReasoningDelta
                          │
                          ▼
                    AgentView 组件读取并渲染 Markdown
```

### 6.3 子 agent 完成信号

```
子 agent 完成 → collect_until_outcome 返回 Done
  → SubAgentToolExecutor 返回 ToolResult
  → 父 agent run_conversation emit Chat(ToolExecuted { id, is_error })
  → reduce.rs: view.finish_agent_view(id, Completed/Error)
  → AgentView 状态更新为 Completed/Error，停止 streaming
```

### 6.4 子 agent 持久化（is_agent_tool）

`StoreMessage.is_agent_tool` 标记 assistant 消息的 tool_calls 是否包含 SubAgent 工具。

**保存链路**：
```
History::push_assistant(msg)
  → 检查 msg.tool_calls 里每个 name 的 ToolSource
  → 若有 SubAgent → StoreMessage.is_agent_tool = true
  → store.append → DB: chat_messages.is_agent_tool = true
```

**加载链路**：
```
store.load → StoreMessage.is_agent_tool = true
  → view_from_history → 为 is_sub_agent 工具创建 AgentViewData
  → AgentView 回显（嵌入式卡片 + 最终文本）
```

**数据库字段**：`chat_messages.is_agent_tool: BOOLEAN, default false`（直接在迁移文件定义，无历史兼容负担）。

---

## 7. Tool 调用生命周期（完整时序）

以"列目录 → 读文件"为例：

```
bridge.send → active: [user, assistant#1(streaming)]
TextDelta:  "目录下共发现 3 个文件：…"   → assistant#1.text += chunk
ToolCallStart(id="call_x", name="builtin_read_file")
  → assistant#1.tool_calls += ToolViewData{Pending}
ToolCallArgsDelta → ToolViewData.arguments += delta
ToolCallComplete → arguments 覆写 pretty JSON；phase=Running
RoundEnd         → assistant#1.is_streaming=false
ToolExecuted     → 遍历 active 找到 assistant#1.tool_calls[call_x]；phase=Completed + result
RoundStart       → active.push(assistant#2 占位, streaming)
TextDelta:  ".env 内容如下：…"          → assistant#2.text += chunk
RoundEnd         → assistant#2.is_streaming=false
Done             → finish_turn()：bubbles += [user, assistant#1, assistant#2]
```

渲染呈现：

```
[User]  获取目录文件列表…
[Assistant]  builtin_list_dir 面板（ToolView，含结果）
[Assistant]  "目录下共发现 3 个文件：…" 文本
             builtin_read_file 面板（ToolView，含结果）
[Assistant]  ".env 内容如下：…"
```

### 流式阶段视觉

- `Pending`：脉冲动画（等待参数）
- `Running`：旋转加载图标 + "等待执行…"
- `Completed`：对勾
- `Error`：红色边框 + X 图标
- 流式光标只在无任何内容时显示（有工具面板时让位给 ToolView 动画）

### 异常与兜底（全部在 active 区内 O(turn) 实现）

- **ArgsDelta 先于 ToolCallStart**：服务端已修复（accumulator 缓冲 + Start 后 flush）；气泡侧 find 不到即丢弃
- **Complete 缺失 Start**：`tool_call_complete` 用事件自带 `name` 补建 `ToolViewData{Running}`
- **Executed 缺失 Start/Complete**：`tool_call_executed` 补建到 `active` 最后 assistant 气泡
- **`request_user_action`（UI 工具）**：GUI 刻意跳过其 Start/Complete，由交互卡片替代，Executed 时文本化
- **`ToolExecuted` 迟到**：遍历 `active` 全部气泡（非 streaming 也可命中）

---

## 8. 历史加载与快照校准

### 8.1 全量重建（`view_from_history`，reduce.rs）

`view_from_history(&[StoreMessage]) -> ChatView` 从服务端历史快照重建完整投影（低频、全量），是唯一的历史翻译入口，替代旧的 `build_bubbles` + `load_from_history` 两段：

| 消息角色 | 行为 |
|---|---|
| `User` | 独立 user 气泡 |
| `Assistant` | 独立 assistant 气泡（reasoning + text + tool_calls，phase 默认 Completed） |
| `Tool` | 正常工具按 `tool_call_id` 回填对应 `ToolViewData.result`；`request_user_action` 的 Tool 文本化进气泡/agent_views |
| 其他（System 等） | 忽略 |

内部维护 `tool_index: HashMap<tool_call_id, (bubble_idx, tool_idx)>`，Tool 消息精确回填；同时从 `is_agent_tool` 消息提取子 agent 骨架（`AgentViewData`）。

### 8.2 GUI 侧历史加载

```
session_boot.rs (boot_flexible_session):
  1. 构造 ChatMessageStore(plan_id, repo)
  2. 注入 ChatService → 服务端 History 自动 load()
  3. service.history() 获取已恢复的 Vec<StoreMessage>
  4. *view.write() = view_from_history(&history)
  5. bridge = ChatBridge::connect(service, view)
```

### 8.3 `HistoryUpdated` 事件（展示校准，当前保持注释）

仅在破坏性操作（pop/clean/rollback/clear）后 emit。启用时用快照重建 `view` 的 `bubbles`，`active` 保留不动——正在 streaming 的气泡物理隔离在 `active`，天然受保护，无需再按 seq 做差集。

---

## 9. 交互卡片流程（request_user_action）

```
LLM 调用 request_user_action
  → 服务端 emit UIActionRequest { message, questions, session_id }
  → reduce: view.set_pending(PendingUI { tool_call_id, run_id, message, questions })
  → ChatPanel 在输入框上方渲染 ChatUIActionsView（并列 questions 问题卡）
  → 用户作答 on_user_action((ActionReply, PendingUI))  // Submit(文本 `header => answer`) 或 Cancel
  → bridge.confirm(reply, pending):
      1. fmt_reply(reply) 追加到 view（主 agent → 气泡；子 agent → agent_views）
      2. 主 agent 再 push 新 assistant 占位气泡
      3. run_id 非空 → resume_sub_agent；否则 confirm_user_action（action_id = submit / cancel）
```

---

## 10. 关键设计决策

| 决策 | 原因 |
|---|---|
| **单一订阅桥 `ChatBridge`** | UI 只依赖一个桥 + 一个 `Signal<ChatView>`，事件、发送、确认、停止都收口到桥，组件不直接碰 `ChatService` |
| **`ChatView` 单一值类型** | 把 6 个散落 signal 收敛为一个可整体读写的投影，配合纯 reducer 可脱离 dioxus 单测 |
| **单一数据源 `bubbles` + `active`** | 流式事件 O(当前 turn) 增量更新，历史完全不动，消除全量遍历 |
| **`Bubble` 扁平化** | 删除 `Message`/`ChatMessage`/`RenderMessage`/`RenderBubble` 多层，一次成型 |
| **`ToolViewData` 单一数据源** | 消除 name/arguments 与 phase/result 双源同步错位 |
| **`finish_turn` 在 `Done`/`Error` 时并入历史** | turn 原子性，`ToolExecuted` 迟到回填有明确边界 |
| **锁序约定（先改 view 再调 svc）** | `send`/`confirm` 先改 view、释放写锁再调 svc，避免与 svc 事件回调的 `view.write()` 嵌套锁 |
| 工具按 `tool_call_id` 精确路由 | 曾按 name/顺序匹配，多工具并发时参数/结果串台 |
| 每条 Assistant 独立气泡、text 全保留 | 曾把含 tool_calls 的 assistant 当纯工具消息，text 丢失 |
| 文本在工具面板之前渲染 | 与 ChatGPT/Claude/Cursor 一致 |
| **持久化上移服务端**（`ChatHistoryStore` trait） | 单一数据源——服务端 `History` 在每次写入/清理时同步 store，GUI 不再自行持久化 |
| **`Arc<ChatService>` 由桥持有** | 构造后服务指针稳定不变，桥即唯一持有者，组件无需关心 |
| **事件处理函数抽离出 rsx!** | rsx! 只负责布局，业务逻辑在组件体中定义为闭包/函数 |

---

## 11. 已知限制

- **同一 assistant 气泡内无法区分"工具前文本"与"工具后文本"**：文本统一放在工具面板之前（主流模型先说后调，可接受近似）
- **历史加载后工具错误态（is_error）无法还原**：`Message` 未持久化该字段，加载后统一显示为 Completed
- **`request_user_action` 的 Tool 消息在 GUI 侧缺失**：confirm 后服务端 `push_tool` 无事件驱动（由交互卡片替代展示）
- **`ToolExecuted` 迟到到 `Done` 之后**：`active` 已并入 `bubbles`，`find` 落空；服务端时序保证 `ToolExecuted` 先于 `Done`，正常不会发生
