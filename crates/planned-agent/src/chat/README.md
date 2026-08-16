# chat 模块说明

有状态、后台 loop 驱动、事件订阅的多轮对话服务（`ChatService`），
是 v1 [`crate::chat`] 的重构版。

## 公开 API 一览

```rust
pub use service::{
    SendTicket,           // send 的完成凭证（可 await）
    SubscriptionGuard,    // RAII 事件订阅守卫
    SubscriptionId,       // 事件订阅 ID
    ChatConfig,         // 配置
    ChatEvent,          // 事件协议
    ChatService,        // 服务入口
};
```

## 快速开始

```rust
use planned_agent::chat::{ChatService, ChatConfig};

// 1. 创建服务
let service = ChatService::new(
    ai_manager,
    tool_registry,
    prompt_manager,
    ChatConfig::default(),
)?;

// 2. 启动 driver（必须在 send 前调用一次）
service.start_driver()?;

// 3. 发送消息并等待完成
let ticket = service.send_text("你好")?;
ticket.await?;

// 或者 fire-and-forget，通过事件监听结果
service.send_text("你好")?;
```

## 目录结构

```
chat/
├── mod.rs       入口：模块声明 + 对外重导出（5 个公开类型）
├── service/     对外 API 层 —— 接口与入队，无后台逻辑
│   ├── mod.rs    重导出本层公开类型
│   ├── service.rs  ChatService（聊天服务入口）
│   ├── ticket.rs   SendTicket（send 的完成凭证，可 await）
│   ├── config.rs   ChatConfig（纯数据配置）
│   └── event.rs    ChatEvent / SubscriptionId（事件协议与订阅 ID）
├── state/       内部状态层 —— 对话所需的全部共享/累积状态
│   ├── mod.rs    重导出本层内部类型（仅 chat 内可见）
│   ├── state.rs    State 容器（Arc 共享）+ History / Subscribers 封装
│   ├── command.rs  Command（命令队列）/ RunState（运行状态）
│   └── accumulator.rs  ToolCallAccumulator（流式 tool_call 累积）
├── driver/      后台 driver —— 真正的执行逻辑
│   ├── mod.rs      driver_loop：常驻 task，串行消费命令队列
│   ├── bridge.rs   ToolExecutionBridge trait + SubAgentBridge（流式旁路）
│   ├── round.rs    run_conversation：一次 send 的完整多轮 loop
│   ├── confirm.rs  await_confirm：等待用户 UI 确认
│   └── prompt.rs   inject_system_prompt：system prompt 注入（幂等）
├── sub_agent/   子 agent 实现
│   └── mod.rs    SubAgentRunner / SubAgentSession
├── tools/       工具相关
│   ├── mod.rs     build_tool_definitions（白名单过滤）
│   └── ui.rs      parse_ui_actions（request_user_action 参数解析）
└── tests.rs     集成测试
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

## 主 Agent 完整数据流

```
用户输入
  │
  ▼
ChatService::send_text("帮我分析代码")
  │  → Command::Send 入队
  │  → return SendTicket
  ▼
driver_loop (后台 task，串行消费)
  │  → Command::Send { message, done }
  │  → history.push_user(message)           ← user 消息入 history
  │  → pick_ui_strategy() → BlockAndConfirm
  ▼
run_conversation(state, rx, queue, BlockAndConfirm, bridge)
  │
  │  ╔═══════════════════════════════════════════╗
  │  ║  第 1 轮循环                                ║
  │  ╠═══════════════════════════════════════════╣
  │  ║                                           ║
  │  ║  构造 ChatCompletionRequest                ║
  │  ║    messages = history.snapshot()           ║
  │  ║    tools = build_tool_definitions()        ║
  │  ║                                           ║
  │  ║  流式消费 LLM 响应                          ║
  │  ║    → TextDelta        → subscribers emit  ║→ 前端渲染
  │  ║    → ReasoningDelta   → subscribers emit  ║→ 前端渲染
  │  ║    → ToolCallStart    → subscribers emit  ║→ 前端渲染
  │  ║    → ToolCallArgsDelta→ subscribers emit  ║→ 前端渲染
  │  ║    → ToolCallComplete → subscribers emit  ║→ 前端渲染
  │  ║                                           ║
  │  ║  history.push_assistant(assistant_msg)     ║← assistant 消息入 history
  │  ║                                           ║
  │  ║  分流工具调用                               ║
  │  ║    ├─ backend_tools (普通工具)              ║
  │  ║    │   → call_tool()                      ║
  │  ║    │   → history.push_tool(id, content)   ║← tool 消息入 history
  │  ║    │                                      ║
  │  ║    ├─ sub_agent_tools (子 agent 工具)       ║
  │  ║    │   → bridge.needs_stream() → true      ║
  │  ║    │   → bridge.create_stream()            ║
  │  ║    │   → call_tool_streamed()              ║→ 子 agent 独立运行
  │  ║    │   → history.push_tool(最终结果)        ║← 只有最终结果入 history
  │  ║    │                                      ║
  │  ║    └─ ui_tools (request_user_action)       ║
  │  ║        → emit UIActionRequest             ║→ 前端弹出确认卡片
  │  ║        → await_confirm() 阻塞等待          ║
  │  ║        → Command::Confirm 入队             ║
  │  ║        → history.push_tool(choice)        ║← tool 消息入 history
  │  ║                                           ║
  │  ╚═══════════════════════════════════════════╝
  │  （循环直到无 tool_calls 或达到 max_tool_rounds）
  │
  ▼
finish_send(Completed)
  │  → subscribers.emit(Done)
  │  → done.send(SendOutcome::Completed)
  ▼
前端收到 Done，对话结束
```

**主 agent 的 history 变化：**

```
[0] User:      "帮我分析代码"
[1] Assistant:  tool_calls=[read_file, execute_shell]
[2] Tool:       "/src/main.rs 的内容..."
[3] Tool:       "cargo build 成功"
[4] Assistant:  "分析结果如下..."
```

## 子 Agent 完整数据流

### 阶段一：启动与执行

```
主 agent LLM 决定调用子 agent
  │  assistant tool_calls=[{ name: "data_expert", arguments: { task: "分析数据" } }]
  ▼
round.rs: bridge.needs_stream("data_expert") → true
  │
  │  ┌─ bridge.create_stream("data_expert", "call_abc123")
  │  │    → (stream: ToolStreamSender, handle: JoinHandle)
  │  │    → handle 内部：tokio::spawn relay task
  │  │       while let Some(ev) = stream_rx.recv()
  │  │           subscribers.emit(ev.event)    ← 转发到父 agent subscribers
  │  │
  │  └─ tool_registry.call_tool_streamed("data_expert", args, "call_abc123", stream)
  │       │
  │       ▼
  │     SubAgentToolExecutor::execute_streamed
  │       │
  │       ▼
  │     SubAgentRunner::start(arguments, stream)
  │       │
  │       ├─ set_run_id("call_abc123")         ← 标记子 agent 身份
  │       ├─ service.send_text("分析数据")
  │       │    → Command::Send 入队
  │       │    → 子 agent 自己的 driver_loop 消费
  │       │    → history.push_user("分析数据")  ← 子 agent 自己的 history
  │       │    → pick_ui_strategy() → EmitAndSuspend (因为 run_id = Some)
  │       │
  │       ▼
  │     子 agent的 run_conversation(state, rx, queue, EmitAndSuspend, bridge)
  │       │
  │       │  ╔═══════════════════════════════════════════╗
  │       │  ║  第 1 轮循环                                ║
  │       │  ╠═══════════════════════════════════════════╣
  │       │  ║                                           ║
  │       │  ║  构造 ChatCompletionRequest                ║
  │       │  ║    messages = 子 agent history.snapshot()  ║
  │       │  ║    tools = build_tool_definitions()        ║
  │       │  ║                                           ║
  │       │  ║  流式消费 LLM 响应                          ║
  │       │  ║    → TextDelta        → subscribers emit  ║──┐
  │       │  ║    → ToolCallStart    → subscribers emit  ║  │ 子 agent 自己的
  │       │  ║    → ToolCallComplete → subscribers emit  ║  │ subscribers
  │       │  ║                                           ║  │
  │       │  ║  子 agent history.push_assistant(msg)     ║  │
  │       │  ║                                           ║  │
  │       │  ║  分流：backend_tools                       ║  │
  │       │  ║    → call_tool() 普通工具                  ║  │
  │       │  ║    → 子 agent history.push_tool()          ║  │
  │       │  ║                                           ║  │
  │       │  ║  分流：ui_tools                            ║  │
  │       │  ║    → emit UIActionRequest { run_id }      ║──┤
  │       │  ║    → return Suspended { run_id }          ║  │
  │       │  ║                                           ║  │
  │       │  ╚═══════════════════════════════════════════╝  │
  │       │                                                │
  │       │  on_chat_with_guard 闭包（collect_until_outcome） │
  │       │    → stream_clone.emit_event_sync(chat_event) ◄─┘
  │       │    → mpsc channel                              │
  │       │    → bridge relay task recv                    │
  │       │    → 父 agent subscribers.emit(ev)             │
  │       │    → 前端 UI 渲染子 agent 的文本/工具调用          │
  │       │                                                │
  │       │  collect_until_outcome: SendOutcome::Suspended  │
  │       │    → SubAgentRunOutcome::AwaitingUserAction     │
  │       │                                                │
  │       ▼                                                │
  │     execute_streamed 收到 AwaitingUserAction             │
  │       → session 存入 store (key="call_abc123")          │
  │       → 阻塞等待 signal_resume                          │
  │                                                        │
  │  ◄── call_tool_streamed 此时阻塞                        │
  │  ◄── round.rs 此时阻塞                                 │
  │  ◄── driver_loop 此时阻塞                              │
  │                                                        │
  │  ─── 但父 agent 的 subscribers 仍然活跃，前端可以操作 ───  │
```

### 阶段二：挂起-恢复

```
  用户在前端确认
  │  前端点击确认 → 选择 "approved"
  ▼
ChatService::resume_sub_agent("call_abc123", {choice:"approved", action_id:"..."})
  │  → ToolRegistry::signal_resume("call_abc123", user_input)
  ▼
execute_streamed 被唤醒
  │  → session.resume(user_input, stream)
  ▼
SubAgentSession::resume
  │  → service.resume("approved", "...")
  │  → Command::Resume 入队（子 agent 的 driver 队列）
  ▼
子 agent 的 driver_loop 收到 Command::Resume
  │  → history.find_pending_ui_tool_call_id() → "tool_call_xyz"
  │  → history.push_tool("tool_call_xyz", {"choice":"approved","action_id":"..."})
  │    ← 闭合 assistant(tool_calls) → tool 消息协议
  │  → run_conversation(..., EmitAndSuspend) 继续
  │
  │  ╔═══════════════════════════════════════════╗
  │  ║  第 2 轮循环（从 history 继续）              ║
  │  ╠═══════════════════════════════════════════╣
  │  ║                                           ║
  │  ║  子 agent LLM 看到确认结果，继续推理         ║
  │  ║  → 无 tool_calls → break                  ║
  │  ║                                           ║
  │  ╚═══════════════════════════════════════════╝
  │
  │  history.push_assistant("数据分析结果如下...")
  │  → Completed
  ▼
collect_until_outcome: SendOutcome::Completed
  │  → last_text = "数据分析结果如下..."      ← 从子 agent history 提取
  │  → SubAgentRunOutcome::Done(ToolResult { content: last_text })
  ▼
execute_streamed 返回 ToolResult
  │  → call_tool_streamed 返回
  ▼
round.rs:267-287
  │  → result = Ok(ToolOutcome { result: ToolResult })
  │  → history.push_tool("call_abc123", "数据分析结果如下...")
  │    ← 最终结果进入父 agent history
  ▼
round.rs 下一轮循环
  │  → 父 agent LLM 看到子 agent 的结果
  │  → 继续推理或结束
  ▼
finish_send(Completed)
  → 前端收到 Done
```

### 两个 History 对比

```
子 agent history（独立，不暴露给父 agent）：
  [0] User:     "分析数据"
  [1] Assistant: tool_calls=[query_db]
  [2] Tool:      查询结果...
  [3] Assistant: tool_calls=[request_user_action]
  [4] User:      ← 此处挂起，等待确认
  ──── resume ────
  [5] Tool:      {"choice":"approved","action_id":"..."}
  [6] Assistant: "数据分析结果如下..."

父 agent history（包含子 agent 的最终结果）：
  [0] User:     "帮我分析代码"
  [1] Assistant: tool_calls=[data_expert(task="分析数据")]
  [2] Tool:      "数据分析结果如下..."    ← 只有这一条，子 agent 的中间过程全部省略
  [3] Assistant: "根据子 agent 的分析..."
```

### 事件桥接路径

```
子 agent subscribers.emit()
  │
  ├─ 路径 1：on_chat_with_guard 闭包
  │    stream_clone.emit_event_sync(chat_event)
  │      → mpsc::Sender::try_send
  │      → mpsc channel
  │      → bridge relay task recv
  │      → 父 agent state.subscribers.emit()
  │      → 前端 UI 收到事件
  │
  └─ 路径 2：最终结果（非事件，同步返回）
       collect_until_outcome 提取 last_text
         → ToolResult
         → call_tool_streamed 返回值
         → round.rs push_tool
         → 父 agent history
```

## 关键行为契约

- **显式启动**：`start_driver()` 必须在首次 `send` / `confirm_user_action` /
  `reset_session` 前调用（幂等）。主 agent 在 service ready 后调用一次；
  子 agent 在 `SubAgentRunner::start()` 内部调用。
- **串行队列**：所有 `send` / `confirm_user_action` 严格保序；awaiting
  期间的 `send` 排队，当前对话结束后再处理。
- **取消**：`stop()` 在下个检查点生效（流式间隙 / 工具后 / UI 等待中），
  取消后发出 `Done{cancelled:true}`；下次 `send` 重置标志。
- **回滚**：对话失败时历史回滚到本次 `send` 写入前（含 system 注入），
  不残留脏上下文。
- **协议闭合**：取消或达到 `max_tool_rounds` 时清理未闭合的
  assistant(tool_calls) 消息，保证 OpenAI 协议顺序。
- **子 agent 隔离**：子 agent 的中间过程不进入父 agent history，
  只有最终结果（最后一条 assistant 文本）作为 tool 消息进入。
- **事件桥接**：子 agent 的实时事件通过 mpsc channel 桥接到父 agent subscribers，
  前端通过 `session_id`（run_id）区分主 agent 和子 agent 的事件来源。

## 设计要点

| 特性 | 说明 |
|------|------|
| **显式启动** | `start_driver()` 必须在首次 send 前调用（幂等），主 agent 在 service ready 后调用，子 agent 在 start() 时调用 |
| **自动回滚** | 对话失败时历史回滚到本次 send 前 |
| **Weak 生命周期** | driver 不阻止 service drop，无内存泄漏 |
| **panic 隔离** | 单个订阅者 panic 不影响其他订阅者和 driver |
| **协议闭合** | 取消/超时时自动清理未闭合的 tool_calls 消息 |
| **Bridge 抽象** | ToolExecutionBridge trait 封装子 agent 流式旁路，round.rs 不硬编码 |
| **子 agent 复用** | 子 agent 直接复用 ChatService，不重复实现 ReAct 循环 |
| **挂起-恢复** | 子 agent 遇到 UI 交互时 EmitAndSuspend，保留 history checkpoint，resume 时闭合协议继续 |
