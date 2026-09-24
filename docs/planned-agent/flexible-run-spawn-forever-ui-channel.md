# 灵活计划执行：执行载体（`spawn_forever`）与 UI 通信

> 主题：用 dioxus 长驻 Future `spawn_forever` 承载 `FlexibleExecutor`；UI 与执行体之间的通信通道；执行状态的归属与生命周期
> 状态：**需求与设计待确认**（本文定稿后才动代码）
> 本轮范围：**执行载体 + UI ↔ 执行 通信 + 状态归属（root 重建不失联）**
> 明确移出本轮：SQLite 持久化 / 跨重启状态恢复（见 §8）
> 涉及 crate：`crates/agent-gui`（宿主侧全部改动）、`crates/planned-agent`（**零改动**）
> 关联文档：`docs/planned-agent/flexible-executor.md`（执行器内核）、`docs/planned-agent/plan-storage-design.md`（存储设计，本轮不用）
> 前身：`flexible-run-std-thread-sqlite.md`（`std::thread` 与 SQLite 两条线均已撤回，文件名随之更新）

---

# 第一部分 · 需求整理

## R0 需求来源与处置

| # | 原始诉求 / 指示 | 本轮处置 |
|---|---|---|
| 1 | 「FlexibleExecutor 属于长耗时，使用 `std::thread::spawn` 执行」 | **撤回**。继续用 dioxus 长驻 Future `spawn_forever`（依据见 §2） |
| 2 | 「增加 sqlite 本地持久化，记录执行中 / 失败 / 完毕状态」 | **移出本轮**（§8），先走通「执行 + UI 通信」 |
| 3 | 「通过订阅方式和执行体通信是否可行？如何实现？」 | **本轮核心**（§3） |
| 4 | 「`SyncSignal` 是 `Send + Sync`，可安全共享来更新 Dioxus 响应式」 | 采纳；但**创建方式必须改**：owner 得是 `ScopeId::ROOT`（§2.3） |
| 5 | 「app root 组件重建，`use_signal_sync` / `flexible_run_manager` 会丢失吗？」 | **本轮核心**（§4） |
| 6 | 「先不要实现，给出设计方案文档给我确认」 | 约束 C5 |

## R1 本轮需求条目

| 编号 | 需求 | 现状差距 | 验收标准 |
|---|---|---|---|
| **R1 执行离屏** | 长耗时执行不因切页面 / 组件卸载 / 会话切换而中断 | 已具备（`spawn_forever` 投 ROOT scope） | 执行中切走再切回，任务照常跑完，进度连续 |
| **R2 状态实时** | 执行中 / 失败 / 完毕在界面实时反映，**订阅式**而非轮询 | 已具备（signal 写 → 读的组件重渲染） | PIPELINE / STATS 随步骤推进刷新，无需手动刷新页面 |
| **R3 会话隔离** | 多会话并行时只显示本会话状态 | 已具备（`HashMap<session_id, RunState>`） | 两会话并行执行互不串台 |
| **R6 通信通道** | 执行体 → UI 用订阅；UI → 执行体用取消通道 | 已具备，但**跨 scope 归属错**，且所有权挂在 `app()` 上 | 两向通道都有单一、明确的载体与归属（§3） |
| **R7 生命周期健壮** | `app()` 重建后：状态不失联、停止不失效、同一会话不会被重复启动 | **缺失**：signal owner 是 `app()` 的 scope，重建即换一份全新空 signal | `app()` 重建后仍读到同一份状态、停止按钮仍有效 |
| **R4 / R5（跨重启可查 / 记录内容）** | 进程重启后能看到上次结果、未跑完显示「已中断」 | 完全缺失 | **移出本轮**（§8） |

## R2 约束（C）与非目标（NG）

**约束**
- **C1** 不改 `crates/planned-agent`：执行器、事件、报告保持原样。
- **C2** 不引入新依赖（本轮不新增 crate，不新增表）。
- **C3** 状态更新失败不得影响执行本身（写状态是旁路，不是执行的一环）。
- **C4** 不改变 UI 交互与样式（按钮、布局、参数编辑都不动）。
- **C5** 本文档定稿后才动代码。

**非目标（本轮）**
- **NG1** 不做持久化、不做执行历史回看、不做跨重启恢复。
- **NG2** 不做多窗口 / 多进程状态同步。
- **NG3** 不改 `planned-agent` 的事件语义（例如 `Failed` 仍是「单步失败」）。
- **NG4** 不引入 WebSocket / 事件总线 / 消息队列等新机制。
- **NG5** 不把执行搬到别的线程（本轮明确不做，理由与后续升级路径见 §2.4）。

## R3 现状差距（代码事实）

| # | 事实 | 位置 |
|---|---|---|
| G1 | 执行任务已用 `spawn_forever` 投到 ROOT scope（**载体本身不用改**） | `crates/agent-gui/src/services/flexible_run_manager.rs:275-282`（事件消费）、`:297-311`（执行器） |
| G2 | 状态 signal 与取消表由 `app()` 用 `use_signal_sync` 创建后经 context 注入 | `crates/agent-gui/src/main.rs:106-114`、`src/context/mod.rs:36-40` |
| G3 | 因此 signal 的 owner 是 `ScopeId::APP`(3)，而使用它的任务 current scope 是 `ScopeId::ROOT`(0) → 触发 `dioxus_signals::warnings::__copy_value_hoisted` | 成因链见 §2.2 |
| G4 | 左栏直接读注入进来的 signal（读即订阅） | `src/pages/plan/left_panel/left_panel.rs:220-224`、`:234-236` |
| G5 | 两向通道其实已在：执行 → UI 走 `mpsc` + 消费 Future 并入 signal；UI → 执行走 `watch::Sender<bool>`；但**两者都随 `app()` scope 存亡** | `flexible_run_manager.rs:265-282`、`:319-323` |
| G6 | `RunState.records` 只写未读（预留实时指标） | `flexible_run_manager.rs:94-99` |
| G7 | 无任何执行记录表 / 启动对账 | `src/storage/`（本轮不补，见 §8） |

## R4 开放问题

逐条见 **§9**（已拍板 3 项 + 剩余 4 项待确认）。

---

# 第二部分 · 落地设计

## 1. 结论先行

| # | 判定 | 结论 |
|---|---|---|
| 1 | 执行载体 | **继续 `spawn_forever`**（现状即如此）。它是 dioxus 里唯一「任务不随组件卸载取消」的官方机制，且执行链全程 async 无需额外线程（§2） |
| 2 | 执行 → UI | **`SyncSignal` 订阅式推送**，但 signal 的 **owner 必须是 `ScopeId::ROOT`**，不能再用 `use_signal_sync` 的默认 owner（§2.3、§3.1） |
| 3 | UI → 执行 | **`watch::Sender<bool>` 取消信号**，从「存 signal 里」改为「存进程级注册表」（§3.2） |
| 4 | 归属（R7 的解法） | **进程级 `static OnceLock<RunRegistry>`**：`app()` 重建也不换实例，状态不失联（§4） |
| 5 | 执行体阻塞红线 | future 在 **GUI 主线程**被 poll，执行链里严禁同步阻塞（§2.4） |

## 2. 执行载体：继续用 `spawn_forever`

### 2.1 三个必须先确认的源码事实（dioxus 0.7.9）

**(a) `spawn_forever` 的任务挂在 ROOT scope，ROOT 常驻。**

```rust
// dioxus-core/src/global_context.rs:199-201
pub fn spawn_forever(fut: impl Future<Output = ()> + 'static) -> Task {
    Runtime::with_scope(ScopeId::ROOT, |cx| cx.spawn(fut))
}
```

`ScopeId::ROOT = ScopeId(0)`，是 `VirtualDom` 构造时创建的第一个 scope（`dioxus-core/src/virtual_dom.rs:319-325`），官方注释称它「**always be around**」（同文件 `:340-345` 的 `base_scope()`）。它只随 `VirtualDom` 一起消失，**不随任何组件卸载**。

**(b) 任务的 Future 在 GUI 主线程被 poll，不在别的线程。**

```text
Tao 事件循环（主线程）
  └─ Event::UserEvent(UserWindowEvent::Poll(id)) => app.poll_vdom(id)   // dioxus-desktop/src/launch.rs:40
       └─ WebviewInstance::poll_vdom()                                   // dioxus-desktop/src/webview.rs:537
            ├─ self.dom.wait_for_work()          // 处理 SchedulerMsg，跑任务   :572
            └─ self.dom.render_immediate(...)    // 应用变更                   :587
                 └─ Runtime::handle_task_wakeup → Future::poll
```

轮询任务时先把它自己的 scope 压栈：

```rust
// dioxus-core/src/tasks.rs:285-288（LocalTask 的 scope 见 :344-351）
let poll_result = self.with_scope_on_stack(task.scope, || {
    self.current_task.set(Some(id));
    let poll_result = task.task.borrow_mut().as_mut().poll(&mut cx);
```

dioxus-desktop 的 waker 注释把这件事说得很直白：

```rust
// dioxus-desktop/src/waker.rs:10-11
/// All IO and multithreading lives on other threads. Thanks to tokio's work stealing approach, the main thread can never
/// claim a task while it's blocked by the event loop.
```

**(c) 在 Future 体内，`Runtime::try_current_scope_id()` 返回 `Some(ScopeId::ROOT)`。**

由 (a) + (b) 直接推出：`task.scope == ROOT`，poll 前压栈，而 `try_current_scope_id()` 就是取栈顶（`dioxus-core/src/runtime.rs:227-229`）。

> ⚠️ 这一条**推翻了**上一版文档（std::thread 版）的说法。上一版说「线程里 `try_current()` 返回 `None`，检查不执行」——那对 `std::thread` 成立，但对 `spawn_forever` **不成立**：检查会执行，且当前 scope 是 ROOT。

### 2.2 `__copy_value_hoisted` 的真实成因与唯一正确修法

警告的判定逻辑（`dioxus-signals-0.7.9/src/warnings.rs:11-20`）：

```rust
let origin_scope = value.origin_scope;                 // 信号创建时归属的 scope
let Some(rt) = dioxus_core::Runtime::try_current() else { return; };
if let Some(current_scope) = rt.try_current_scope_id() {
    // 当前 scope 就是 owner，或者是 owner 的后代 → 放行
    if origin_scope == current_scope || rt.is_descendant_of(current_scope, origin_scope) {
        return;
    }
    // 否则告警：值可能在 owner 被 drop 后仍被使用
```

代入现状：`origin_scope = APP(3)`（`app()` 里 `use_signal_sync` 创建），`current_scope = ROOT(0)`（执行任务的 scope）。`ROOT` 是 `APP` 的**祖先**，不是后代 → 告警。

| 修法 | 做法 | 判定 | 取舍 |
|---|---|---|---|
| A. 搬执行体（上一版方案） | 用 `std::thread::spawn` 跑执行器 | 线程里无 runtime → `try_current()` 返回 `None` → 第 12 行直接 return，不告警 | **撤回**：执行链本来就是 async，搬到裸线程要自建 runtime；且白丢 dioxus 的任务跟踪与取消语义；跨线程唤醒 UI 反而多一层 |
| B. 改 owner（**采纳**） | 显式用 `Signal::new_maybe_sync_in_scope(value, ScopeId::ROOT)` 创建 | `origin_scope == current_scope == ROOT` → 第一个分支就 return，不告警 | 改动一行 API 调用；顺带解决 R7（§4） |

B 的合法性：`new_maybe_sync_in_scope` 只做 `Runtime::current().scope_owner(scope)`（`dioxus-signals/src/copy_value.rs:108-124`），唯一要求是该 scope 已存在——ROOT 在 `VirtualDom` 构造时就有，不会 panic；对「当前 scope 是谁」没有任何额外约束，所以在 `APP` 的渲染函数里指定 `ROOT` owner 完全合法。

补充两点：
- 界面组件读同一个 signal **不**会告警：组件 scope 是 `ROOT` 的后代，走 `is_descendant_of` 分支（`warnings.rs:18`）。
- 该警告只在 **debug 构建**打印（`warnings` crate 的 `if_enabled` 带 `#[cfg(debug_assertions)]`），release 下静默——所以「没看到警告」不等于写法正确。

### 2.3 owner 定成 ROOT 之后，语义上更顺

- 执行体（ROOT scope 的 future）写状态 → 与 owner 同 scope，**这是警告系统期待的方向**（下层写上层持有）。
- 组件（ROOT 的后代）读状态 → 合法订阅。
- ROOT 常驻 → 状态天生比任何组件活得久，`app()` 重建后 handle 依然有效（§4）。

### 2.4 代价：GUI 主线程不得阻塞（本轮必须遵守的红线）

Future 在主线程 poll，意味着**执行链里的任何同步阻塞都会卡住整个界面**。区分清楚两类：

| 环节 | 现状 | 判断 |
|---|---|---|
| AI 调用 `AiClient::chat_completion` | async（`crates/core/src/ai/traits.rs:7`，`Send + Sync` 的对象安全 trait） | ✅ 等待期间只是 `Pending`，不占主线程 |
| 工具调用 `ToolRegistry::call_tool` | `async fn`（`crates/tool-manager/src/core/registry.rs:598`） | ✅ 外层是 async |
| **工具实现内部** | 由各 executor 决定（可能有同步文件 IO、同步等待外部进程） | ⚠️ **需要逐个核查**；真有阻塞就得在 executor 内用 `spawn_blocking` 隔离 |
| 事件归并 / 写 signal | 纯内存操作 | ✅ |

**本轮处理方式**：不预先改造工具实现，只在验证清单里加一条「执行期间界面是否可交互」的手工检查（§7），并把「工具内阻塞隔离」单列为后续议题。

**若将来确实需要真离屏**（`tokio::spawn` 到 tokio worker 线程），本身是可行的：执行链的对象都是 `Send + Sync`（`AiClient: Send + Sync`、`PlanRunSink: Send + Sync`，见 `crates/planned-agent/src/flexible/event.rs:50`），写 `SyncSignal` 在无 dioxus runtime 的线程里也合法（`update_subscribers` 只走 `SchedulerMsg` 通道，见 §3.1）。但那是另一轮的事，本轮按指示继续 `spawn_forever`。

## 3. UI ↔ 执行 的通信（本次核心）

### 3.0 三条通道总览

| # | 方向 | 载体 | 用途 | 关键依据 |
|---|---|---|---|---|
| ① | 执行体 → UI | `SyncSignal<HashMap<String, RunState>>`（owner = `ScopeId::ROOT`） | 实时进度 / 相位 / 终态；**订阅式**，写即唤醒界面 | `dioxus-signals/src/signal.rs:22`、`:205-226` |
| ② | UI → 执行体 | `tokio::sync::watch::Sender<bool>`（存注册表） | 用户点「停止」 | 现有约定，见 `flexible_run_manager.rs:319-323` |
| ③ | 执行体 → 状态归并 | `tokio::sync::mpsc::UnboundedSender<PlanRunEvent>` → 消费 Future | 把同步回调 `emit` 出来的事件并入 ① | `flexible_run_manager.rs:265-282`、`event.rs:50-54` |

一句话概括：**UI 只跟 signal 打交道（读），执行体只往 signal 写（+ 读取消），二者之间没有直接引用**——注册表是它们唯一的交汇点。

### 3.1 通道 ①：执行体 → UI，`SyncSignal` 订阅式推送

**类型与创建**（进程级，只创建一次，owner 指定 ROOT）：

```rust
// 仓库现状统一入口就是 `use dioxus::prelude::*;`：
// `ScopeId` 见 dioxus-0.7.9/src/lib.rs:245，`SyncStorage` 由 dioxus-signals 从 generational_box 再导出
// （dioxus-signals/src/lib.rs:30-31）。
use dioxus::prelude::*;
use std::collections::HashMap;

// 只能在「有 dioxus runtime」的上下文里执行（组件渲染期）；进程内只跑一次。
let states: Signal<HashMap<String, RunState>, SyncStorage> =
    Signal::new_maybe_sync_in_scope(HashMap::new(), ScopeId::ROOT);
```

`SyncSignal<T>` 就是 `Signal<T, SyncStorage>` 的别名（`dioxus-signals/src/signal.rs:22`），可替换写法。

**写（执行体内）**：

```rust
let mut all = states.write_unchecked();
if let Some(state) = all.get_mut(&session_id) { state.apply(event); }
// guard 在此 drop → 此刻才通知订阅者
```

两个必须照做的点：

1. **用 `write_unchecked()` 而不是 `write()`**。`write()` 的借用守卫带生命周期检查，跨线程场景下既无必要也不便（`write_unchecked` 是 `&self`，不要求 `&mut`，见 `dioxus-signals/src/write.rs:287`；底层 `try_write_unchecked` 在 `:47`）。
2. **守卫不得跨 `.await` 持有**。通知订阅者发生在守卫 **drop** 时（`WriteLock` 的 metadata `D` 就是干这个的，注释在 `dioxus-signals/src/write.rs:163`），跨 await 持有会长时间占住借用并让界面停更。

**读（组件内）**：

```rust
let run_state = session_id
    .as_ref()
    .and_then(|sid| run_mgr.states().read().get(sid).cloned());
```

读即订阅：写者改值 → 该组件被标记为 dirty → 重渲染。**不需要轮询、不需要定时器**。

**唤醒链路（为什么跨线程写也能唤醒界面）**：

```text
states.write_unchecked() 的值变更
  └─ guard drop → Signal::update_subscribers()                  // dioxus-signals/src/signal.rs:255-269
       └─ 对每个订阅者 ReactiveContext::mark_dirty()
            └─ ReactiveContext 里存的 update 闭包：             // dioxus-core/src/reactive_context.rs:103-108
                 sender.unbounded_send(SchedulerMsg::Immediate(scope_id))
                      └─ 主线程事件循环 wait_for_work 收到 → 调度该 scope 重渲染
```

关键点：`ReactiveContext` 持有的是构造时存下的 `runtime.sender`（`UnboundedSender<SchedulerMsg>`），**不依赖 `Runtime::current()`**——所以在任何线程写都成立。这是你第 4 条判断的源码依据。

**时序（一次「点执行」的全过程）**：

```text
UI 线程                          执行 Future（ROOT scope）            其他线程
────────────────────────────────────────────────────────────────────────────
点击「执行」
  └─ run_mgr.start(sid, tpl, params)
       ├─ registry.states 插入 RunState::Running     ← 界面立刻从「待执行」变「执行中」
       ├─ 建 watch 通道 + 注册取消句柄
       └─ spawn_forever(消费事件) / spawn_forever(跑执行器)
                                    │
                                    ├─ emit(StepStarted {index}) ──③──▶ 事件队列
                                    │                                    │ 消费 Future: apply() → 写 signal
                                    │                                    └──①──▶ 主线程标记 dirty → PIPELINE 重渲染
                                    ├─ await AI / 工具（IO 在别的线程，主线程只 poll）
                                    └─ emit(RunFinished{report}) + 收尾写 status=Finished
点击「停止」
  └─ registry.cancel(sid) ──sender.send(true)──②──▶ 执行体在步骤边界 / 工具循环判定 *rx.borrow()
```

### 3.2 通道 ②：UI → 执行体，`watch` 取消信号

保留现有机制，只改**归属**（从 `Signal<HashMap<String, watch::Sender<bool>>>` 改为注册表里的普通 `Mutex<HashMap<...>>`）：

- 取消句柄只用于「查表后 `send(true)`」，**不参与渲染订阅**——放 signal 里是没有收益的额外状态；放注册表还能天然跨 `app()` 重建存活。
- 判定必须是**当前值**而非变更事件：

```rust
fn is_cancelled(rx: &watch::Receiver<bool>) -> bool { *rx.borrow() }
```

用 `rx.changed()` 会把「sender 被 drop」也算作一次变更，从而把正常结束误判成取消。

- 语义边界：取消是**协作式**的，执行器在步骤边界与每步的工具循环内检查；`stop` 返回即「已送达请求」，不代表立刻停下。

### 3.3 通道 ③：执行体 → 状态归并，`mpsc` + 消费 Future

`PlanRunSink::emit` 是**同步回调**，而且它的契约写明「实现不应阻塞」（`crates/planned-agent/src/flexible/event.rs:50`：`pub trait PlanRunSink: Send + Sync`）。所以事件先入 `UnboundedSender`，由独立的消费 Future `rx.recv().await` 后并入 ①。

**为什么不让 `Sink` 直接写 signal？** 技术上可以（同 scope、同线程、无 await），保留中间层的三个理由：

1. 执行器与 UI 状态解耦：`planned-agent` 侧只认 `PlanRunSink`，宿主可以换实现而不改执行器；
2. 归并策略（`RunState::apply`）只在一个地方；
3. 将来接持久化 / trace 时，只需在消费者侧多挂一个 sink（`§8` 的回归路径）。

代价是多一次跨任务调度（微秒级），本轮接受。

### 3.4 明确不用这两种做法

| 做法 | 为什么不行 |
|---|---|
| `GlobalSignal` / `Signal::global(...)` | 其全局上下文是 `Rc<RefCell<HashMap<…>>>`（`dioxus-signals/src/global/mod.rs:220`），**不是 `Send`**；`resolve()` 每次都要 `Runtime::current()`（同文件 `:201`、`:287`）。它只适合「跨 await 但仍在 dioxus 主线程」，与我们「进程级、未来可能跨线程」的诉求冲突 |
| 把状态塞进 context 往下传 | context 挂在创建它的 scope 上：`app()` 重建 → context 一起重建 → 又回到「状态失联」（这正是 R7 的现状故障）。context 适合传**依赖**（`ai` / `tools`），不适合传**长期身份状态** |

### 3.5 红线清单（写代码时逐条对）

| 红线 | 原因 |
|---|---|
| signal 的 owner 必须是 `ScopeId::ROOT` | 执行任务 current scope 就是 ROOT（§2.2） |
| 创建只做一次（`OnceLock`） | 重复创建 = 两份状态，界面与后台对不上 |
| `write_unchecked` 的守卫不跨 `.await` | 通知发生在 drop 时，长持有会让界面停更 |
| 每会话同一时刻只有一个写者 | `try_write_unchecked` 的借用冲突会 panic / 丢写 |
| 判定取消用 `*rx.borrow()` | 见 §3.2 |
| 只读不订阅的场景用 `peek()` | `read()` 会检查 runtime，非 UI 上下文（将来定时执行）会 panic |

## 4. 状态归属与生命周期（R7）

### 4.1 现状故障链

signal owner = `app()`（`APP` scope）：

1. `app()` 因 ErrorBoundary reset / Suspense 重挂 / 热重载被重建 → 新 scope、`use_signal_sync` 建一份**全新空 signal**；
2. 后台执行任务仍持旧 signal 的句柄（引用计数让值还活着）——但**没人读它**了；
3. 界面表现：进度「凭空消失」、停止按钮静默失效（`cancels` 也是新表，查不到 sender）、同一会话可能被再次 `start`。

### 4.2 修法：进程级 `RunRegistry`

```rust
/// 进程级唯一：`app()` 重建也复用同一个实例。
static REGISTRY: OnceLock<RunRegistry> = OnceLock::new();

pub struct RunRegistry {
    /// 执行状态：UI 读即订阅，执行体写即唤醒。（owner = ScopeId::ROOT）
    states: SyncSignal<HashMap<String, RunState>>,
    /// 取消句柄：只用于 stop 查表发送，不参与渲染订阅。
    cancels: Mutex<HashMap<String, watch::Sender<bool>>>,
}

impl RunRegistry {
    /// 只能在 dioxus runtime 内、组件渲染期调用（`OnceLock` 只让闭包跑一次）。
    pub fn init() -> &'static RunRegistry {
        REGISTRY.get_or_init(|| RunRegistry {
            states: Signal::new_maybe_sync_in_scope(HashMap::new(), ScopeId::ROOT),
            cancels: Mutex::new(HashMap::new()),
        })
    }
    pub fn states(&self) -> SyncSignal<HashMap<String, RunState>> { self.states }
    pub fn register_cancel(&self, sid: &str, tx: watch::Sender<bool>) { /* … */ }
    pub fn cancel(&self, sid: &str) { /* 查表 → send(true)，缺表即无操作 */ }
    pub fn is_running(&self, sid: &str) -> bool { /* states.peek()，见红线表 */ }
}
```

`FlexibleRunManager` 保留，但降级为「持有 `ai` / `tools` 依赖 + 引用注册表」的门面；`FlexibleRunSignals` 结构体删除。

- `app()` 首帧调用 `RunRegistry::init()`；重建时 `get_or_init` 返回**同一个**实例。
- 组件仍从 context 取 `FlexibleRunManager`（它承载 `ai` / `tools`，是真正的依赖注入），但状态一律从注册表来。

### 4.3 归属表

| 对象 | 谁创建 | 何时创建 | 跨 `app()` 重建 |
|---|---|---|---|
| `RunRegistry`（`states` + `cancels`） | `app()` 首帧 → `OnceLock` | 首次渲染 | **保留** |
| `RunState` 值 | 注册表写入 | `start()` 时 | **保留** |
| 执行任务的 Future | `RunRegistry` / `FlexibleRunManager::start` | 点执行 | **保留**（绑 ROOT scope） |
| `watch` 取消句柄 | `start()` | 点执行 | **保留**（在注册表里） |
| `FlexibleRunManager` | `ReadyShell` 注入 | 启动就绪后 | 重建即换新实例（但里面只有 `ai`/`tools`/注册表引用，**无状态**，换了也无妨） |

### 4.4 已知限制

- `RunRegistry` 里的 signal 归 `VirtualDom` 的 ROOT scope 所有。**进程退出**时随 `VirtualDom` 释放，无影响；若开发态热重载导致整个 `VirtualDom` 重建，旧句柄会失效——届时需要重启应用，本轮接受（与现状等价，不再更差）。
- 本轮不做「同一会话跨进程恢复」，所以进程被杀后残留的状态仅存在于内存，随进程消失（`R4` / `R5` 见 §8）。

## 5. 改动清单（本轮）

| 文件 | 动作 | 要点 |
|---|---|---|
| `crates/agent-gui/src/services/run_registry.rs` | **新增** | `static OnceLock<RunRegistry>`；`states`（owner=ROOT 的 `SyncSignal`）、`cancels`（`Mutex<HashMap>`）；`init()` / `states()` / `register_cancel()` / `cancel()` / `is_running()` |
| `crates/agent-gui/src/services/mod.rs` | 修改 | 注册 `run_registry` 模块 |
| `crates/agent-gui/src/services/flexible_run_manager.rs` | 修改 | 删除 `FlexibleRunSignals`；`new(ai, tools)` 内部从注册表取 `states`；两个 `spawn_forever` 保留；`stop()` 走注册表；模块文档按 §2.2 重写 |
| `crates/agent-gui/src/main.rs` | 修改 | 删除 `run_states` / `run_cancels` 的 `use_signal_sync` 与 `FlexibleRunSignals` provider（`:106-114`）；`app()` 首帧 `RunRegistry::init()`；`ReadyShell` 里 `FlexibleRunManager::new(run_ai, run_tools)`（`:261-280` 简化） |
| `crates/agent-gui/src/context/mod.rs` | 修改 | 删除 `:36-40`「唯一例外：`FlexibleRunSignals` 由 `app()` 注入」的说明 |
| `crates/agent-gui/src/pages/plan/left_panel/left_panel.rs` | 修改 | 仅改注释（读的是注册表的 signal）；`on_run` / `on_stop` / 渲染逻辑不变 |
| `crates/agent-gui/src/boot.rs`、`src/storage/**` | **不动** | 本轮不涉及持久化 |
| `crates/planned-agent/**` | **零改动** | 执行器 / 事件 / 报告不动 |

## 6. 风险与对策

| 风险 | 影响 | 对策 |
|---|---|---|
| `app()` 组件重建 | 状态失联、停止失效、重复启动 | 进程级注册表 + owner=ROOT（§4） |
| future 在主线程 poll | 执行链路若有同步阻塞 → 界面卡住 | §2.4 红线；验证清单加「执行期间界面可交互」；必要时单列「工具内阻塞隔离」议题 |
| 权限/资源导致 `start()` 失败 | 界面无反馈 | 沿现状：失败写进 `RunState.error`，界面从同一处读 |
| 执行 Future panic | 状态永远停在 `Running` | 执行 Future 内包 `catch_unwind`（或 drop guard）落成 `Failed`——见 §9-⑥ |
| 忘改 owner 直接 `use_signal_sync` | 警告只是表象，真问题是生命周期 | 代码注释写明「必须在 ROOT scope 创建」；`RunRegistry` 是唯一创建点 |
| 跨线程写 signal（将来） | 借用冲突 panic | 每会话单写者；真要多线程再评估 |

## 7. 验证计划

`crates/planned-agent` 零改动 → 不新增其测试文件。`agent-gui` 侧（`cargo test -p planned-agent-gui --bins` 或等价命令）：

1. `RunRegistry`：`init()` 幂等（两次调用拿到同一实例）；`register_cancel` / `cancel` 命中与未命中都不 panic。
2. `states` 归属：`start()` 后 `states.get(sid)` 为 `Running`；`is_running` 为真；两会话互不干扰。
3. 状态机：成功 / 失败 / 取消三条路径，`status` 与 `error` 落点正确。
4. 端到端：执行器的端到端测试留在 `crates/planned-agent` 内跑（`cargo test -p planned-agent`）。该 crate 已有测试桩 `FakeAiClient`（`src/flexible/testing.rs:18`）与 `RecordingSink`（`:204`），均为 `pub(crate)`，**`agent-gui` 侧访问不到**，跨 crate 复用需要另开接口——本轮不做。

**手工验证（必须逐条过）**：

- 执行中切到别的页面再切回 → 进度连续、任务未中断；
- 切换会话 → 状态互不串台；
- 点「停止」→ 立即生效（在步骤边界停下），界面回到非执行态；
- 执行期间拖窗口 / 点其它交互 → **界面不卡顿**（§2.4 红线的验收项）；
- `RUST_LOG=warn` 下跑一次执行 → **`__copy_value_hoisted` 不再出现**（debug 构建）；
- 触发 `app()` 重建（ErrorBoundary 复位 / 热重载）→ 状态仍在、停止仍有效、同一会话不会被重复启动。

## 8. 本轮不做（移出项）与回归路径

移出内容：`plan_runs` 表与实体、写库单写者线程、启动对账、`RunStatus::Interrupted`、UI 的「历史终态兜底」、跨重启恢复（原 R4 / R5 / NG1）。

**回归路径**（将来要做时的最小接缝）：

1. 状态机不变，只在 `RunState` 之外多一张表；写入点仍是同样的三处时机（`start()` / 步骤边界 / 终态）；
2. 通道 ③ 的消费 Future 是多路分发点：**加一个持久化 sink 即可**，执行器与 UI 都不需要改；
3. `RunStatus::Interrupted` 与启动对账随持久化一起做。

也就是：本轮把「执行 + UI 通信」这条线做干净，持久化是挂在 ③ 上的一条旁路，不影响本轮任何结论。

## 9. 待确认（已拍板 3 项 + 余下 4 项）

**已拍板**

| # | 问题 | 决定 |
|---|---|---|
| ① | 执行载体 | 保持现状：每次 `start()` 起一对 `spawn_forever` Future（事件消费 + 执行），不做常驻 dispatcher |
| ② | 状态归属 | 进程级 `OnceLock<RunRegistry>`，signal 用 `Signal::new_maybe_sync_in_scope(_, ScopeId::ROOT)` 创建 |
| ③ | 文档文件 | 由 `flexible-run-std-thread-sqlite.md` 更名为 `flexible-run-spawn-forever-ui-channel.md` |

**余下待你确认（都有推荐）**

| # | 问题 | 推荐 |
|---|---|---|
| ④ | 通道 ③ 是否保留中间的 `mpsc` + 消费 Future（对比：`Sink` 直接写 signal） | **保留**（解耦 + 持久化的接缝，§3.3） |
| ⑤ | 取消句柄存放：注册表 `Mutex<HashMap>` vs 继续放在 `Signal` 里 | **`Mutex<HashMap>`**（不参与渲染订阅，且跨重建天然存活，§3.2） |
| ⑥ | 是否本轮就加 `catch_unwind`，把执行 Future 的 panic 落成 `Failed` | **加**（几行代码，消掉「永远执行中」的一个死角，§6） |
| ⑦ | §2.4 工具内阻塞的核查深度 | **本轮只做手工观察**，发现问题再单列「阻塞隔离」议题 |

---

## 附录 A：dioxus 0.7.9 源码索引（本轮逐条实测）

| 位置 | 内容 | 用途 |
|---|---|---|
| `dioxus-core/src/global_context.rs:199-201` | `spawn_forever` → `Runtime::with_scope(ScopeId::ROOT, cx.spawn)` | 任务投 ROOT scope |
| `dioxus-core/src/scopes.rs:46-61` | `ROOT = ScopeId(0)`、`APP = ScopeId(3)` | scope 编号 |
| `dioxus-core/src/virtual_dom.rs:319-325` | `VirtualDom` 构造时创建第一个 scope（ROOT） | ROOT 在 app 之前就存在 |
| `dioxus-core/src/virtual_dom.rs:340-345` | `base_scope()` 注释「always be around」 | ROOT 常驻 |
| `dioxus-core/src/tasks.rs:344-351` | `LocalTask { scope, … }` | 任务记住自己的 scope |
| `dioxus-core/src/tasks.rs:285-288` | poll 前 `with_scope_on_stack(task.scope, …)` | 任务内 current scope = ROOT |
| `dioxus-core/src/runtime.rs:227-229` | `try_current_scope_id` = 取 scope 栈顶 | 事实 (c) |
| `dioxus-core/src/runtime.rs:216-219` | `scope_owner(scope)` | 显式 owner 的取值点 |
| `dioxus-core/src/runtime.rs:530-538` | `is_descendant_of` | 警告的放行条件 |
| `dioxus-desktop/src/webview.rs:537,572,587` | `poll_vdom` → `wait_for_work` / `render_immediate` | 任务在主线程被 poll |
| `dioxus-desktop/src/launch.rs:40` | `UserWindowEvent::Poll(id) => app.poll_vdom(id)` | 事件循环驱动 |
| `dioxus-desktop/src/waker.rs:10-11` | 「IO 与多线程都在别的线程；主线程被事件循环占用时不可能领取任务」 | 主线程阻塞红线的依据 |
| `dioxus-signals/src/warnings.rs:11-20` | `copy_value_hoisted` 判定 | 警告成因与修法 |
| `dioxus-signals/src/signal.rs:22` | `SyncSignal<T> = Signal<T, SyncStorage>` | 跨线程信号 |
| `dioxus-signals/src/signal.rs:205-226` | `Signal::new_maybe_sync_in_scope` | 显式 owner 创建 |
| `dioxus-signals/src/copy_value.rs:108-124` | `new_maybe_sync_in_scope_with_caller` → `Runtime::current().scope_owner(scope)` | owner 指定合法、需 runtime |
| `dioxus-signals/src/signal.rs:255-269` | `update_subscribers` → `mark_dirty` | 写入唤醒订阅者 |
| `dioxus-core/src/reactive_context.rs:103-108` | `new_for_scope` 存 `runtime.sender`，`unbounded_send(SchedulerMsg::Immediate(id))` | 唤醒不依赖 `Runtime::current()` |
| `dioxus-signals/src/write.rs:47,163` | `try_write_unchecked`；`WriteLock` 的 `D` 记录「写守卫何时 drop」 | 写 API 与通知时机 |
| `dioxus-signals/src/global/mod.rs:201,220,287` | `Rc<RefCell<…>>` + `Runtime::current()` | `GlobalSignal` 不可跨线程 |
| `warnings-0.2.1/src/warnings.rs:58-67` + `:69-73`（`if_enabled`） | `WarningId::enabled()` 非 debug 构建恒为 `false` | 警告只在 debug 可见 |

## 附录 B：边界

- `crates/planned-agent` 零改动：`PlanRunEvent` 已是 `Clone + Send + Sync`，`PlanRunSink: Send + Sync`（`event.rs:50`），宿主侧直接消费。
- 不改现有 UI 交互与样式（C4）。
- 不引入新依赖（C2）。
- 本轮不含任何持久化（§8）。
- 本文档确认后才开始实现（C5）。
