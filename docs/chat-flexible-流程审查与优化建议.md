# chat + flexible 流程审查与优化建议

> 审查范围：`crates/planned-agent/src/chat`（服务端）、`crates/agent-gui/src/components/chat`
> （渲染层）、`crates/agent-gui/src/pages/plan/flexible`（灵活模式页面层）。
>
> 结论：整体分层清晰（service / state / driver / sub_agent / tools），文档与实现一致度高。
> 但存在若干**明确隐患**、**解耦问题**与**可优化点**，按优先级整理如下。
> 本文档仅做诊断，未改动任何代码。

---

## 一、明确的隐患（建议优先处理）

### 隐患 1：生产流程被临时测试配置破坏（最高优先级）

**位置**：`crates/agent-gui/src/pages/plan/flexible/chat_service_factory.rs::build_for_session`

**现象**：协调器（父 Agent）的 `ChatConfig` 里：

- `allowed_tools` 只保留了 `flexible_step_rua_demo`，而 `flexible_step1~5`、
  `flexible_state`、`request_user_action` **全部被注释掉**；
- `max_tool_rounds: 2`（正常调度 step1~5 需要多轮，默认应为 10）。

代码注释自述这是「轮次触顶复现测试的临时改动，测试完请还原」。

**影响**：当前状态下，正常的一次「澄清 → 执行 → 字段选择 → 参数确认 → 定稿」流程
**无法跑通**——协调器调不到任何 step 子 Agent，且 2 轮即触顶。

**建议**：还原为 5 个 step 子 Agent + `flexible_state` + `request_user_action` 的白名单，
`max_tool_rounds` 恢复默认（删除该行）。测试专用的 `flexible_step_rua_demo` / `builtin_read_documentation`
若非生产需要一并移除，或抽到测试 fixture 中。

---

### 隐患 2：存储层读写时序不一致（真实竞态）— ✅ 已解决（async 化）

**位置**：`crates/planned-agent/src/chat/storage.rs`（trait）、
`crates/agent-gui/src/pages/plan/flexible/chat_flexible_message_storage.rs`（SQLite 实现）

**现象**：同一个 `ChatHistoryStore` 实现里，曾存在两种执行模型混用：

| 方法 | 原执行模型 | 现执行模型 |
|---|---|---|
| `load` | `block_in_place` + `block_on`（同步） | ✅ `async` + `await` |
| `append` | `block_in_place` + `block_on`（同步） | ✅ `async` + `await` |
| `update` | `tokio::spawn`（异步 fire-and-forget） | ✅ `async` + `await` |
| `clear` | `tokio::spawn`（异步 fire-and-forget） | ✅ `async` + `await` |

**竞态成因**：`clear` / `update` 是 `tokio::spawn` 异步 fire-and-forget，而 `append` 是
同步 `block_on` 等完成。driver 在 `finish_send` 后紧接着 `push_user`，异步清理/更新的
删除可能迟到到下一次 `append` 之后执行，误删新写入的消息。

**处理方式**：把 `ChatHistoryStore` trait 整体改为 `#[async_trait]`，四个方法全部 `async`，
两个实现（`InMemoryStore` / `ChatMessageStore`）直接 `await` 底层 repo。由于 `driver_loop`
是单 task 串行消费命令，写操作现在都是「等真正落库完成才返回」，执行顺序由 driver 串行
执行天然保证，竞态消除。

> 配套：此前已移除 `ChatHistoryStore::rollback_to`（trait 声明 + `InMemoryStore` /
> `ChatMessageStore` 两个实现）及 `History` 里仅服务于回滚的
> `rollback_to_store_id` / `pop_last_assistant_tool_calls_if_not_first` /
> `find_idx_by_store_id` / `leading_system_count` 四个死方法，并同步修正 `event.rs`
> 的 `HistoryUpdated` 注释。

**连锁改动（async 化波及面）**：
- `History`：`new`（含 `load`）/ `push_user` / `push_assistant` / `push_tool` /
  `push_cancelled_tool` / `clear` 全部改 async；`push_cancelled_tool` 的「先查已闭合 →
  append → push」拆成两段短锁，避免 `Mutex` guard 跨 `await`。
- `ChatService`：`new` / `from_ai_client` / `with_store` / `clear` 改 async
  （`resolve_ai_client` 保持同步）。
- driver：`close.rs` 四个 close 函数、`finish_send` 改 async；全部 `push_*` / `clear` /
  `close_*` 调用点补 `.await`。
- 调用方：`sub_agent/runner.rs`、`chat_service_factory.rs::build_for_session`（改 async）、
  `controller.rs`、`tests.rs`（`make_service` 改 async，17 处调用点 + `clear` 补 `.await`）。

**遗留（可选，未在本轮处理）**：
- `update` 在 `History` 中从未被调用（与 `rollback_to` 同属「trait 定义了、但未接线」的死代码），
  可另行评估移除或接线。

---

### 隐患 3：`append` 每次全表查询算序号（O(n²)）

**位置**：`chat_flexible_message_storage.rs::append`

**现象**：每次插入一条消息，都先 `find_by_plan_and_session` 拉出该会话的**全部**消息，
再用 `rows.last().map(|r| r.sequence_order + 1)` 计算下一条序号。

**影响**：聊天历史越长，每次发送越慢（O(n²) 累积）。长会话下尤其明显。

**建议**：把「取下一序号」下推到 repo 层（如 `ChatMessageRepo` 提供原子
`next_sequence(session)`，或 `INSERT` 时用 `MAX(sequence_order)+1` 的 SQL），
避免每次全表读取。序号应原子、单调递增，保证并发/回滚后不重号。

---

### 隐患 4：`block_in_place` 在 GUI 运行时上的兼容性风险 — ✅ 已解决

**位置**：原 `chat_flexible_message_storage.rs::load`（经 `History::new` → `with_store`
构造时同步触发）

**现象**：`load` 曾在 `ChatService::with_store` 构造（`History::new`）里同步调用，
而构造发生在 dioxus 的 `spawn(async …)` 任务中。

**影响**：`tokio::task::block_in_place` 依赖 multi-thread runtime 的 blocking 线程池。
若 dioxus 跑在 `current_thread` runtime、或当前线程不在 blocking 池，会 **panic**。

**处理方式**：随隐患 2 的 async 化一并消除——`ChatHistoryStore` 四个方法改为 `async`，
存储 IO 全程 `await`，不再有任何 `block_in_place` / `block_on` 阻塞，运行时兼容性风险消失。

---

### 隐患 5：`step5_callback` 异步写库乱序（隐患）

**位置**：`crates/agent-gui/src/pages/plan/flexible/step5_callback.rs::on_result`

**现象**：`on_result` 里 `tokio::spawn` 异步调 `save_snapshot`，随后立刻返回
`ResultDecision::Accept`。同一 session 内若多次触发 step5 定稿（同一会话反复改需求），
多次 `spawn` 的 `UPDATE` 覆盖会**乱序**。

**影响**：最终落库的快照可能不是最新一次产出（旧覆盖新），版本语义被破坏。

**建议**：落库不 `spawn` 到无界并行，改为串行（如内部队列 / 按 session 串行化），
或带版本号/序号校验，丢弃过期写入。

---

## 二、解耦 / 架构隐患

### 解耦 6：`SessionSlot` 会话上下文传递 —— ✅ 已评估：现状合理，无需改动

**位置**：
- `crates/agent-gui/src/pages/plan/flexible/session_slot.rs`
- `step5_callback.rs`、`flexible_state_tool.rs` 经 `watch::Receiver<Option<String>>` 读当前会话

**结论**：`SessionSlot` 是「当前活跃会话 id」的**上下文传递机制**（controller 级，与
`ChatService` 一对一，**非进程级全局单例**）。它把 `session_id` 隔离在 `ChatService` 之外：
chat 服务端核心逻辑（driver / round / sub_agent）完全不感知 `session_id`，仅在「落库侧」
（`step5_callback`、`flexible_state_tool`）通过 watch 的 latest-value 语义消费。

**为什么不用「显式传参」替代**：`session_id` 是 UI / 会话层概念。若改为「注册随 session
重建 + 显式传 `session_id`」，反而会让 session 概念渗透进 `register_sub_agent` /
`ChatConfig` / `SubAgentRunner` / `ChatService`，污染 chat 服务端抽象。现状通过独立 watch
槽把 `session_id` 挡在 `ChatService` 门外，正是解耦的体现。

**边界（依赖既有约束，不构成 bug）**：单页面同一时刻只有一个活跃协调器（`switch_session`
先停旧），故无「多会话串台」风险；`session_id` 语义单一（即「当前 session」），可维护性
负担小。

---

### 解耦 7：`register_sub_agent` 五处高度重复

**位置**：`crates/agent-gui/src/pages/plan/flexible/controller.rs`（step1~step5 注册段）

**现象**：step1~step5 的注册代码每个约 30 行，只有 `name / description / schema / prompt /
allowed_tools / callback` 不同，其余结构完全一致。

**建议**：抽成表驱动 + 一个 `register_step(agent_name, prompt, schema, allowed_tools, callback)`
辅助函数，减少 ~150 行重复，降低漏改风险。

---

### 解耦 8：`driver_loop` 里的 `Command::Confirm` 死分支

**位置**：`crates/planned-agent/src/chat/driver/mod.rs`（`Command::Confirm` 分支）与
`driver/confirm.rs::await_confirm`

**现象**：`confirm_user_action` 实际上被 `await_confirm` 在 `rx.recv()` 里直接消费，
`driver_loop` 中 `Command::Confirm` 分支（emit Error）几乎永远不会走到。

**影响**：确认动作存在「两条路径」的隐式语义，代码绕、易误读。

**建议**：澄清或移除该兜底分支，让「确认」只有一条消费路径。

---

## 三、优化方向（可选）

- **存储层统一为异步 + 内部串行**：让 `ChatHistoryStore` 的写操作走同一个内部队列，
  消除 `append` / `rollback` 的执行模型分裂（顺带解决隐患 2、3、4）。
- **序号自增下推到 repo**：由 `ChatMessageRepo` 提供原子 `next_sequence(session)`，
  避免全表查询（隐患 3）。
- **写库顺序保证**：`step5_callback` 落库不 `spawn` 到无界并行，改为串行或带版本校验，
  避免乱序覆盖（隐患 5）。

---

## 四、关联文档索引

- `crates/planned-agent/src/chat/README.md` — chat 服务端架构与数据流
- `crates/agent-gui/src/components/chat/RENDER_FLOW.md` — 渲染层数据流
- `crates/agent-gui/src/pages/plan/flexible/流程分析.md` — 5-step 灵活模式流程
- `crates/agent-gui/src/pages/plan/flexible/架构改造方案_session版本会话.md` — 会话/版本改造基线（含「待做」清单）
- `docs/stop-cancel-propagation.md` — 取消传播设计（关联隐患 2 的取消/回滚时序）

---

## 五、待确认

1. 隐患 1 是「还原生产配置」还是「保留测试分支、另起测试入口」。
2. 解耦 6（`SessionSlot`）已评估为「现状合理、无需改动」，不纳入本轮。
3. 存储层改造（隐患 2/3/4）涉及 `ChatHistoryStore` trait 签名，需要同步改
   `InMemoryStore` 与 `ChatMessageStore` 两个实现及所有调用点，是否接受此范围的改动。
