# v2_chat 模块说明

有状态、后台 loop 驱动、事件订阅的多轮对话服务（`V2ChatService`），
是 v1 [`crate::chat`] 的重构版。

## 公开 API 一览

```rust
pub use service::{
    SendTicket,           // send 的完成凭证（可 await）
    SubscriptionGuard,    // RAII 事件订阅守卫
    SubscriptionId,       // 事件订阅 ID
    V2ChatConfig,         // 配置
    V2ChatEvent,          // 事件协议
    V2ChatService,        // 服务入口
};
```

## 快速开始

```rust
use planned_agent::v2_chat::{V2ChatService, V2ChatConfig};

// 1. 创建服务
let service = V2ChatService::new(
    ai_manager,
    tool_registry,
    prompt_manager,
    V2ChatConfig::default(),
)?;

// 2. 发送消息并等待完成
let ticket = service.send_text("你好")?;
ticket.await?;

// 或者 fire-and-forget，通过事件监听结果
service.send_text("你好")?;
```

## 目录结构

```
v2_chat/
├── mod.rs       入口：模块声明 + 对外重导出（5 个公开类型）
├── service/     对外 API 层 —— 接口与入队，无后台逻辑
│   ├── mod.rs    重导出本层公开类型
│   ├── service.rs  V2ChatService（聊天服务入口）
│   ├── ticket.rs   SendTicket（send 的完成凭证，可 await）
│   ├── config.rs   V2ChatConfig（纯数据配置）
│   └── event.rs    V2ChatEvent / SubscriptionId（事件协议与订阅 ID）
├── state/       内部状态层 —— 对话所需的全部共享/累积状态
│   ├── mod.rs    重导出本层内部类型（仅 v2_chat 内可见）
│   ├── state.rs    State 容器（Arc 共享）+ History / Subscribers 封装
│   ├── command.rs  Command（命令队列）/ RunState（运行状态）
│   └── accumulator.rs  ToolCallAccumulator（流式 tool_call 累积）
├── driver/      后台 driver —— 真正的执行逻辑
│   ├── mod.rs     driver_loop：常驻 task，串行消费命令队列
│   ├── round.rs    run_conversation：一次 send 的完整多轮 loop
│   ├── confirm.rs  await_confirm：等待用户 UI 确认
│   └── prompt.rs   inject_system_prompt：system prompt 注入（幂等）
├── tools/       工具相关
│   ├── mod.rs     build_tool_definitions（白名单过滤）
│   └── ui.rs      parse_ui_actions（request_user_action 参数解析）
└── tests.rs     集成测试（9 个用例）
```

## 分层原则

- **service/ 只做接口**：`send` / `confirm_user_action` 把命令写入
  `mpsc` 队列后立即返回（不堵塞），返回 `SendTicket` 供调用方按需等待。
- **state/ 只装数据**：`State` 是 `Arc` 共享的容器，`History` 与
  `Subscribers` 分别把「消息历史」和「事件订阅」的 `Mutex` 封装起来，
  调用方不再直接接触锁。
- **driver/ 只跑逻辑**：唯一一个常驻 task 串行消费队列，一次
  `send` 引发的完整多轮 tool-call / UI 确认循环都在这里完成。
- **tools/ 只处理工具**：LLM 侧的 ToolDefinition 构建与
  `request_user_action` 的 UI 参数解析。

可见性约定：对外只有 `service/` 的 5 个类型是 `pub`；`state/` 内部类型
经 `mod.rs` 以 `pub(super)` 重导出，`driver/` / `tools/` 均 `pub(super)`，
crate 外不可见。

## 一次 send 的完整生命周期

```
调用方 send(msg)  ──►  命令入队（立即返回 SendTicket）
                         │
                         ▼
                 driver_loop 取出 Command::Send
                         │
                         ▼
                 run_conversation 开始多轮 loop：
                   ┌─────────────────────────────┐
                   │ 每轮：RoundStart 事件        │
                   │  → LLM 流式（TextDelta /     │
                   │    ReasoningDelta /          │
                   │    ToolCallStart/ArgsDelta/  │
                   │    ToolCallComplete）        │
                   │  → assistant 消息入历史       │
                   │  → RoundEnd 事件             │
                   │  → 若无 tool_calls → 结束     │
                   │  → 后端工具：执行 + Tool 消息  │
                   │    入历史（ToolExecuted 事件） │
                   │  → UI 工具：发 UIActionRequest│
                   │    → await_confirm 挂起       │
                   │      （用户 confirm 后压入     │
                   │        Tool 消息，继续下一轮） │
                   └─────────────────────────────┘
                         │
                         ▼
                  Done{cancelled} 或 Error 事件
                  SendTicket::wait() 返回
```

## 数据流向图

```
                    ┌──────────────┐
                    │  用户输入     │
                    └──────┬───────┘
                           │ send_text("开始")
                           ▼
                    ┌──────────────┐
                    │  service 层   │  入队 Command::Send，立即返回
                    └──────┬───────┘
                           │ mpsc channel
                           ▼
                    ┌──────────────┐
                    │  driver_loop │  串行消费命令
                    └──────┬───────┘
                           │
                           ▼
              ┌─── run_conversation ───┐
              │                         │
              │  ① inject_system_prompt │
              │  ② LLM 流式调用        │ ──→ emit(TextDelta/ReasoningDelta)
              │  ③ assistant 入历史      │ ──→ emit(RoundEnd)
              │  ④ 工具执行/等待确认     │
              │     ├─ 后端工具 → 执行    │ ──→ emit(ToolExecuted)
              │     └─ UI 工具 → 挂起    │ ──→ emit(UIActionRequest)
              │         │                  │
              │         │  await_confirm   │ ◄── Command::Confirm (用户点击)
              │         │  push_tool       │
              │         ▼                  │
              │     下一轮 loop            │
              └────────────────────────────┘
                           │
                           ▼
                    ┌──────────────┐
                    │  emit(Done)  │  通知 UI 对话结束
                    │  SendTicket  │  oneshot 返回 Ok(())
                    └──────────────┘
```

### 数据流关键节点说明

| 节点 | 模块 | 数据 | 方向 |
|------|------|------|------|
| **用户输入** | UI 层 | `String` | → service |
| **Command 入队** | `service/service.rs` | `Command::Send` | → mpsc channel |
| **driver 消费** | `driver/mod.rs` | `Command` 枚举 | 队列 → driver |
| **LLM 流式** | `driver/round.rs` | `TextDelta` / `ReasoningDelta` | driver → subscribers → UI |
| **assistant 入历史** | `state/state.rs` `History` | `Message` | driver → history |
| **后端工具执行** | `tool_registry` | `ToolResult` | registry → history（tool 消息） |
| **UI 工具挂起** | `driver/confirm.rs` | `UIActionRequest` | driver → UI → `Confirm` 命令 → driver |
| **对话结束** | `driver/mod.rs` | `V2ChatEvent::Done` | driver → subscribers → UI |
| **SendTicket 完成** | `service/ticket.rs` | `Result<()>` | driver → oneshot → 调用方 |

## 如何阅读代码（推荐路径）

1. `mod.rs` —— 先看类型总览与目录结构；
2. `service/service.rs` —— 看对外接口（方法签名与文档注释）；
3. `service/event.rs` + `service/config.rs` —— 事件协议与配置；
4. `driver/mod.rs` —— driver_loop 如何消费命令、管理取消与回滚；
5. `driver/round.rs` —— 一次对话的多轮循环细节（流式累积、工具执行、
   UI 确认、max_tool_rounds 清理）；
6. `state/state.rs` —— History / Subscribers 的锁封装与清理语义。

## 关键行为契约

- **串行队列**：所有 `send` / `confirm_user_action` 严格保序；awaiting
  期间的 `send` 排队，当前对话结束后再处理。
- **取消**：`stop()` 在下个检查点生效（流式间隙 / 工具后 / UI 等待中），
  取消后发出 `Done{cancelled:true}`；下次 `send` 重置标志。
- **回滚**：对话失败时历史回滚到本次 `send` 写入前（含 system 注入），
  不残留脏上下文。
- **协议闭合**：取消或达到 `max_tool_rounds` 时清理未闭合的
  assistant(tool_calls) 消息，保证 OpenAI 协议顺序。

---

## 使用指南

### 创建服务

```rust
use planned_agent::v2_chat::{V2ChatService, V2ChatConfig};

// 方式一：通过 AiManager（推荐）
let service = V2ChatService::new(
    ai_manager,           // AiManager 实例
    tool_registry,        // Arc<ToolRegistry>
    prompt_manager,       // Arc<PM>
    V2ChatConfig::default(),
)?;

// 方式二：直接注入 AiClient（测试/自定义场景）
let service = V2ChatService::from_ai_client(
    ai_client,            // Arc<dyn AiClient>
    tool_registry,
    prompt_manager,
    config,
);
```

### 发送消息

```rust
use planned_agent_core::ai::types::{Message, MessageRole, MessageContent};

// 方式一：完整 Message
let ticket = service.send(Message {
    role: MessageRole::User,
    content: Some(MessageContent::Text { text: "你好".into() }),
    tool_calls: None,
    tool_call_id: None,
    name: None,
    reasoning_content: None,
})?;

// 方式二：便捷方法（推荐）
let ticket = service.send_text("你好")?;

// 等待对话完成（可选）
ticket.await?;

// 不等待（fire-and-forget），通过事件监听结果
drop(ticket);
```

### 订阅事件

```rust
use planned_agent::v2_chat::{V2ChatEvent, SubscriptionGuard};

// 推荐：RAII 守卫，自动退订
let guard: SubscriptionGuard = service.on_chat(|event| {
    match event {
        V2ChatEvent::Chat(chat_event) => {
            // 流式文本、工具调用、UI 交互等
        }
        V2ChatEvent::Done { cancelled } => {
            // 对话结束
        }
        V2ChatEvent::Error(msg) => {
            // 错误
        }
    }
});

// guard drop 时自动退订，无需手动管理
drop(guard);
```

### UI 交互确认

```rust
// 监听 UIActionRequest 事件
service.on_chat(|event| {
    if let V2ChatEvent::Chat(ChatEvent::UIActionRequest {
        message, actions, session_id
    }) = event {
        // 渲染 UI 卡片给用户
        // 用户点击后调用：
        service.confirm_user_action(
            &tool_call_id,  // 从事件获取
            &choice,        // 用户选择
            &action_id,     // 动作 ID
        ).unwrap();
    }
});
```

### 控制与查询

```rust
// 取消当前对话（下个检查点生效）
service.stop();
assert!(service.is_cancelled());

// 查询状态
service.is_awaiting_user_action();  // 是否在等 UI 确认
service.history();                   // 获取消息历史快照

// 热切换配置
service.set_system_prompt_template(Some("new/template".into()));
service.set_allowed_tools(Some(vec!["tool1".into(), "tool2".into()]));

// 会话重置（安全，入队串行执行）
service.reset_session()?;

// 立即清空历史（需确保无活跃对话）
service.clear();
```

---

## 配置说明

```rust
pub struct V2ChatConfig {
    /// AI provider 名；None 时使用默认 provider
    pub provider: Option<String>,
    
    /// system prompt 模板路径（prompts/ 下的 toml，不含 .toml 后缀）
    /// None 时不注入 system message
    pub system_prompt_template: Option<String>,
    
    /// 采样温度（None = provider 默认值）
    pub temperature: Option<f32>,
    
    /// 最大生成 token 数（None = provider 默认值）
    pub max_tokens: Option<u32>,
    
    /// tool 调用循环上限（达到后强制结束）
    pub max_tool_rounds: usize,  // 默认 10
    
    /// 是否启用思考模式（hint，具体行为由 provider 决定）
    pub enable_thinking: bool,   // 默认 true
    
    /// 工具白名单（None = 全部可用）
    pub allowed_tools: Option<Vec<String>>,
    
    /// system prompt 的 {{ context }} 变量值
    pub context: Option<String>,
}
```

---

## 事件类型

```rust
pub enum V2ChatEvent {
    /// 复用 core 流式事件
    Chat(ChatEvent),
    
    /// 一次 send 引发的整段对话结束
    Done { cancelled: bool },
    
    /// 可终止性异常（LLM 请求失败等）
    Error(String),
}
```

### ChatEvent 变体

| 事件 | 说明 |
|------|------|
| `RoundStart { round }` | 每轮开始 |
| `TextDelta(text)` | 流式文本增量 |
| `ReasoningDelta(text)` | 流式推理增量 |
| `ToolCallStart { id, name }` | 工具调用开始 |
| `ToolCallArgsDelta { id, delta }` | 工具参数增量 |
| `ToolCallComplete { id, name, arguments }` | 工具调用完成 |
| `ToolExecuted { id, name, is_error, content }` | 工具执行完成 |
| `UIActionRequest { message, actions, session_id }` | UI 交互请求 |
| `RoundEnd { message }` | 每轮结束 |

---

## 常见场景

### 场景 A：简单对话

```rust
let service = V2ChatService::new(...)?;
let ticket = service.send_text("解释 Rust 的所有权")?;
ticket.await?;
```

### 场景 B：流式监听 + UI 交互

```rust
let service = V2ChatService::new(...)?;
let _guard = service.on_chat(|event| {
    match event {
        V2ChatEvent::Chat(ChatEvent::TextDelta(t)) => print!("{t}"),
        V2ChatEvent::Chat(ChatEvent::UIActionRequest { message, actions, .. }) => {
            // 渲染 UI，等待用户点击
            // 用户点击后调用 service.confirm_user_action(...)
        }
        V2ChatEvent::Done { .. } => println!("\n完成"),
        _ => {}
    }
});
service.send_text("帮我写个函数")?;
// 不需要 await，事件会持续推送
```

### 场景 C：模板热切换

```rust
// 停止当前对话
service.stop();

// 切换模板
service.set_system_prompt_template(Some("new/template".into()));
service.set_allowed_tools(Some(vec!["tool1".into()]));

// 重置会话（清空历史，新模板生效）
service.reset_session()?;

// 发送新对话
service.send_text("新话题")?;
```

### 场景 D：并发保护

```rust
// 串行队列保证：多个 send 不会并发执行
let ticket1 = service.send_text("任务1")?;
let ticket2 = service.send_text("任务2")?;  // 排队等待

ticket1.await?;
ticket2.await?;
```

---

## 设计要点

| 特性 | 说明 |
|------|------|
| **Lazy 启动** | 第一次 send 时才启动后台 driver |
| **自动回滚** | 对话失败时历史回滚到本次 send 前 |
| **Weak 生命周期** | driver 不阻止 service drop，无内存泄漏 |
| **panic 隔离** | 单个订阅者 panic 不影响其他订阅者和 driver |
| **协议闭合** | 取消/超时时自动清理未闭合的 tool_calls 消息 |

---

## 子 Agent

`V2ChatService` 可以作为子 agent 被父 agent 调用。核心思路：**不重复实现 ReAct 循环**，
直接复用 `V2ChatService::send_text`。

### 公开类型

```rust
pub use sub_agent::{
    V2ChatSubAgentRunner,    // 实现 SubAgentSessionRunner，注册为工具
    V2ChatSubAgentSession,   // 实现 SubAgentSession，支持挂起-恢复
};
```

### 注册子 Agent

```rust
use planned_agent::v2_chat::{V2ChatSubAgentRunner, V2ChatService, V2ChatConfig};

// 1. 创建子 agent 的 V2ChatService（独立 prompt 和工具白名单）
let child_service = V2ChatService::new(
    ai_manager.clone(),
    tool_registry.clone(),
    prompt_manager.clone(),
    V2ChatConfig {
        system_prompt_template: Some("data_expert/system".into()),
        allowed_tools: Some(vec!["query_data".into(), "create_chart".into()]),
        ..Default::default()
    },
)?;

// 2. 创建 Runner（depth=0, max_depth=3 防递归）
let runner = V2ChatSubAgentRunner::new(child_service, 0, 3);

// 3. 注册为子 agent 工具
tool_registry.register_sub_agent(
    Tool {
        name: "data_expert".to_string(),
        description: "数据分析专家".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "任务描述" }
            },
            "required": ["task"]
        }),
    },
    vec![ToolCategory::Data],
    Arc::new(runner),
);
```

### 子 Agent 数据流向图

```
═══════════════════════════════════════════════════════════════
  父 Agent 侧（driver/round.rs）
═══════════════════════════════════════════════════════════════

父 LLM 返回 tool_call: data_expert(task="分析数据")
    │
    ├─ 判断 is_sub_agent = true（ToolRegistry.metadata.source == SubAgent）
    │
    ├─ 创建 ToolStreamSender ─── mpsc::channel(64) ─── ToolStreamReceiver
    │
    ├─ spawn 后台任务：ToolStreamReceiver → 逐条转发到父 subscribers.emit()
    │
    └─ tool_registry.call_tool_streamed("data_expert", args, call_id, stream)
         │
         ▼
═══════════════════════════════════════════════════════════════
  SubAgentRunner::start()（sub_agent.rs）
═══════════════════════════════════════════════════════════════

    ├─ 防递归检查：depth >= max_depth → 返回错误 ToolResult
    │
    ├─ self.service.send_text("分析数据")
    │    └─ Command::Send 入队子 agent 的 mpsc 队列
    │
    └─ collect_until_outcome(service, ticket, stream)
         │
         ├─ 注册临时 on_chat_with_guard：子 agent 事件 → stream.emit_event_sync()
         │
         └─ ticket.wait().await（等待子 agent 对话结束）
              │
              ▼
═══════════════════════════════════════════════════════════════
  子 Agent driver_loop → run_conversation
  （与父 agent 完全相同的 ReAct 循环）
═══════════════════════════════════════════════════════════════

    ┌─────────────────────────────────────────────────────┐
    │  每轮循环：                                          │
    │                                                     │
    │  ① LLM 流式调用                                     │
    │     TextDelta / ReasoningDelta                     │
    │     → on_chat 触发 → stream.emit_event_sync()       │
    │     → ToolStreamReceiver → spawn 后台任务            │
    │     → state_clone.subscribers.emit(V2ChatEvent)     │
    │     → 父 agent 的 on_chat handler 收到               │
    │                                                     │
    │  ② 子 agent 调用后端工具                              │
    │     → 直接执行，tool 消息入子 agent history            │
    │     → 事件转发到父 agent（同上路径）                    │
    │                                                     │
    │  ③ 子 agent 调用 UI 工具（request_user_action）        │
    │     → emit(UIActionRequest)                         │
    │     → on_chat handler 转发为 SubAgentUIAction        │
    │     → 父 agent 的 subscribers.emit()                 │
    │     → 父 UI 渲染卡片                                 │
    │                                                     │
    │  ④ 子 agent 调用嵌套子 agent 工具                     │
    │     → SubAgentRunOutcome::Suspended                 │
    │     → 创建 V2ChatSubAgentSession                    │
    │     → runner 返回 Suspended，父 driver 收到           │
    │     → 父 agent 存储 session，等待嵌套 UI 确认         │
    │                                                     │
    └─────────────────────────────────────────────────────┘
              │
              ▼
═══════════════════════════════════════════════════════════════
  子 agent 对话结束（Done 事件）
═══════════════════════════════════════════════════════════════

    collect_until_outcome 收到 ticket.wait() = Ok(())
    │
    ├─ service.history() → 提取最后一条 assistant 文本
    └─ 返回 SubAgentRunOutcome::Done(ToolResult { content })
         │
         ▼
═══════════════════════════════════════════════════════════════
  父 agent 接收结果
═══════════════════════════════════════════════════════════════

    call_tool_streamed 返回 ToolResult
    │
    ├─ tool 消息入父 agent history: push_tool(call_id, content)
    ├─ emit(ToolExecuted { id, name, content })
    └─ 父 agent 下一轮 LLM 调用携带此 tool 消息继续
```

### 子 Agent UI 交互（挂起-恢复）流向

```
子 agent 调用 request_user_action
    │
    ▼
driver/round.rs: emit(UIActionRequest)
    │
    ▼
sub_agent.rs: on_chat handler 捕获
    │  stream.emit_event_sync(UIActionRequest { session_id })
    │  → SubAgentRunOutcome::Suspended(ToolResult { session_id })
    │
    ▼
父 agent driver/round.rs: call_tool_streamed 返回 Suspended
    │  创建 V2ChatSubAgentSession { service, depth, max_depth }
    │  存储 session（父 agent 的 tool 执行结果）
    │
    ▼
父 UI 渲染卡片 → 用户点击确认
    │
    ▼
前端调用 parent_service.resume_sub_agent("data_expert", session_id, choice)
    │
    ▼
Command::ResumeSubAgent 入父 agent 队列
    │
    ▼
driver_loop 收到 → session.resume(user_input)
    │
    ├─ self.service.send_text(choice)    ← 把用户选择作为新消息发给子 agent
    ├─ collect_until_outcome()           ← 重新监听事件流
    └─ 子 agent 继续 ReAct 循环
         │
         ├─ 可能再次 suspend（嵌套 UI）
         └─ 自然完成 → Done → ToolResult 返回父 agent
```

### UI 交互处理

```rust
// 父 agent 监听子 agent 的 UI 交互请求
service.on_chat(|event| {
    if let V2ChatEvent::SubAgentUIAction {
        agent_name,
        session_id,
        message,
        actions,
        ..
    } = event {
        // 1. 渲染 UI 卡片给用户
        render_ui_card(&message, &actions);
        
        // 2. 用户确认后，恢复子 agent
        service.resume_sub_agent(
            &agent_name,
            &session_id,
            json!({ "choice": "approve", "action_id": "confirm" }),
        )?;
    }
});
```

### 防递归

```rust
// Runner 构造时指定嵌套深度限制
let runner = V2ChatSubAgentRunner::new(
    child_service,
    depth: 0,       // 当前深度
    max_depth: 3,   // 最大允许嵌套 3 层
);

// start() 中检查：depth >= max_depth → 返回错误
```

### 生命周期

| 时刻 | Arc\<State\> 引用计数 | driver 状态 |
|---|---|---|
| runner 创建 | 1 | 未启动 |
| start() → send_text() | 1 | 启动 |
| 自然完成，session 不创建 | 0（runner drop） | Weak 失败 → 退出 |
| 挂起，session 创建 | 1（session 持有） | 存活 |
| resume() → 完成 | 0（session drop） | Weak 失败 → 退出 |

### 完整示例

```rust
use planned_agent::v2_chat::{
    V2ChatService, V2ChatConfig, V2ChatEvent,
    V2ChatSubAgentRunner,
};

// 创建父 agent
let parent_service = V2ChatService::new(...)?;

// 创建子 agent
let child_service = V2ChatService::new(
    ai_manager.clone(),
    tool_registry.clone(),
    prompt_manager.clone(),
    V2ChatConfig {
        system_prompt_template: Some("coding_expert/system".into()),
        ..Default::default()
    },
)?;

// 注册子 agent 工具
let runner = V2ChatSubAgentRunner::new(child_service, 0, 3);
tool_registry.register_sub_agent(
    coding_expert_tool,
    vec![ToolCategory::Dev],
    Arc::new(runner),
);

// 父 agent 监听事件
let _guard = parent_service.on_chat(|event| {
    match event {
        V2ChatEvent::Chat(ChatEvent::TextDelta(t)) => print!("{t}"),
        V2ChatEvent::SubAgentUIAction { session_id, message, .. } => {
            // 渲染子 agent 的 UI 交互
            println!("[子 agent 请求] {}", message);
            // 用户确认后：
            // parent_service.resume_sub_agent("coding_expert", &session_id, choice)?;
        }
        V2ChatEvent::Done { .. } => println!("\n完成"),
        _ => {}
    }
});

// 发送任务（LLM 会自动调用子 agent 工具）
parent_service.send_text("帮我写一个快排算法")?;
```
