# chat 模块渲染流程文档

> 覆盖 `crates/agent-gui/src/components/chat/` 全部实现：从服务端事件到最终 UI 的完整数据流。
> 阅读顺序建议：模块总览 → 数据模型 → 消息序列模型 → 事件消费 → 渲染管线 → Tool 生命周期 → 持久化 → 交互卡片。

---

## 1. 模块总览

```
chat/
├── mod.rs                    # 模块入口，pub use ChatPanel
├── chat_flow/                # 消息流转（数据层 + 业务编排）
│   ├── types.rs              #   纯数据类型：ChatMessage / ToolCallEntry / ToolViewData / PendingUI
│   ├── signals.rs            #   ChatSignals 信号容器（全部状态变更方法）
│   └── controller.rs         #   事件消费 / 发送消息 / 用户操作回调
├── chat_panel/               # 完整聊天面板（消息列表 + 输入区 + composer 工具栏）
│   ├── component.rs          #   ChatPanel 组件 + build_bubbles 渲染预计算
│   └── style.css
├── chat_ui_actions_view/     # Agent 交互卡片（Confirm / Select / Input / MultiSelect）
├── reasoning_view/           # Assistant「深度思考」折叠面板
└── tool_view/                # Tool 调用详情折叠卡片
```

| 模块 | 职责 | 关键导出 |
|---|---|---|
| `chat_flow` | 消息状态、事件消费 | `ChatSignals`、`send_message`、`ensure_subscription`、`handle_user_action` |
| `chat_panel` | 纯 UI 布局，不持有业务逻辑 | `ChatPanel`、`template_label` |
| `chat_ui_actions_view` | `request_user_action` 交互卡片 | `ChatUIActionsView` |
| `reasoning_view` | 推理内容折叠面板 | `ReasoningView` |
| `tool_view` | 工具调用折叠卡片 | `ToolView` |

**分层原则**：`chat_flow` 只操作 `Signal` 状态、不渲染；`chat_panel` 只渲染、不碰服务；页面层（如 `pages/plan/flexible/page.rs`）负责构造 `ChatService`（注入 `ChatMessageStore`）、从 `service.history()` 加载历史、把事件处理函数传入 `ChatPanel`。

**ChatService 直接传递**：不使用 Signal 包装——`Arc<ChatService>` 在组件体中构造，各事件闭包独立 clone。`require_resource::<T>()` 辅助函数（`context/mod.rs`）简化 Resource 读取。

---

## 2. 数据模型

### 2.1 `ChatSignals`（signals.rs）

所有聊天状态，`Copy` 可直接进闭包/异步块：

```rust
pub struct ChatSignals {
    pub messages: Signal<Vec<ChatMessage>, SyncStorage>,   // 消息序列（渲染的唯一输入）
    pub pending_ui: Signal<Option<PendingUI>, SyncStorage>, // 交互卡片
    pub input_text: Signal<String, SyncStorage>,            // composer 输入框
    pub pending_tool_call_id: Signal<Option<String>, SyncStorage>, // request_user_action 的 tool_call_id
    pub subscription: Signal<Option<SubscriptionGuard>, SyncStorage>, // 事件订阅 guard
}
```

**持久化由服务端 `History` + `ChatHistoryStore` 处理**——GUI 不再持有 `last_persisted_seq`、`ctx` 等存储字段，`ChatSignals` 是纯展示缓冲。

### 2.2 `ChatMessage`（types.rs）

GUI 层消息包装 = 底层 `Message` + UI 状态：

```rust
pub struct ChatMessage {
    pub message: Message,                  // role/content/reasoning_content/tool_calls/tool_call_id
    pub sequence_order: u64,               // 显示序号（稳定排序 key）
    pub is_streaming: bool,                // 是否正在流式接收
    pub tool_call_id: Option<String>,      // role=Tool 时的关联 id
    pub tool_call_entries: Vec<ToolCallEntry>, // UI 层工具状态（持久化时不存储）
}
```

### 2.3 工具相关：`ToolCallEntry`（轻量状态）与 `ToolViewData`（渲染数据）

**设计要点：单一数据源。** `name`/`arguments` 的权威在 `Message.tool_calls`
（持久化也依赖它），`tool_call_entries` 只存 UI 独有状态，二者通过 `tool_call_id`
一一关联，**不再双写**：

```rust
/// 存于 ChatMessage.tool_call_entries：纯 UI 状态
pub struct ToolCallEntry {
    pub tool_call_id: String,   // 关联键
    pub phase: ToolCallPhase,   // Pending / Running / Completed / Error
    pub result: Option<serde_json::Value>,
    pub is_error: bool,
}

/// 渲染时组合生成，ToolView 的输入
pub struct ToolViewData {
    pub tool_call_id: String,
    pub name: String,           // ← Message.tool_calls[].function.name
    pub arguments: String,      // ← Message.tool_calls[].function.arguments
    pub phase: ToolCallPhase,   // ← ToolCallEntry
    pub result: Option<serde_json::Value>, // ← ToolCallEntry
    pub is_error: bool,         // ← ToolCallEntry
}
```

### 2.4 `PendingUI`（交互卡片）

```rust
pub struct PendingUI {
    pub message: String,           // 引导文本
    pub actions: Vec<UIAction>,    // 可选动作（Confirm/Select/Input/MultiSelect）
    pub tool_call_id: String,      // confirm_user_action 回填用
    pub run_id: Option<String>,    // Some = 子 agent 挂起（resume 路径）；None = 主 agent
}
```

---

## 3. 消息序列模型（严格 OpenAI 四段式）

`ChatSignals.messages` 严格按 OpenAI 规范组织角色序列：

```
User → Assistant(tool_calls) → Tool → Assistant(text) → …
```

- **User**：用户输入（`send_message` 时 push）
- **Assistant(tool_calls)**：一条 assistant 消息**可同时**含 `content`（工具调用前说的话）
  与 `tool_calls`（OpenAI 规范允许，如"让我查一下" + `builtin_list_dir`）
- **Tool**：工具执行结果，由 `tool_call_executed` 追加到列表**末尾**
- **Assistant(text)**：下一轮 RoundStart 时 push 的占位消息，流式累积最终文本

> 顺序一致性保证：Tool 消息追加末尾，sequence_order 单调递增；渲染层按 `tool_call_id` 回填结果，**不依赖** Tool 消息位置。

---

## 4. 事件 → 状态消费（controller.rs `handle_event`）

服务端事件（`planned_agent::chat::ChatEvent`）→ `handle_event` → `ChatSignals` 方法：

| 服务端事件 | ChatSignals 动作 | 说明 |
|---|---|---|
| `Chat(TextDelta)` | `append_streaming` | 追加到最新 streaming assistant（`rfind`） |
| `Chat(ReasoningDelta)` | `append_streaming_reasoning` | 同上，追加 reasoning |
| `Chat(RoundStart)` | 若未在流式则 `push_assistant_placeholder` | 防御性（send_message 已预建占位） |
| `Chat(ToolCallStart)` | `tool_call_start` | 建 Pending entry + `message.tool_calls` 条目 |
| `Chat(ToolCallArgsDelta)` | `tool_call_append_args` | 只更新 `message.tool_calls[].arguments` |
| `Chat(ToolCallComplete)` | `tool_call_complete` | entry → Running；arguments 覆写完整 JSON |
| `Chat(ToolExecuted)` | `tool_call_executed` | entry → Completed/Error + result；追加 Tool 消息 |
| `Chat(RoundEnd)` | `stop_streaming` | 轮次结束（持久化已由服务端自动处理） |
| `Chat(UIActionRequest)` | `set_pending` | 弹出交互卡片 |
| `Done` | `stop_streaming` + `clear_pending` | 收尾 |
| `Error(e)` | `stop_streaming` + 追加错误文本 | 收尾 |
| **`HistoryUpdated`** | `reconcile_with_snapshot` | 用快照校准 messages（保护 streaming 状态） |

**发送链路**（`send_message`）：`clear_pending` → `push_user_turn`（push User + 预建 streaming Assistant 占位）→ `chat_svc.send_text(text)`。持久化由服务端 `History.push_user` 在内部自动完成。

---

## 5. 渲染管线（chat_panel/component.rs）

### 5.1 总览

```
ChatSignals.messages (Vec<ChatMessage>)
        │
        ▼
build_bubbles(&[ChatMessage]) → Vec<RenderBubble>
        │  （纯函数预计算，rsx 只负责布局）
        ▼
RenderBubble { is_assistant, messages: Vec<RenderMessage>, is_streaming }
RenderMessage { text, reasoning, is_streaming, tool_calls: Vec<ToolViewData> }
        │
        ▼
render_assistant_message / render_user_bubble
        │
        ▼
ReasoningView + Markdown(text) + ToolView × N + 光标
```

### 5.2 `build_bubbles` 规则

遍历消息，**每条消息一个气泡**：

| 消息角色 | 行为 |
|---|---|
| `User` | 独立 user 气泡 |
| `Assistant`（无论有无 tool_calls） | 独立 assistant 气泡：reasoning + text + ToolView 全保留 |
| `Tool` | 不产生气泡；按 `tool_call_id` 找到对应 `ToolViewData` 回填 `result` |
| 其他（System 等） | 忽略 |

维护 `tool_index: HashMap<tool_call_id, (bubble_idx, msg_idx, entry_idx)>`，Tool 消息精确回填。

### 5.3 `resolve_tool_entries`（组合 ToolViewData）

`name`/`arguments` 遍历 `Message.tool_calls` 取得；`phase`/`result`/`is_error`
按 `tool_call_id` 从 `tool_call_entries` 取，无对应 entry 视为 `Completed`
（覆盖历史加载后无实时状态的场景）。

### 5.4 单条 assistant 消息渲染顺序（与 ChatGPT/Claude 一致）

```
1. ReasoningView（如有推理内容，流式时脉冲动画）
2. Markdown(text)（工具调用前说的话，显示在工具面板之前）
3. ToolView × N（工具面板紧随其说明文本）
4. 光标 "▍"（仅当 streaming 且 text/reasoning/tool_calls 全空）
```

> 关键：**文本在工具面板之前**。assistant 通常先说一段话再发起工具调用。

### 5.5 输入区 + Composer 工具栏

- `Textarea`：Enter 发送（Shift+Enter 换行），busy 时禁用
- 工具栏：模板选择、思考模式、温度选择
- 发送/停止按钮：busy → 停止（`svc.stop()`）；空闲 → 发送

---

## 6. Tool 调用生命周期（完整时序）

以"列目录 → 读文件"为例：

```
RoundStart
  └─ send_message 已预建 Assistant 占位（is_streaming=true）
TextDelta:  "目录下共发现 3 个文件：…"      → append_streaming
ToolCallStart(id="call_x", name="builtin_read_file")
  ├─ ToolCallEntry { id, Pending }          → tool_call_start
  └─ Message.tool_calls += { id, name, args:"" }
ToolCallArgsDelta(id, "{\"path\":\"…")      → 只更新 Message.tool_calls.arguments
ToolCallComplete(id, args=完整JSON)          → entry→Running；arguments 覆写 pretty JSON
RoundEnd                                     → stop_streaming（持久化由服务端自动处理）
ToolExecuted(id, content, is_error)          → entry→Completed/Error + result；
                                              追加 Tool 消息（末尾，seq=max+1）
RoundStart (下一轮)                          → 新 Assistant 占位
TextDelta:  ".env 内容如下：…"              → 最终文本
RoundEnd                                     → stop_streaming
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

### 异常与兜底

- **ArgsDelta 先于 ToolCallStart**：首段 delta 被丢弃；`ToolCallComplete` 完整参数覆写，数据不丢
- **Complete/Executed 时 entry 缺失**：按 id 判断不存在才补建，同时补 `message.tool_calls`
- **`request_user_action`（UI 工具）**：GUI 刻意跳过其 ToolCallStart/Complete，由交互卡片替代

---

## 7. 持久化流程（服务端 ChatHistoryStore）

### 架构

```
GUI page.rs
  │
  ├─ 创建 ChatMessageStore { plan_id, repo: Arc<ChatMessageRepo> }
  │     （实现服务端 ChatHistoryStore trait，内部 tokio::spawn 异步写 SQLite）
  │
  └─ 注入 ChatService::new(..., Some(Arc::new(store)))
        │
        └─ History::new(store)
              │
              ├─ store.load() → 从 SQLite 恢复 Message 列表 → 填入 History.inner
              │
              └─ 每次写入/清理时同步 store：
                    push_user/assistant/tool → store.append()
                    rollback_to / pop_last / clean_unclosed → store.rollback_to()
                    clear → store.clear()
```

### trait 定义（服务端 `crates/planned-agent/src/chat/storage.rs`）

```rust
pub trait ChatHistoryStore: Send + Sync {
    fn load(&self) -> Vec<Message>;       // 恢复历史（构造时调用一次）
    fn append(&self, msg: &Message);      // 追加一条消息
    fn rollback_to(&self, len: usize);    // 回滚到指定长度
    fn clear(&self);                      // 清空会话
}
```

默认实现 `InMemoryStore`（不持久化），用于子 agent 临时会话和测试。

### GUI 侧历史加载

```
page.rs:
  1. 构造 ChatMessageStore(plan_id, repo)
  2. 注入 ChatService → 服务端 History 自动 load()
  3. service.history() 获取已恢复的 Vec<Message>
  4. chat.load_from_history(&history) → 重建 ChatMessage 列表
  5. build_bubbles 渲染（resolve_tool_entries 自动组合 tool_call_entries）
```

### 子 agent 不持久化

子 agent 构造 `ChatService` 时传 `None`（默认 `InMemoryStore`），天然不落库。

---

## 8. HistoryUpdated 事件（展示校准）

### 触发时机

仅在**破坏性操作**（删除/回滚/清理）后 emit——append 类操作 GUI 已通过现有事件链知道：

| 服务端操作 | 触发位置 |
|---|---|
| `pop_last_assistant_tool_calls_if_not_first` | `round.rs`（max_tool_rounds） |
| `clean_unclosed_assistant_tool_calls` | `round.rs`（工具取消 + 用户取消）× 2 |
| `rollback_to` | `driver/mod.rs`（对话失败回滚） |
| `clear` | `driver/mod.rs`（Command::Reset） |

### GUI 校准策略（`ChatSignals::reconcile_with_snapshot`）

```
收到 HistoryUpdated { messages }:
  1. 收集当前正在 streaming 的消息（保留，比快照更新）
  2. 按 sequence_order 构建快照 → 新的 ChatMessage 列表
  3. 把 streaming 消息按 seq 替换回新列表
  4. 替换 ChatSignals.messages
```

---

## 9. 交互卡片流程（request_user_action）

```
LLM 调用 request_user_action
  → 服务端 emit UIActionRequest { message, actions, session_id }
  → handle_event: set_pending(PendingUI { tool_call_id, run_id })
  → ChatPanel 末尾渲染 ChatUIActionsView
  → 用户操作 on_action((UIAction, choice))
  → handle_user_action:
      1. choice 文本追加到最后一个 assistant 消息
      2. push 新 Assistant 占位
      3. run_id 非空 → resume_sub_agent；否则 confirm_user_action
```

---

## 10. 关键设计决策（历史修复沉淀）

| 决策 | 原因 |
|---|---|
| 工具按 `tool_call_id` 精确路由 | 曾按 name/顺序匹配，多工具并发时参数/结果串台 |
| `name`/`arguments` 单一数据源（`Message.tool_calls`） | 消除双写同步错位 |
| Tool 消息追加末尾而非插中 | 保证 seq 与列表顺序一致 |
| 每条 Assistant 独立气泡、text 全保留 | 曾把含 tool_calls 的 assistant 当纯工具消息，text 丢失 |
| 文本在工具面板之前渲染 | 与 ChatGPT/Claude/Cursor 一致 |
| **持久化上移服务端**（`ChatHistoryStore` trait） | 单一数据源——服务端 `History` 在每次写入/清理时同步 store，GUI 不再自行持久化 |
| **`HistoryUpdated` 事件** | 破坏性操作（pop/clean/rollback/clear）不产生其他事件，GUI 无法感知；用快照校准 |
| 子 agent 默认 `InMemoryStore` | 临时会话不持久化，构造时传 `None` |
| `ToolViewData` 与 `ToolCallEntry` 分离 | 渲染结构 vs UI 状态职责分离 |
| **`Arc<ChatService>` 直接传递**（不用 Signal） | 构造后服务指针稳定不变，Signal 追踪"一次初始化"是多余开销；`require_resource` 辅助函数简化 Resource 读取 |
| **事件处理函数抽离出 rsx!** | rsx! 只负责布局，业务逻辑在组件体中定义为闭包/函数，可读性和可测试性更好 |

---

## 11. 已知限制

- **同一 assistant 消息内无法区分"工具前文本"与"工具后文本"**：`Message.content`
  是整轮拼接的单字符串，渲染统一放在工具面板之前（主流模型先说后调，可接受近似）
- **历史加载后工具错误态（is_error）无法还原**：`Message` 未持久化该字段，加载后
  统一显示为 Completed
- **`request_user_action` 的 Tool 消息在 GUI 侧缺失**：confirm 后服务端 `push_tool`
  无事件驱动（由交互卡片替代展示）
- **`ChatMessageStore::append` 的 sequence_order**：从 DB 当前最大 seq + 1 推导，
  并发 append 时理论上有 seq 冲突（幂等保护兜底，不影响功能）
