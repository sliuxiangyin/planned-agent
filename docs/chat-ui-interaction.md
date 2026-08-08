# Chat UI 交互系统设计

基于 ToolCall 机制实现 Chat 助手与前端 UI 的双向交互：Agent 通过 tool call 请求用户确认/选择，前端渲染交互组件（按钮、选项列表），用户操作后以 tool result 形式回流，继续对话。

## 概述

### 动机

当前 `plan.rs` 中的 Chat 页面只支持纯文本对话——LLM 输出文本，前端用 Markdown 渲染。当用户输入的信息不足以生成计划时，LLM 只能在文本中引导，无法提供结构化的交互（确认按钮、选项列表等）；用户也无法通过点击按钮快捷响应。

### 设计目标

1. **LLM 可驱动 UI**：Chat 助手在需要用户决策时，返回结构化交互指令
2. **前端渲染交互组件**：确认按钮、单选/多选列表、文本输入引导
3. **用户操作回流**：点击/选择后结果以 tool result 形式回传给 LLM，继续对话
4. **零协议创新**：完全复用 OpenAI Function Calling 标准，与现有 `ChatService` 的 tool-call 循环无缝集成

### 业内参考

该设计与 Vercel AI SDK、CopilotKit（AG-UI 协议）、LangChain 等主流框架的 Human-in-the-Loop 模式一致——核心思路是将"请求用户交互"建模为一个 tool call，前端拦截并渲染 UI，用户操作结果以 tool result 形式回流。

---

## 架构

### 端到端数据流

```
用户输入: "帮我分析 /var/log/nginx/access.log 中的 404 错误"
    │
    ▼
ChatService.chat_with_callback(history, on_event)
    │
    ├─► LLM 生成文本: "好的，我来确认一下..."
    │       │  ChatEvent::TextDelta(chunk) → 前端流式渲染
    │
    ├─► LLM 调用 tool: request_user_action {
    │       message: "你的需求已清晰，需要生成执行计划吗？",
    │       actions: [
    │         { id: "gen", type: "confirm", label: "生成计划" },
    │         { id: "edit", type: "confirm", label: "我再补充" }
    │       ]
    │   }
    │       │  ChatEvent::ToolCallStart / ToolCallArgsDelta / ToolCallComplete
    │       │
    │       ▼  🆕 拦截点：检测到 request_user_action
    │       │  ChatEvent::UIActionRequest { message, actions }
    │       │  推送占位 tool result → break 循环
    │       │  ChatResponse { pending_ui_actions: [...] }
    │
    ▼
前端渲染:
┌───────────────────────────────────────────┐
│ 你的需求已清晰，需要生成执行计划吗？        │
│                                           │
│  [ 生成计划 ]    [ 我再补充 ]              │
└───────────────────────────────────────────┘
    │
    │  用户点击 "生成计划"
    ▼
handle_user_action()
    │  用真实用户选择替换占位 tool result
    │  追加 assistant 占位消息
    │
    ▼
ChatService.chat_with_callback(新 history)
    │  history: [..., assistant(tool_calls=[request_user_action]),
    │                   tool(result={"action_id":"gen","choice":"生成计划"})]
    │
    ├─► LLM 看到 tool result → 继续回复
    │       "好的，计划正在生成中..."
    │
    ▼
ChatResponse { pending_ui_actions: [] }  // 无待处理 UI → 正常展示
```

### 多轮渐进式对话与"生成计划"触发

用户的需求往往**不会一次性说完**——可能在多轮对话中逐步补充信息，最后才说"生成计划"。系统必须**累积理解全部对话历史**来判断完整性。

#### 渐进式需求构建流程

```
第1轮: 用户: "我想分析日志"
        助手: "好的，请告诉我日志文件的路径？"
              actions: [{id:"input_path", type:"input", label:"输入文件路径"}]

第2轮: 用户: "/var/log/nginx/access.log"
        助手: "明白了，需要做什么分析呢？"
              actions: [{id:"404", type:"select", label:"分析404错误"},
                       {id:"perf", type:"select", label:"分析性能"},
                       {id:"custom", type:"input", label:"自定义分析"}]

第3轮: 用户点击 "分析404错误"
        助手: "好的，还需要指定其他条件吗？比如时间范围？"
              （或信息已完整时：）
              actions: [{id:"gen", type:"confirm", label:"生成计划"},
                       {id:"more", type:"confirm", label:"继续补充"}]

第4轮: 用户: "生成计划"  ← 显式触发
        助手: 【回顾全部历史】
              "根据之前的对话，我理解你的需求是：
              分析 /var/log/nginx/access.log 中的 404 错误。
              确认无误？"
              actions: [{id:"gen", type:"confirm", label:"创建计划"},
                       {id:"edit", type:"confirm", label:"修改"}]
```

#### "生成计划"触发词处理

当用户**显式说出**触发词（"生成计划"、"创建计划"、"可以了"、"开始吧"等）时，助手的行为应该是：

1. **回顾全部历史消息**：从对话开始到当前的所有用户消息中提取需求信息
2. **评估完整性**：基于累积的全部信息判断是否足够创建计划
3. **完整性足够** → 总结需求 + 确认按钮
4. **完整性不足** → 列出已获取的信息 + 指出仍缺失的部分 + 引导补充

```
用户: "生成计划"
  ↓
助手分析全部历史:
  已获取: 目标=分析日志, 路径=/var/log/nginx/access.log
  缺失: 分析类型（404? 性能? 关键词?）, 输出格式
  ↓
助手回复:
  "根据目前的对话，我已了解：
   ✓ 目标：分析日志文件
   ✓ 路径：/var/log/nginx/access.log
   ✗ 分析类型：尚未指定
   ✗ 输出格式：尚未指定

   请补充以上信息，或选择："
  actions: [
    {id:"404", type:"select", label:"分析404错误"},
    {id:"perf",type:"select", label:"分析性能"},
    {id:"text_report",type:"confirm", label:"先按默认生成"},
  ]
```

#### 与单轮的对比

| | 单轮一次性 | 多轮渐进式 |
|---|---|---|
| 信息来源 | 仅当前消息 | **全部历史用户消息** |
| 完整性判断时机 | 每条消息后 | 每条消息后 + 显式触发时 |
| 触发方式 | LLM 自动判断 | LLM 自动判断 + 用户说"生成计划" |
| 对话节奏 | 一问一答快速确认 | 逐步构建，最后确认 |

#### 对 System Prompt 的关键要求

1. **"回顾全部历史"** 必须作为强约束写入 system prompt
2. **触发词识别**：明确列出"生成计划"、"创建计划"等词应触发完整性检查
3. **需求总结**：确认生成前必须先总结已理解的全部需求，让用户确认

### 关键设计决策

| 决策 | 选择 | 理由 |
|------|------|------|
| UI 交互通道 | 复用 Function Calling | LLM 原生理解 tool call 语义，零协议创新 |
| 拦截方式 | 在 tool 执行循环前按 name 过滤 | 最小侵入，不改动 tool 执行器逻辑 |
| 循环中断 | 检测到 UI tool 后 break | 等待用户操作后再继续，避免 LLM 看到占位符乱猜 |
| 状态传递 | `ChatEvent::UIActionRequest` + `ChatResponse.pending_ui_actions` | 事件用于流式通知，response 用于调用方判断 |
| 占位 tool result | `{"status":"awaiting_user_input"}` | 保持 assistant→tool 消息顺序，前端后续替换 |

---

## 数据类型

### `UIAction` 与 `UIActionType`（`planned-agent/src/chat/ui_action.rs`）

```rust
/// UI 交互动作 —— Agent 通过 tool call 请求前端渲染交互组件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UIAction {
    /// 动作唯一标识（如 "generate_plan", "add_more_detail"）
    pub id: String,
    /// 动作类型
    #[serde(rename = "type")]
    pub action_type: UIActionType,
    /// 展示文本（按钮文字、选项标签）
    pub label: String,
    /// 补充说明（可选 tooltip）
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UIActionType {
    /// 确认按钮：用于"是/否"或多选一场景
    Confirm,
    /// 单选列表：从多个选项中选一个
    Select,
    /// 文本输入提示：引导用户输入具体信息（如文件路径、关键词）
    Input,
}
```

### `ChatEvent::UIActionRequest`（`chat/event.rs` 新增）

```rust
/// Agent 请求用户交互。前端应渲染对应 UI 组件（按钮/选项列表等）。
///
/// 触发时机：tool_calls 中检测到 `request_user_action` 时发出。
/// 此后 chat 循环中断，调用方需收集用户选择后重新调用 `chat_with_callback`。
UIActionRequest {
    /// 展示给用户的引导文本
    message: String,
    /// 用户可执行的动作列表
    actions: Vec<UIAction>,
},
```

### `PendingUIAction` 与 `ChatResponse` 扩展（`chat/service.rs`）

```rust
/// 待处理的 UI 交互请求
#[derive(Debug, Clone)]
pub struct PendingUIAction {
    pub message: String,
    pub actions: Vec<UIAction>,
}

pub struct ChatResponse {
    pub message: Message,
    pub history: Vec<Message>,
    pub tool_calls_executed: usize,
    pub finish_reason: Option<FinishReason>,
    // 🆕 待处理的 UI 交互。非空 = 需要用户操作后才能继续对话
    pub pending_ui_actions: Vec<PendingUIAction>,
}
```

---

## ChatService 改动（`chat/service.rs`）

### 拦截位置

在 `chat_with_callback` 中，现有 tool 执行循环（约 L313-350）之前插入 UI tool 检测与拦截：

```rust
// ── 分离 UI 工具和普通工具 ──
const UI_TOOL_NAMES: &[&str] = &["request_user_action"];

let (ui_calls, backend_calls): (Vec<_>, Vec<_>) = tool_calls_vec
    .iter()
    .partition(|tc| UI_TOOL_NAMES.contains(&tc.function.name.as_str()));

// 先执行普通后端工具（现有逻辑不变）
for call in backend_calls {
    let args: Value = serde_json::from_str(&call.function.arguments)
        .unwrap_or_else(|_| Value::String(call.function.arguments.clone()));

    let outcome = self.tool_registry.call_tool(&call.function.name, args).await;
    // ... 现有处理逻辑 ...
}

// 🆕 处理 UI 工具
let mut pending_ui_actions = Vec::new();

for call in ui_calls {
    let args: Value = serde_json::from_str(&call.function.arguments)
        .unwrap_or_default();

    let message = args["message"].as_str().unwrap_or("").to_string();
    let actions: Vec<UIAction> = serde_json::from_value(
        args.get("actions").cloned().unwrap_or(Value::Array(vec![]))
    ).unwrap_or_default();

    // 1. 通知前端
    on_event(ChatEvent::UIActionRequest {
        message: message.clone(),
        actions: actions.clone(),
    });

    // 2. 推送占位 tool result（保持 assistant→tool 消息顺序，后续前端替换）
    history.push(Message {
        role: MessageRole::Tool,
        content: Some(MessageContent::ToolResult {
            tool_call_id: call.id.clone(),
            content: r#"{"status":"awaiting_user_input"}"#.to_string(),
        }),
        tool_call_id: Some(call.id.clone()),
        tool_calls: None,
        name: None,
        reasoning_content: None,
    });

    pending_ui_actions.push(PendingUIAction { message, actions });
}

// 有 UI action → 中断循环（等用户操作后前端重新调用）
if !ui_calls.is_empty() {
    break;
}
```

### 返回值构造

```rust
Ok(ChatResponse {
    message,
    history,
    tool_calls_executed,
    finish_reason,
    pending_ui_actions,  // 🆕
})
```

### 逐出与导出（`chat/mod.rs`）

```rust
pub use event::ChatEvent;
pub use service::{ChatResponse, ChatService, PendingUIAction};  // 新增 PendingUIAction
pub use config::ChatConfig;
```

---

## `request_user_action` Tool 定义

### JSON Schema

向 `ToolRegistry` 注册的 Tool Schema：

```json
{
  "name": "request_user_action",
  "description": "当需要用户确认、选择或补充信息时调用。用于引导用户完善需求、确认计划生成等交互场景。调用后等待用户操作，不要自行假设用户选择。",
  "input_schema": {
    "type": "object",
    "properties": {
      "message": {
        "type": "string",
        "description": "展示给用户的引导文本，应清晰说明需要用户做什么决定"
      },
      "actions": {
        "type": "array",
        "description": "用户可选的动作列表（按钮/选项）",
        "items": {
          "type": "object",
          "properties": {
            "id": {
              "type": "string",
              "description": "动作唯一标识"
            },
            "type": {
              "type": "string",
              "enum": ["confirm", "select", "input"],
              "description": "动作类型：confirm=确认按钮, select=单选列表, input=文本输入提示"
            },
            "label": {
              "type": "string",
              "description": "展示文本"
            },
            "description": {
              "type": "string",
              "description": "补充说明，可选"
            }
          },
          "required": ["id", "type", "label"]
        }
      }
    },
    "required": ["message", "actions"]
  }
}
```

### 注册方式

在 agent-gui 初始化时（`main.rs` 或 `plan.rs` 组件初始化），作为自定义工具注册到 `ToolRegistry`：

```rust
let ui_action_tool = Tool {
    name: "request_user_action".into(),
    description: "请求用户确认、选择或补充信息。当需要用户决策时调用。".into(),
    input_schema: serde_json::json!({
        "type": "object",
        "properties": {
            "message": {
                "type": "string",
                "description": "展示给用户的引导文本"
            },
            "actions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "type": { "type": "string", "enum": ["confirm", "select", "input"] },
                        "label": { "type": "string" },
                        "description": { "type": "string" }
                    },
                    "required": ["id", "type", "label"]
                }
            }
        },
        "required": ["message", "actions"]
    }),
};

// 使用 no-op executor（前端拦截，永不实际执行）
struct NoopExecutor;
impl ToolExecutor for NoopExecutor {
    async fn execute(&self, _name: &str, _args: Value) -> Result<ToolResult> {
        Ok(ToolResult {
            content: Value::String("ok".into()),
            is_error: false,
        })
    }
}

tool_registry.register_custom_tool(
    ui_action_tool,
    vec![ToolCategory::Utility],
    Arc::new(NoopExecutor),
);
```

---

## System Prompt 设计（`agent-gui/prompts/thorough/thorough_system.toml`）

```toml
[name]
description = "计划顾问助手 —— 引导用户完善需求并生成执行计划"

[content]
text = """
你是一个智能计划顾问助手。你的核心职责是帮助用户理清需求，引导用户提供足够完整的信息。

## 你的角色
你是用户和计划生成系统之间的桥梁。你**不直接生成计划**，而是帮助用户将模糊的想法转化为清晰、可执行的需求描述。

## 行为规则

### 1. 分析用户输入（跨轮累积）
每次收到用户消息后，**不要只看当前消息**，必须结合对话中**全部历史用户消息**来判断完整性：
- **目标明确**：用户想做什么
- **实体完备**：URL、路径、关键词、数值等关键信息没有缺失或模糊
- **范围清晰**：任务的边界和预期结果可推断

用户的需求往往是分多次消息逐步补充的（如先说"分析日志"、再说路径、再说分析类型）。你需要持续追踪已收集的信息，在每轮对话中更新你的理解。

### 2. 四种响应模式

**模式 A：信息完整 → 确认生成**
当**全部历史消息累积**的信息足够清晰时，调用 `request_user_action` 工具引导确认。确认时必须**先总结你理解的全部需求**。

示例：
- message = "根据之前的对话，我理解你的需求是：分析 /var/log/nginx/access.log 中的 404 错误。确认无误吗？"
- actions = [
    { id: "generate", type: "confirm", label: "创建计划" },
    { id: "edit", type: "confirm", label: "我再补充一下" }
  ]

**模式 B：信息不完整 → 引导补充**
当缺少关键信息时，调用 `request_user_action` 提供选项引导补充。引导时应先说明已了解的部分，再指出缺失部分。

示例：
- message = "我已了解：你想分析日志文件。还需要确认："
- actions = [
    { id: "input_path", type: "input", label: "输入文件路径", description: "如 /var/log/app.log" },
    { id: "select_404", type: "select", label: "分析 404 错误", description: "统计 404 错误的频率和来源" },
    { id: "select_perf", type: "select", label: "分析性能", description: "分析响应时间和慢请求" }
  ]

**模式 C：用户显式触发"生成计划" → 回顾全部历史后响应**
当用户说"生成计划"、"创建计划"、"可以了"、"开始吧"、"帮我生成"等触发词时，你必须：

1. **回顾全部历史用户消息**，提取所有已提及的需求信息
2. **评估完整性**（基于累积的全部信息）
3. 如果完整 → 总结需求 + 确认按钮（模式 A）
4. 如果不完整 → 列出"已获取 ✓"和"仍缺失 ✗"的清单 + 引导补充（模式 B）

注意：不要因为用户说了"生成计划"就强行确认——信息不够时必须诚实告知。

示例（不完整时）：
- message = "根据目前的对话，我已了解：\n✓ 目标：分析日志文件\n✓ 路径：/var/log/nginx/access.log\n✗ 分析类型：尚未指定\n✗ 输出格式：尚未指定\n\n请补充以上信息："
- actions = [
    { id: "404", type: "select", label: "分析 404 错误" },
    { id: "perf", type: "select", label: "分析性能" },
    { id: "default", type: "confirm", label: "先按默认生成" }
  ]

**模式 D：闲聊或问候 → 普通回复**
如果用户只是闲聊（问候、询问功能、随便聊聊等），正常友好回复，不需要调用工具。

### 3. 不要做的事情
- 不要替用户做决定（如猜测文件路径、默认选项）
- 不要在信息不完整时直接说"可以生成计划"
- 不要在单次回复中多次调用 request_user_action
- 不要编造用户未提及的实体（URL、路径、关键词等）
- **不要只基于最后一条消息做判断——必须综合全部历史**

### 4. 对话风格
- 友好、耐心、引导式提问
- 使用中文回复
- 回顾用户已提供的信息，只就缺失部分提问
- 用选择题而非填空题引导用户，降低认知负担

{{ context }}
"""

[variables]
context = { description = "计划上下文信息（可选）", required = false }
```

---

## 前端改动（`agent-gui/src/pages/plan.rs`）

### 新增状态

```rust
/// 待处理的 UI 交互状态
#[derive(Clone)]
struct PendingUIState {
    message: String,
    actions: Vec<UIAction>,
    /// 当时的对话历史快照（用于用户操作后继续 chat）
    history_snapshot: Vec<Message>,
}

// 在 PlanPage 组件中新增 signal
let pending_ui = use_signal_sync(|| None::<PendingUIState>);
```

### `send_message` 中捕获 `UIActionRequest`

在 `on_event` 回调中新增分支：

```rust
ChatEvent::UIActionRequest { message, actions } => {
    *pending_ui.write() = Some(PendingUIState {
        message,
        actions,
        history_snapshot: messages.read().clone(),
    });
}
```

### `send_message` 返回值处理

```rust
match result {
    Ok(response) => {
        if !response.pending_ui_actions.is_empty() {
            // UI actions 已通过 event 设置到 pending_ui signal
            // 不清除 streaming——按钮会在消息区下方渲染
            streaming_idx.set(None);
        } else {
            finalize_assistant(messages, streaming_idx, &display_text(&response.message));
        }
    }
    Err(e) => {
        finalize_assistant(messages, streaming_idx, &format!("聊天失败: {}", e));
    }
}
```

### `handle_user_action` 函数

用户点击按钮后的完整处理流程：

```rust
fn handle_user_action(
    action: UIAction,
    pending: PendingUIState,
    mut messages: Signal<Vec<Message>, SyncStorage>,
    mut streaming_idx: Signal<Option<usize>, SyncStorage>,
    mut pending_ui: Signal<Option<PendingUIState>, SyncStorage>,
    chat_signal: ChatServiceSignal,
) {
    // 1. 获取历史快照
    let mut history = pending.history_snapshot;

    // 2. 替换占位 tool result 为真实的用户选择
    for msg in history.iter_mut().rev() {
        if let Some(MessageContent::ToolResult { content, .. }) = &mut msg.content {
            if content.contains("awaiting_user_input") {
                *content = serde_json::to_string(&serde_json::json!({
                    "action_id": action.id,
                    "action_type": action.action_type,
                    "choice": action.label,
                })).unwrap_or_default();
                break;
            }
        }
    }

    // 3. 更新 UI messages（展示用户选择）
    messages.set(history.clone());

    // 4. 清除 pending 状态
    pending_ui.set(None);

    // 5. 添加新的 assistant 占位（用于流式输出）
    let asst_idx;
    {
        let mut msgs = messages.write();
        asst_idx = msgs.len();
        msgs.push(Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text { text: String::new() }),
            ..Default::default()
        });
    }
    streaming_idx.set(Some(asst_idx));

    // 6. 继续聊天
    let chat = (*chat_signal.read()).clone();
    let Some(chat) = chat else { return; };

    spawn(async move {
        let result = chat.chat_with_callback(history, |event| match event {
            ChatEvent::TextDelta(chunk) => {
                if let Some(idx) = *streaming_idx.read() {
                    if let Some(msg) = messages.write().get_mut(idx) {
                        if let Some(t) = display_text_mut(msg) {
                            t.push_str(&chunk);
                        }
                    }
                }
            }
            ChatEvent::UIActionRequest { message, actions } => {
                *pending_ui.write() = Some(PendingUIState {
                    message,
                    actions,
                    history_snapshot: messages.read().clone(),
                });
            }
            _ => {}
        }).await;

        match result {
            Ok(response) => {
                if response.pending_ui_actions.is_empty() {
                    finalize_assistant(messages, streaming_idx, &display_text(&response.message));
                } else {
                    streaming_idx.set(None);
                }
            }
            Err(e) => {
                finalize_assistant(messages, streaming_idx, &format!("出错: {}", e));
            }
        }
    });
}
```

### UI 渲染（rsx! 中）

在消息列表与输入区之间，渲染 pending 的 UI 动作：

```rust
{
    let ui = pending_ui.read();
    if let Some(ref pending) = *ui {
        rsx! {
            div { class: "chat-ui-actions",
                p { class: "chat-ui-actions__message", "{pending.message}" }
                div { class: "chat-ui-actions__buttons",
                    for action in &pending.actions {
                        {
                            let action = action.clone();
                            // PendingUIState 需实现 Clone
                            let p = pending.clone();
                            rsx! {
                                button {
                                    class: "chat-ui-action-btn chat-ui-action-btn--{action.action_type:?}",
                                    onclick: move |_| {
                                        handle_user_action(
                                            action.clone(),
                                            p.clone(),
                                            messages,
                                            streaming_idx,
                                            pending_ui,
                                            chat_signal,
                                        );
                                    },
                                    title: action.description.clone().unwrap_or_default(),
                                    "{action.label}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
```

---

## 空状态与边界处理

### 首轮对话

用户首次进入 Chat 页面，Assistant 发送欢迎消息，不需要 `request_user_action`。当前 `plan.rs` 已有欢迎消息：

```rust
vec![Message {
    role: MessageRole::Assistant,
    content: Some(MessageContent::Text {
        text: "欢迎来到 Plan 页面！".into(),
    }),
    ..Default::default()
}]
```

可后续优化为引导式开场白，但不需要交互按钮。

### 用户输入"生成计划"等触发词

用户可以直接在输入框中输入"生成计划"，此时 `send_message` 会将其作为普通 User Message 发送。LLM 收到后按 System Prompt 中的**模式 C**响应：

1. 清除当前的 `pending_ui`（因为用户选择了直接输入文本）
2. 发送 `"生成计划"` 作为新的 User Message
3. LLM 回顾全部历史，评估完整性后决定：
   - 完整 → 调用 `request_user_action`（总结确认）
   - 不完整 → 调用 `request_user_action`（列出缺失 + 引导）

注意：触发词是**自然语言**，由 LLM 在上下文理解中识别，不需要前端做特殊判断。System Prompt 中已明确列出触发词列表。

### 连续多轮 UI Action

`handle_user_action` 中调用 `chat_with_callback` 时同样注册了 `UIActionRequest` 的捕获，因此支持 LLM 在多轮对话中多次请求用户交互。每次只渲染一组按钮，新请求会覆盖旧状态。

### 用户直接输入文本（跳过/覆盖按钮）

用户可以不点击按钮而直接在输入框输入新文本。此时应：
1. 清除 `pending_ui` 状态（`pending_ui.set(None)`）
2. 将当前文本作为新的 User Message 发送
3. 现有的 tool call + 占位 tool result 仍在 history 中，LLM 会在完整上下文中理解用户的意图

在 `send_message` 开头增加：

```rust
// 清除未响应的 UI action（用户选择了直接输入）
pending_ui.set(None);
```

### 需求变更（用户推翻之前的说法）

用户可能在多轮对话中改变想法，例如第3轮说"不要分析日志了，改成分析 CPU 使用率"。此时：
1. 前端正常发送 User Message
2. LLM 在 System Prompt 约束下**综合全部历史**理解——会识别到新消息推翻了之前的"日志"需求
3. LLM 更新累积理解（以最新信息为准），重新评估完整性
4. 必要时重新调用 `request_user_action` 引导补充新需求的信息

---

## 改动文件清单

| 文件 | 改动内容 | 复杂度 |
|------|----------|--------|
| `crates/core/src/types.rs` | 新增 `UIAction`、`UIActionType` 类型 | 低 |
| `crates/planned-agent/src/chat/event.rs` | 新增 `ChatEvent::UIActionRequest` 变体 | 低 |
| `crates/planned-agent/src/chat/service.rs` | UI tool 拦截逻辑 + `pending_ui_actions` 字段 + `PendingUIAction` 类型 | 中 |
| `crates/planned-agent/src/chat/mod.rs` | 导出 `PendingUIAction` | 低 |
| `crates/agent-gui/prompts/thorough/thorough_system.toml` | System prompt 重写，定义角色与行为规则 | 中 |
| `crates/agent-gui/src/pages/plan.rs` | 新增 `pending_ui` signal、`UIActionRequest` 事件处理、`handle_user_action` 函数、UI 渲染 | 高 |
| `crates/agent-gui/src/main.rs`（或初始化处） | 注册 `request_user_action` 自定义工具 | 低 |
| `crates/agent-gui/src/services/chat_service/` | 导出新类型到前端使用 | 低 |

---

## 测试要点

1. **单轮完整场景**：用户一次性输入足够具体的需求 → LLM 调用 `request_user_action` → 按钮渲染 → 点击"生成计划" → 继续对话
2. **单轮不完整场景**：用户输入模糊需求 → LLM 调用 `request_user_action` 带选项列表 → 用户选择 → 继续引导或确认
3. **多轮渐进式场景**（核心）：
   - 第1轮：用户说"分析日志" → 助手引导补充路径
   - 第2轮：用户说路径 → 助手引导补充分析类型
   - 第3轮：用户说"404 错误" → 助手确认完整，展示按钮
   - 第4轮：用户点"生成计划" → 助手总结全部累积需求 → 确认
4. **多轮后显式触发**：用户在多轮对话后说"生成计划" → LLM 回顾全部历史，评估完整性后响应（完整则确认，不完整则列清单引导）
5. **触发时信息不足**：用户说了3轮但信息仍不完整，突然说"生成计划" → LLM 列出 ✓已获取 + ✗仍缺失，引导补充
6. **闲聊场景**：用户问候/闲聊 → LLM 正常回复，不调用 tool
7. **跳过按钮场景**：按钮渲染后用户直接输入文本 → 清除 pending，正常发送
8. **连续多轮 UI Action**：LLM 多次请求用户交互 → 每轮正确替换占位 tool result
9. **中途改变需求**：用户说"不要分析日志了，改成分析 CPU" → LLM 更新累积理解，重新评估

---

## 注意事项

1. **`PendingUIState` 需要 `Clone`**：在 rsx! 闭包中需要捕获 pending 的快照。如果后续字段增多，可考虑用 `Arc` 包装。
2. **占位 tool result 替换逻辑**：通过 `content.contains("awaiting_user_input")` 查找，需确保没有其他 tool 误产生此字符串。可后续改为更健壮的方式（如标记位）。
3. **UI tool name 硬编码**：当前 `const UI_TOOL_NAMES: &[&str] = &["request_user_action"]` 为硬编码。可后续通过 tool 元数据中的标记或工具名前缀（如 `ui_*`）实现动态识别。
4. **工具注册时机**：`request_user_action` 必须在 `ChatService` 构建前注册到 `ToolRegistry`，否则 LLM 不知道此工具存在。
5. **Concurrent 安全**：`pending_ui` 使用 `SyncStorage`，在 spawn 异步任务中可安全读写。

---

## 实现计划

### 改动文件清单

| # | 阶段 | 文件 | 改动内容 | 复杂度 | 状态 |
|---|------|------|----------|--------|------|
| 1 | 一 | `crates/core/src/types.rs` | 新增 `UIAction`、`UIActionType` 类型 | 低 | ✅ 完成 |
| 2 | 一 | `crates/planned-agent/src/chat/event.rs` | 新增 `ChatEvent::UIActionRequest` 变体 | 低 | ✅ 完成 |
| 3 | 二 | `crates/planned-agent/src/chat/service.rs` | `PendingUIAction` + `ChatResponse` 扩展 + tool 拦截逻辑 | 中 | ✅ 完成 |
| 4 | 二 | `crates/planned-agent/src/chat/mod.rs` | 导出 `PendingUIAction` | 低 | ✅ 完成 |
| — | 二 | `crates/planned-agent/src/lib.rs` | **追加**：re-export `PendingUIAction` 到 crate root | 低 | ✅ 完成 |
| 5 | 三 | `crates/agent-gui/prompts/thorough/thorough_system.toml` | 重写 System Prompt（四种模式 + 跨轮累积） | 中 | ✅ 完成 |
| 6 | 三 | `crates/agent-gui/src/context/tools.rs` | 注册 `request_user_action` 自定义工具（NoopExecutor） | 低 | ✅ 完成 |
| 7 | 四 | `crates/agent-gui/src/pages/plan.rs` | `pending_ui` signal + `UIActionRequest` 事件处理 | 高 | ✅ 完成 |
| 8 | 四 | `crates/agent-gui/src/pages/plan.rs` | `handle_user_action` 函数（替换占位 + 继续 chat） | 高 | ✅ 完成 |
| 9 | 四 | `crates/agent-gui/src/pages/plan.rs` + `assets/plan.css` | UI 渲染（按钮组件 + CSS 样式） | 中 | ✅ 完成 |
| 10 | 四 | `crates/agent-gui/src/pages/plan.rs` | 边界处理（clear pending / 需求变更 / 触发词输入） | 低 | ✅ 完成 |

### 实施过程中追加的改动

| 文件 | 改动 | 原因 |
|------|------|------|
| `crates/planned-agent/src/lib.rs` | 添加 `PendingUIAction` 到 crate root re-export | `plan.rs` 需要通过 `planned_agent::PendingUIAction` 引用，但 lib.rs 未透传 |
| `crates/agent-gui/Cargo.toml` | 添加 `async-trait` workspace 依赖 | `NoopExecutor` 实现 `ToolExecutor` trait 需要 async_trait |

### 依赖关系与执行顺序

```
阶段一 (1, 2)          ← 可并行，无依赖
    ↓
阶段二 (3 → 4)         ← 依赖 1, 2
    ↓
阶段三 (5, 6)          ← 无代码依赖，可与阶段二并行
    ↓
阶段四 (7 → 8 → 9 → 10) ← 依赖 1-6 全部
```

### 最终实现说明

**plan.rs 核心改动：**

1. **`PendingUIState`** 结构体：存储 pending 状态（message + actions + history_snapshot）
2. **`pending_ui` signal**：`use_signal_sync(|| None::<PendingUIState>)`
3. **`run_chat_stream` 重写**：新增 `mut pending_ui` 参数，回调中捕获 `ChatEvent::UIActionRequest` 设置 pending 状态，响应时判断 `response.pending_ui_actions.is_empty()` 决定是否 finalize
4. **`send_message` 入口**：接收 `mut pending_ui`，发送前 `pending_ui.set(None)` 清除旧状态
5. **`handle_user_action`**：完整流程——获取 history snapshot → 替换占位 tool result → 更新 messages → 清除 pending → 追加 Assistant 占位 → spawn 继续 chat_with_callback
6. **UI 渲染**：消息区与输入区之间渲染 `chat-ui-actions`，遍历 `pending.actions` 生成按钮，`onclick` 调用 `handle_user_action`
7. **if-let-else 模式**：`pending_ui` 为空时 `rsx! {}` 渲染空节点

**plan.css 新增样式：**

- `.chat-ui-actions` / `__message` / `__buttons`：flex 容器布局
- `.chat-ui-action-btn`：基础按钮样式 + hover/active 态
- `.chat-ui-action-btn--Select`：虚线边框区分选择类型
- `.chat-ui-action-btn--Input`：透明背景区分输入类型

### 验证方式

| 阶段 | 验证命令 | 状态 |
|------|----------|------|
| 一 + 二 | `cargo check -p planned-agent` 编译通过 | ✅ 通过 |
| 三 | `FilePromptManager` 能正确加载解析 toml | ✅ 通过 |
| 四 | `cargo check -p planned-agent-gui` 编译通过 | ✅ 通过 |
| 端到端 | 启动 agent-gui，发送需求消息，验证按钮渲染和点击继续流程 | ⏳ 待验证 |
