# 父 agent 停止 → 级联取消正在运行的子 agent（设计稿 v1，供审阅）

> 状态：**仅设计，未改任何代码**。审阅通过后再实现。
> 涉及 crate：`planned-agent`、`tool-manager`。目标：UI 点"终止"能**立刻**中断正在流式输出的子 agent，并级联收口整棵父子 agent 树。

---

## 1. 背景与问题

当主 agent（父）调用"子 agent"工具时，整个子 agent 运行在**独立的一个 `ChatService`**（`planned-agent/sub_agent/runner.rs`）里：

```
父 driver ──(阻塞)──> execute_backend_tool_call
                        └─ call_tool_streamed(stream)
                            └─ SubAgentExecutor.execute_streamed
                                └─ SubAgentRunner::start  → ChatService::new（独立 State / cancelled）
                                       └─ collect_until_outcome：ticket.wait_outcome().await 阻塞
```

根因（已核实）：

- `ChatService::stop()`（`service.rs:209`）只会设置**父 State 自己**的 `cancelled`，再 `clear_sub_agent_sessions()` 清掉**挂起（resume 阻塞）的会话**。
- 子 agent **正在流式生成**（未挂起）时，父 driver 完全阻塞在 `execute_streamed` 内部，根本读不到自己的 `cancelled`；子 agent 的 `cancelled` 是**另一个独立实例**，父的 stop 触达不到。
- 因此点击终止"无反应"，直到子 agent 自己挂起（UI action）时才会因 `clear_sub_agent_sessions` 被中断 —— 与用户观察一致。

**可用结论（已核实）**：只要把父的取消**传到子 agent 的 `State`**，使子 agent 进入正常收尾路径（`driver/mod.rs:121-123` 会给阻塞中的父回发 `Completed`），父随即读到自己的 `cancelled` 而整树收口。LLM chunk/轮次循环里已有每 chunk 检查 `state.cancelled`（`round/mod.rs:278`）。

---

## 2. 设计核心：每层一个"本地可观察取消 watch"，向下游以 `ToolStreamSender` 携带

三层关键思路：

1. **每层 `State` 自带一个本地取消 watch**（`watch::Sender<bool>`），任何取消来源（本层 `stop()` **或** 上游取消）都统一走 `mark_cancelled()`：置原子 `cancelled` + `watch.send(true)`。既保留现有全部 `cancelled.load()` 检查点语义，又让取消变成**可等待、可订阅**的信号。
2. **下游子 agent 的"上游取消" = 父的本地 watch 接收端**，经**父子间唯一的桥 `ToolStreamSender`** 传递（它在 `create_stream → call_tool_streamed → execute_streamed → runner.start` 全链路里是同一个 `Clone` 对象，**无需改任何公共 trait 签名**）。
3. **驱动 driver 的所有"阻塞式工具 await"都要竞速"本层有效取消"**（`tokio::select!`）。这一步是根治关键——否则若中间某层自己也阻塞在等孙 agent，纯"每 chunk 轮询"会卡死在该层。

### 为何必须有第 3 点（递归正确性的关键）

假设父→子→孙三层。父取消后，孙的"上游"= 子的本地 watch；但**子当前正阻塞在 `execute_streamed` 等孙返回**，子的 driver 轮不到检查自己的取消，子的本地 watch 永远不会翻 → 孙永远等不到。所以每层等待子工具的那个 `.await` 必须**自己**能感知"我被取消了（含上游）"并提前返回 —— 这正是 `confirm.rs` 里主 agent 等确认时 100ms 轮询能停的原因。第 3 点把它推广到"等任何子工具"。

---

## 3. 需修改的文件与改动点

### 3.1 `crates/planned-agent/src/chat/state/state.rs`（`State`）
- 在 `cancelled: Arc<AtomicBool>` 之外新增：
  - `cancel_tx: tokio::sync::watch::Sender<bool>`（本地取消 watch，初始 `false`）；
  - `upstream_cancel: Mutex<Option<watch::Receiver<bool>>>`（当本 service 是被某父创建的**子 agent** 时，持有上游接收端；主 agent 为 `None`）。
- 新增方法（内部实现 `mark_cancelled` 统一置位，保持现有检查点不变）：
  - `pub fn mark_cancelled(&self)`：`cancelled.store(true)` + `cancel_tx.send(true)`；
  - `pub fn cancel_rx(&self) -> watch::Receiver<bool>`：`cancel_tx.subscribe()`（用于**交给下游**）；
  - `pub fn attach_upstream(&self, rx: watch::Receiver<bool>)`：由子 agent 构造后调用；
  - `pub fn cancel_signal(&self)`：可等待的取消 future —— `select` 本层 `cancel_rx.changed()` 与 `upstream.changed()`，任一触发即 `mark_cancelled()` 后返回（供 driver 的 `select!` 使用）。
- 说明：依赖 tokio 自带 `watch`，**不引入新第三方依赖**。

### 3.2 `crates/planned-agent/src/chat/service/service.rs`（`ChatService`）
- 所有 `State { ... }` 构造点（`from_ai_client` / `with_store`，第 73-85、98-110 行）：初始化 `cancel_tx = watch::channel(false).0`、`upstream_cancel = None`。
- `stop()`（第 209 行）：把 `cancelled.store(true)` 改为调用 `state.mark_cancelled()`，**保留** `clear_sub_agent_sessions()`（仍负责挂起会话）。
- 新增 `pub(crate) fn attach_upstream(&self, rx)` 透传，供 runner 使用。
- `State` 自身需确保 `cancel_tx` 不阻碍 `Drop`：watch 随 `State` 一起释放，无泄漏（不 spawn 额外监听 task，见 §4）。

### 3.3 `crates/tool-manager/src/sub_agent/stream.rs`（`ToolStreamSender`）
- 字段新增：`upstream_cancel: Option<watch::Receiver<bool>>`（默认 `None`）。
- `new()` / `disabled()` 构造默认置 `None`（**保持既有调用兼容**）；新增 `pub fn with_upstream(mut self, rx) -> Self`。
- `Clone` 派生已存在，receiver 随 clone 传递（`watch::Receiver` 实现 `Clone`）。
- 另提供 `pub fn upstream(&self) -> Option<watch::Receiver<bool>>` 供 runner 取出。

> 注意：`tool-manager` 需能引用 `tokio::sync::watch` —— 该 crate 已依赖 tokio（用到 `mpsc`/`oneshot`），直接使用即可。

### 3.4 `crates/planned-agent/src/chat/driver/bridge.rs`（`SubAgentBridge::create_stream`）
- 构造 `ToolStreamSender` 处：`.with_upstream(self.state.cancel_rx())` —— 把**本层（父）** 本地取消 watch 交给即将创建的子 agent。
- 桥持父 `State`，天然取得到；此处不加下游字段的扩散。

### 3.5 `crates/planned-agent/src/chat/sub_agent/runner.rs`（`SubAgentRunner::start`）
- 创建子 `ChatService` 后，调用 `child_service.attach_upstream(stream.upstream().unwrap_or_default())`（或显式分支处理 `None`）。
- 这样**子 agent 的有效取消自动包含"父被取消"**，且父被 stop 后子 driver 的 await/检查会立即感知。

### 3.6 `crates/planned-agent/src/chat/driver/round/mod.rs`（及 `handlers.rs`）—— 关键第 3 点
- 父 driver 中**阻塞等待子工具结果**的 await（`call_tool_streamed(...).await`，handlers.rs:124-131 / round 循环）改为 `tokio::select!`：
  - 分支 A：`result = state.tool_registry.call_tool_streamed(...)` → 正常返回；
  - 分支 B：`state.cancel_signal()` → 本层被取消（含上游传导），走关闭收尾。
- 取消分支沿用现有 `close_unclosed_tool_calls` / 结束路径，最终在 `driver/mod.rs` 收尾向阻塞者发 `Completed`（已被取消的语义）。`main_agent` 与子 agent **共用同一份 `round/mod.rs` 代码**，故天然对每层（含孙）递归生效，无需额外分叉逻辑。
- 已有的 LLM 每 chunk 检查保留（`round/mod.rs:278`），它负责"等待 LLM 生成期间"的取消；`select!` 负责"等待子工具/孙 agent 期间"的取消 —— 两者覆盖 driver 的两类阻塞点。

### 3.7 不修改
- `ai-manager` / `ai-openai` LLM 请求本身：取消落在**下一 chunk 边界**或**子工具返回点**（相对及时）。若日后要"真正立刻掐断正在进行的一次 LLM 网络请求"，需在 AI 层加 abort —— 本期不做，单列为后续项。

---

## 4. 生命周期 / 无泄漏论证

- 采用"每层本地 watch + driver `select!` 竞速"而非"spawn 一个父→子监听 task"，因此**没有**游离的转发 task 需要清理，`State` 释放即全部回收。
- `watch::Receiver` 由 `ToolStreamSender`（父桥临时对象）与子 `State` 持有；子 `ChatService` 结束后父侧 stream 一并 drop，无跨层长生命周期句柄泄漏。
- 子 agent 正常完成后，父 `call_tool_streamed` 返回，桥对象销毁，sub 关联自动解除 —— 不会影响之后再次调用子 agent。

---

## 5. 边界与权衡

| 场景 | 行为 | 说明 |
|---|---|---|
| 主 agent 等子 agent 流式输出时点终止 | 立即取消，整树收口 | 本次目标 |
| 子 agent 再套孙 agent（任意深度） | 递归逐层 `select!` + watch 传导，整树停 | §3.6 共用代码保证 |
| 子 agent 挂在 UI action（resume 阻塞） | 既有 `clear_sub_agent_sessions` 中断 + 新增传导双保险 | stop() 保留原逻辑 |
| 取消发生在 LLM 单次长请求中途（暂无 token 回流） | 在下一 chunk 到达后停 | 接受；非秒掐断 |
| 取消后再次使用同一主 service 发消息 | 正常；新轮次按需重新 attach 上游 | watch 每 service 各自独立 |

---

## 6. 需要实现的代码清单（汇总）

1. `crates/planned-agent/src/chat/state/state.rs`
2. `crates/planned-agent/src/chat/service/service.rs`
3. `crates/tool-manager/src/sub_agent/stream.rs`
4. `crates/planned-agent/src/chat/driver/bridge.rs`
5. `crates/planned-agent/src/chat/sub_agent/runner.rs`
6. `crates/planned-agent/src/chat/driver/round/mod.rs`（及 `handlers.rs`）

---

## 7. 测试计划（实现后补充执行）

1. **单层**：主 agent 流式生成时点终止 —— 应停止（现有行为回归，不应因改动退化）。
2. **核心**：主 agent 调子 agent，子 agent 流式输出中，GUI 点终止 —— 子 agent 立刻停、父 driver 收到取消并收口、GUI 按钮恢复正常。
3. **递归**：父→子→孙，孙流式输出中点终止 —— 三层全部取消。
4. **挂起回归**：子 agent 停在 UI action 时点终止 —— 仍能停（`clear_sub_agent_sessions` + 新传导）。
5. **正常结束回归**：子 agent 自然完成 —— 不受新字段影响，父能正常拿结果。
6. 编译：`cargo check -p planned-agent-gui`、`cargo check -p planned-agent`、`cargo check -p tool-manager`。
