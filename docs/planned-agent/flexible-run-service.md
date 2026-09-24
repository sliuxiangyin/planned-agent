# Flexible Run Service —— 灵活计划执行服务（宿主侧）

> 状态：**已实现（v2 零端口）** —— 内核 `cargo test -p planned-agent --lib flexible::` →
> **54 passed / 0 failed**；GUI `cargo test -p planned-agent-gui --bins` → **55 passed / 0 failed**；
> `cargo check -p planned-agent-gui --all-targets` 通过。实现记录见 §11，v1 → v2 重构说明见 §12。
> 上游契约：[`flexible-executor.md`](./flexible-executor.md)（执行器本体，已完成阶段 0–4）
> 本文件取代：[`flexible-run-spawn-forever-ui-channel.md`](./flexible-run-spawn-forever-ui-channel.md)（旧宿主通道设计）与
> `crates/agent-gui/src/services/flexible_run_manager.rs`（旧实现，**已删除**）
>
> **本设计不继承旧宿主实现的内部设计**，只承接两条来自仓库现状的硬约束：
> ① 执行必须活过组件卸载（`spawn_forever` 投 ROOT scope）；② 依赖只有在 `ReadyShell` 就绪后才存在。

---

## 1. 需求（本次唯一依据）

1. 用 **dioxus 长驻 Future** 启动一个**服务**；
2. 服务提供**订阅注册**能力，GUI 中可**订阅 / 卸载**；订阅载荷 = `会话 id + 当前执行进度 + 状态`；
3. 服务提供**按会话 id 查询**当前进度状态（不依赖订阅）；
4. 服务可以**启动执行会话已有任务**（模板从库读）。

## 2. 已确认决策

| # | 决策 | 结论 |
|---|---|---|
| 1 | 内核落点 | **A**：内核放 `crates/planned-agent/src/flexible/run_service*`，**纯 Rust、不依赖 dioxus**；GUI 侧只做适配 |
| 2 | 旧 `flexible_run_manager.rs` | **A**：删除，全部消费点迁到新服务 |
| 3 | 进度持久化 | **A**：纯内存，重启即清（第一版）；不复用 `flexible_state` 表（它是「流程阶段/产物」，语义不同） |
| 4 | `start` 入参 | **A（v1）**：`session_id` + 可选覆盖 `(template, params)`；缺省从库读模板、用模板 `inputs[].default` 补参数 → **v2 改为**自包含的 `RunRequest`（§12） |

## 3. 术语与既有事实（均已在代码中核实）

| 事实 | 位置 |
|---|---|
| 「会话」= `plans_flexible_sessions.id`（会话即版本） | `crates/agent-gui/src/storage/entities/plans_flexible_sessions.rs` |
| 执行模板落库列 `parameterized_task`（定稿才非空） | 同上；读法 `PlansFlexibleSessionsRepo::find_parameterized_task` |
| 模板 JSON → `FlexiblePlanTemplate`（`{task, inputs, steps}`） | `crates/planned-agent/src/flexible/template.rs` |
| 三态读取（`NotReady` / `Ready` / `Invalid`） | `crates/agent-gui/src/services/plans_flexible_service.rs::load_template` |
| 执行器入口 | `flexible/executor.rs:67` `run(&tpl, &params, sink: &dyn PlanRunSink, cancel: Option<watch::Receiver<bool>>) -> Result<PlanRunReport>` |
| 事件 7 种 | `flexible/event.rs` `PlanRunEvent` |
| 报告 / 步状态 | `flexible/report.rs` `PlanRunReport` / `StepRunRecord` / `StepStatus` |
| AI 客户端取法 | `crates/ai-manager/src/lib.rs:48` `AiManager::default() -> anyhow::Result<Arc<dyn AiClient>>` |
| 工具注册表 | `context::ToolsContext::registry: Arc<ToolRegistry>` |
| 服务模板源 | `PlansFlexibleService`（**当前在 `PlanPage` 构造注入**，本次要提到 `ReadyShell`） |

## 4. 架构总览

```
                 ┌──────────────── crates/planned-agent/src/flexible/run_service/ ───────────────┐
                 │  纯 Rust，无 dioxus 依赖                                                      │
                 │                                                                              │
  RunService ──► │  RunCommand ──► RunServiceCore::run()  （常驻命令循环）                      │
  (命令入口)     │                    │                                                         │
                 │                    ├─ Start(RunRequest) → 写快照 + 派任务 ───────────────┐  │
                 │                    │   模板/参数/客户端/配置由调用方给齐，内核零接缝  │  │
                 │                    └─ Event ←──── ServiceSink(回送) ◄───────────────────┘  │
                 │                    │                                                         │
                 │                    └─► RunStore：快照表 + 订阅登记表 + 取消通道                │
                 │                          · snapshot(id)  ← 需求 3（同步查询）                │
                 │                          · subscribe(filter) / unsubscribe(id) ← 需求 2      │
                 └──────────────────────────────────────────────────────────────────────────────┘
                                              ▲
                    GUI 适配层（crates/agent-gui/src/services/run_service.rs）
                    · ReadyShell：取 tools.registry → new_run_service → spawn_forever(core.run())
                    · start_run_with_template(..)：取 AI → RunRequest（「没启动起来」在这里报错）
                    · use_run_subscription(session_id)：订阅 + 消费 → 组件本地 Signal + use_drop 卸载
```

关键点：**内核不认识 `Signal`**。GUI 侧的响应式渲染由「组件自己的 hook 订阅 → 写自己的 Signal」完成（订阅回送是 `mpsc`，接收在 dioxus 任务里跑，故写入永远发生在正确的 runtime/scope 内）。

## 5. 内核设计

### 5.1 文件与职责

> 落点细化：决策 1 选的是 `flexible/run_service.rs`。按当前估量（类型 ~150 行 / store ~120 / state ~120 / core ~220 / 单测 ~350），建议落成**子目录** `flexible/run_service/`，与 `flexible/` 一目录一主题的现状一致；若你更想单文件，逻辑切分照旧。

| 文件 | 职责 |
|---|---|
| `run_service/mod.rs` | 门面：声明子模块 + 对外 re-export（外面只 `use` 这里，同 `flexible/mod.rs` 风格） |
| `run_service/types.rs` | `RunRequest` / `RunCommand` / `RunStatus` / `StepPhase` / `StepSnapshot` / `RunSnapshot` / `RunUpdate` / `SessionFilter` |
| `run_service/state.rs` | `apply_event(...)` —— **纯函数**：事件 → 快照归并。单测主战场 |
| `run_service/store.rs` | `RunStore`：快照表 + 订阅登记表 + 同步查询 + 广播 |
| `run_service/core.rs` | `RunServiceCore`：命令循环、`Start`（登记取消 → 写快照 → 派任务）、panic/Err 终态收敛、`ServiceSink` |
| `run_service/service.rs` | `RunService`：给宿主的同步门面（`start`/`stop`/`snapshot`/`subscribe`/`unsubscribe`） |

### 5.2 类型（形状）

```rust
pub type SessionId = String;
pub type SubscriptionId = u64;

pub enum RunStatus { Running, Succeeded, Failed, Cancelled }

pub enum StepPhase { Pending, Running, Done, Failed, Skipped }
// 迁自 GUI 现有 impl：css_suffix() / node_suffix() / line_suffix() 一并搬过来（纯展示映射，留在内核无害）

pub struct StepSnapshot {
    pub index: usize,                 // 从 1 开始
    pub result_reference: String,     // #E1
    pub intent: String,               // 已展开
    pub expected_output: String,
    pub phase: StepPhase,
    pub duration_ms: u64,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub tool_calls: usize,
    pub error: Option<String>,
}

pub struct RunSnapshot {
    pub session_id: SessionId,
    pub run_id: u64,                  // 每次 start 自增；用于丢弃上一轮的迟到事件
    pub status: RunStatus,
    pub total_steps: usize,
    pub current_step: Option<usize>,  // ← 「当前进度」
    pub steps: Vec<StepSnapshot>,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub error: Option<String>,        // 整次失败的原因（单步失败的原因在 steps[].error）
    pub report: Option<PlanRunReport>,// 终态时带上（STATS 块直接用）
    pub last_thought: Option<String>, // 最近一条 StepThought（THINK 终端）
}

impl RunSnapshot {
    pub fn is_running(&self) -> bool;
    pub fn progressed(&self) -> usize;          // Done + Failed 的步数（= 旧 statusbar 的 N/M 分子）
    pub fn phase_of(&self, offset: usize) -> StepPhase;
}

pub struct RunUpdate { pub session_id: SessionId, pub snapshot: RunSnapshot }

pub enum SessionFilter { All, One(SessionId) }

/// 一次执行的完整输入：能执行所需的一切都由调用方给（v2 零端口形态）。
#[derive(Clone)]
pub struct RunRequest {
    pub session_id: SessionId,
    pub template: FlexiblePlanTemplate,
    pub params: PlanRunParams,
    pub client: Arc<dyn AiClient>,
    pub config: ExecutorConfig,
}
// 手写 `Debug`：`Arc<dyn AiClient>` 不实现它（打印 provider/model 名）。

pub enum RunCommand {
    Start { request: RunRequest },
    Event { session_id: SessionId, run_id: u64, event: PlanRunEvent },  // ServiceSink 回送
}
```

### 5.3 无依赖接缝（v2）—— 输入即 `RunRequest`

内核**不定义任何接缝**：不依赖 GUI 类型，也不要求宿主实现 trait。可执行所需的一切
（模板 / 参数 / AI 客户端 / 执行配置）由调用方随 `RunRequest` 给齐；内核长期持有的只有
`Arc<ToolRegistry>` —— 它描述「服务能用哪些工具」，是**环境**而非本次输入。

> **v1 曾经有两个接缝**（`TemplateLoader` / `TemplateLoad` 与 `AiProvider`），让内核自己去读库、
> 自己去取 AI。已删除：它把「会话是否定稿、模板能否反序列化、有没有配 provider」三个宿主领域概念
> 带进了执行器 crate，也与仓库既有习惯相悖（`services/planner_service.rs` 就是直接在调用点取
> `ai.manager.default()`）。`ExecutorConfig` 同理随请求走：`FlexibleExecutor::new(ai, tools, cfg)`
> 的三个入参里，只有 `tools` 是服务的长期环境。理由与迁移见 §12。

### 5.4 RunStore

```rust
pub struct RunStore { /* RwLock<HashMap<SessionId, RunSnapshot>> + RwLock<HashMap<SubscriptionId, Sub>> + AtomicU64 */ }

impl RunStore {
    pub fn snapshot(&self, session_id: &str) -> Option<RunSnapshot>;      // 需求 3
    pub fn snapshots(&self) -> Vec<RunSnapshot>;                          // 总览（首页用）
    pub fn subscribe(&self, filter: SessionFilter, sink: UnboundedSender<RunUpdate>) -> SubscriptionId;
    pub fn unsubscribe(&self, id: SubscriptionId) -> bool;
    pub fn apply(&self, session_id: &SessionId, mutate: impl FnOnce(&mut RunSnapshot)) -> RunSnapshot;
    // apply = 改表 → 广播匹配订阅者（锁内推送；订阅者已 drop → 顺手回收该登记）
}
```

- `subscribe` **注册后立刻回放**当前匹配快照（避免"订阅了但界面空白"）。
- 订阅登记与推送同锁：保证 `unsubscribe` 返回后不会再有 push → 组件卸载时无「写已释放 Signal」竞态。

### 5.5 事件 → 快照（`state.rs` 纯函数）

| `PlanRunEvent` | 快照变化 |
|---|---|
| `RunStarted { total_steps }` | `status=Running`、`started_at_ms`、`steps` 初始化 `total_steps` 个 `Pending` |
| `StepStarted { index, intent }` | `current_step=Some(index)`；该步 `Running` + `intent` |
| `StepThought { index, round, text }` | `last_thought = Some(text)`（不塞进每步大字段） |
| `StepToolCall { index, tool, ok }` | 该步 `tool_calls += 1`（`ok=false` 不改相位，只在 UI 标红） |
| `StepFinished { index, record }` | 该步 `phase ← record.status`、耗时/token/tool_calls/error |
| `RunFinished { report }` | `status = if report.success { Succeeded } else { Failed }`、`finished_at_ms`、`report` |
| `Failed { index: Some(i), error }` | 该步 `Failed` + `error` |
| `Failed { index: None, error }` | 整次 `status=Failed` + `error`（执行器异常退出 / panic 时由服务兜底补发） |

`apply_event` 的签名刻意与 store 解耦：`fn apply_event(snapshot: &mut RunSnapshot, event: PlanRunEvent)`，纯函数、可脱离 tokio 与 dioxus 单测。

### 5.6 RunServiceCore 命令循环

```rust
pub struct RunServiceCore { rx: UnboundedReceiver<RunCommand>, inner: RunLoop }
struct RunLoop {
    store: Arc<RunStore>,
    tools: Arc<ToolRegistry>,        // 服务环境（长期）；其余随 RunRequest 走
    tx: UnboundedSender<RunCommand>,
    seq: u64,                        // 每次 start 递增 → run_id
}
impl RunServiceCore {
    pub async fn run(self);   // loop { select! { rx.recv() / 推进执行任务 } }
}
```

`Start` 的处理（**全同步、无 IO、不 await**：命令进来的那一刻状态就确定）：

1. 已 `Running` 的同一会话 → 忽略并 `warn`，**不动快照**（进行中的进度由在跑的任务维护，覆盖反而把展示搞坏）。
2. `seq += 1` → `run_id`；建 `watch::channel(false)` 存进 `RunStore`（**先登记再写快照**：`stop` 由 GUI 线程直连 store，两步之间的瞬间点停止才不落空）。
3. 写起跑快照（`status=Running`、全 `Pending`）→ 订阅者立刻看到反馈。
4. 把「跑执行器」的 future 推进循环内的 `FuturesUnordered`（**不额外 `tokio::spawn`**：内核只要求宿主能驱动 `core.run()` 这一个 future）。
5. 任务体用 `AssertUnwindSafe(..).catch_unwind()` 包住执行器：panic 被拦在这里（否则会 unwind 穿过 `core.run`，**整个服务连同所有会话一起停**，而 `start` 依旧返回成功 —— 最难排查的一种死法）；panic 与 `Err` 都收敛成一次执行失败。
6. `ServiceSink::emit` → `RunCommand::Event{...}` 回送循环；循环里 `run_id` 不匹配即丢弃，匹配则 `store.update(apply_event)`。
7. 任务异常 / panic 退出后若快照仍在 `Running`（连终态事件都没发出来，该会话会被重入检查永久锁死），补发一条 `Failed { index: None }` 收敛。

**停止（实现期细化）**：取消通道放在 `RunStore` 里，`RunService::stop` **同步**完成（不经命令队列），
点「停止」立刻生效；终态时循环把该会话标成 `Cancelled`（`RunFinished` 与取消标记同时到达时以取消为准）。
因此 `RunCommand` 实际只有 `Start` / `Event` 两个变体。

**错误策略（v2）**：内核只在 `RunRequest` 到手后工作，因此「没启动起来」的失败（未定稿 / 模板损坏 /
未配 AI / 读取失败）在**宿主侧**解析成 `Err`、由 UI 呈现（toast / 按钮置灰），**不进服务快照**；
快照只承载执行期状态。`start` 无返回值 —— 受理成功后再无「启动期」状态可报。

### 5.7 RunService（宿主门面）

```rust
pub struct RunService { tx: UnboundedSender<RunCommand>, store: Arc<RunStore> }

impl RunService {
    pub fn start(&self, request: RunRequest);
    pub fn stop(&self, session_id: &str) -> bool;                       // 无在跑 → false
    pub fn snapshot(&self, session_id: &str) -> Option<RunSnapshot>;    // 需求 3
    pub fn snapshots(&self) -> Vec<RunSnapshot>;
    pub fn subscribe(&self, filter: SessionFilter, sink: UnboundedSender<RunUpdate>) -> SubscriptionId;  // 需求 2
    pub fn unsubscribe(&self, id: SubscriptionId) -> bool;              // 需求 2
}
```

`start` 走通道（服务循环是状态唯一写者）；`stop` / `snapshot` / `subscribe` / `unsubscribe` 直接进
`RunStore`（同步、无需往返）—— `stop` 走 store 是为了让「点停止」**立刻**生效，不必排队等循环。

## 6. GUI 适配

### 6.1 注入点

| 位置 | 做什么 |
|---|---|
| `ReadyShell`（`main.rs`） | 取 `tools.registry` + 新建 `PlansFlexibleService` → `new_run_service(store, tools)` → `use_context_provider(Arc<RunService>)` → `use_hook` 保证只构造一次 → `spawn_forever(core.run())`（AI 客户端不进服务：随每次请求走） |
| `main.rs::app()`（根 rsx） | 挂 `ToastProvider`（`.dx-toast-container` 是 `position:fixed`，不影响布局）—— 启动失败提示的宿主 |
| `main.rs::app()` | **删除** ROOT 的 `FlexibleRunSignals`（`states`/`cancels` 两个 Signal）；`context/mod.rs` 的「唯一例外」注释同步删除 |
| `ReadyShell` | 新增 `use_context_provider(Arc<PlansFlexibleService>)`（从 `StorageContext` 的两个 repo 构造），PlanPage 改为 `require_resource::<PlansFlexibleService>()`（**不再就地 new**） |

收益：服务不持有任何 dioxus 类型 → ROOT scope 的 Signal 例外消失，全部 context 重新统一在 `ReadyShell` 注入。

### 6.2 hooks（`crates/agent-gui/src/services/run_service.rs`）

```rust
pub fn use_run_service() -> Arc<RunService>;                      // require_resource 薄封装

/// 订阅一个会话的进度；组件卸载自动卸载订阅。
/// 返回本地 Signal：`None` = 该会话还没有执行记录。
pub fn use_run_subscription(
    session_id: Signal<Option<String>, SyncStorage>,
) -> Signal<Option<RunSnapshot>, SyncStorage>;
```

`use_run_subscription` 内部：

1. `use_signal_sync` 建本地 Signal，初值用一次同步查询播种（`peek` 会话 id + `service.snapshot`）；
2. `use_effect`：读 `session_id` 信号 → 改订新会话：先 `unsubscribe` 旧的，再
   `service.subscribe(One(sid), tx)` 拿到 `rx`（**注册即回放**，所以立刻有值），最后
   `snapshot.set(service.snapshot(&sid))` **重置**本地值。
   ——**换会话必须重置**：不清就会把上一个会话的进度画到新会话的模板上，旧会话 Running 还会把
   新会话的执行按钮一起置灰。用同步查询播种而非清成 `None`，可避开「有历史快照」的会话闪空态；
   顺序上**先订阅后播种**：万一那唯一一次推送落在两步之间，通道里已排队、消费任务随后会写入，
   反过来（先查后订）则可能永久停在旧值；
3. `spawn`（dioxus 任务，挂组件 scope，卸载即取消）：循环 `rx.recv()` → 按 `update.session_id`
   过滤（`unsubscribe` 只保证「不再 push」，通道里**已缓冲**的旧会话消息仍会被读到）→ 写本地 Signal；
4. `use_drop(|| service.unsubscribe(sub_id))` —— 同锁保证之后不再有 push。

### 6.3 消费点改造

| 文件 | 改法 |
|---|---|
| `main.rs` | 按 §6.1：删 ROOT 信号、ReadyShell 建服务 + spawn_forever、注入 `PlansFlexibleService` |
| `services/mod.rs` | `flexible_run_manager` → `run_service` |
| `services/run_service.rs` | GUI 适配：`start_run_service`（组装 + `spawn_forever`）、`start_run_with_template`（取客户端 → 组请求 → `start`）、`use_run_service`、`use_run_subscription` |
| `services/flexible_run_manager.rs` | **删除** |
| `context/mod.rs` | 删「`FlexibleRunSignals` 唯一例外」段 |
| `pages/plan/page.rs` | 删除页面内 `PlansFlexibleService::new(...)`（改由 `ReadyShell` 注入） |
| `pages/plan/left_panel/left_panel.rs` | `use_run_subscription(session_id)` 取快照；`run_state.*` 改读 `snapshot.is_running()` / `phase_of(i)`；`on_run` → 同步调 `start_run_with_template(..)`，`Err` 用 toast 提示；`on_stop` → `service.stop(&sid)`；`can_run` 口径不变 |
| `pages/plan/left_panel/pipeline.rs` | `StepPhase` 改从 `planned_agent::flexible::run_service` 导入（取值不变） |
| `pages/plan/left_panel/stats.rs` | 从 `snapshot.report` 取 `PlanRunReport`（字段不变） |
| `pages/home/components/active_plans`（可选） | `subscribe(All)` + `use_run_subscription(None)` 做全局进度总览 |

`left_panel` 现在的「执行态叠加到渲染步骤」逻辑保留：`step.phase = snapshot.phase_of(offset)`。

## 7. 语义细则

| 项 | 结论 |
|---|---|
| 并发 | 不同会话可并行；同一会话重复 `start` 忽略并 `warn`（**不动快照**） |
| 取消 | `RunService::stop` → `cancel.send(true)`（同步进 store）；执行器把未跑步骤记 `Skipped`；终态 `Cancelled`；**已成功的终态不被改写** |
| 迟到事件 | `run_id` 不匹配即丢弃 —— 上一轮任务收尾事件不会污染新一轮快照 |
| 订阅回放 | 注册瞬间推送当前匹配快照（`All` 推全表逐条，`One` 推该会话） |
| 无人订阅 | 执行照跑（进度纯内存、随时可查）；`snapshot` 永不因无人订阅而丢失 |
| 服务循环 | 不 panic：执行任务在 `catch_unwind` 内跑，单个执行失败 / panic 只落快照 `Failed`，服务与其它会话不受影响（见 §5.6 步 5–7） |
| 重启 | 内存态清空；`snapshot` 返回 `None`（第一版不做落库，见决策 3） |

## 8. 测试计划

| 层 | 用例 |
|---|---|
| `state.rs`（纯函数） | 事件序列 → 期望快照：正常两步、`StepFinished` 带 `record.status=Failed`、`RunFinished{success:false}`、`Failed{index:None}`、`StepThought` 覆盖 `last_thought` |
| `store.rs` | `subscribe` 即回放；`unsubscribe` 后不再收到；`SessionFilter::One` 不串台；订阅者 drop 后自动回收登记；`snapshot` 空表返回 `None` |
| `core.rs`（集成，用 `flexible::testing::FakeAiClient` + `SlowAi`） | `Start(RunRequest)` 正常跑完 → `Succeeded` + `report.success` + 可同步查询；进度逐步推进（`current_step` 1→2）；`stop` → `Cancelled` + 后续步骤 `Skipped`；同会话重入被忽略（只跑一轮）；`run_id` 去重（手工塞旧 `Event` 不影响新快照）；单步失败 → 报告形状正确 |
| 验收命令 | `cargo test -p planned-agent --lib flexible::`（**55 passed**）与 `cargo test -p planned-agent-gui --bins`（**55 passed**） |

GUI 侧不含逻辑，只做接线，靠 `cargo check -p planned-agent-gui` + 手工验收（执行 / 停止 / 切页不中断 / 切会话各查各的）。

## 9. 施工顺序（每步有独立验收）

| 步 | 内容 | 验收 |
|---|---|---|
| 1 | `run_service/types.rs` + `state.rs` + 单测 | `cargo test -p planned-agent --lib flexible::run_service` 全绿 |
| 2 | `run_service/store.rs` + 单测 | 同上 |
| 3 | `run_service/core.rs` + `service.rs` + `mod.rs` + 集成单测 | 同上；`cargo check -p planned-agent` |
| 4 | GUI 适配层 + 删旧 manager + 改造 6 个文件（§6.3） | `cargo check -p planned-agent-gui` + 手工验收 |
| 5 | 文档收尾：本文件补「实现记录」，`flexible-executor.md` 阶段 5 改指本文件 | 文档一致性自查 |

阶段 5 的 5.1–5.5（迁移引用 / PARAMS 可编辑 / 执行按钮 / 事件→UI / 停止按钮）由本设计一次性覆盖，`flexible-executor.md` 中对应条目改为指向本文件。

上表是 v1 的施工顺序，五步均已完成；**v2 重构（内核零端口）见 §12**。

## 10. 未决 / 风险

| # | 事项 | 处理 |
|---|---|---|
| 1 | `PlanRunParams` 目前无 `Serialize`；若将来要跨进程/落库需补 | 本期不需要（同进程 move） |
| 2 | 内核落点 | **已定**：子目录 `flexible/run_service/`（§5.1） |
| 3 | 首页总览是否要做 | 列为可选步（§6.3 末行） |
| 4 | `StepThought` 只留最近一条，THINK 终端若要完整轨迹需另存（有界环形缓冲） | 第一版只留最近一条；有需要再加 `Vec` + 上限 |
| 5 | 执行记录落库（`PlanRunReport` → HISTORY） | 仍延后（见 `flexible-executor.md` 决策记录 #1） |
| 6 | 启动失败（未定稿 / 模板损坏 / 未配 AI）怎么呈现 | **已定**：宿主 UI（toast / 按钮置灰）；服务快照不承载「没启动」——见 §12 |
| 7 | 内核是否保留依赖接缝 | **已定（v2）**：不保留，改为零端口的 `start(RunRequest)` —— 见 §12 |

## 11. 实现记录（2026-09）

### 交付物

| 位置 | 内容 |
|---|---|
| `crates/planned-agent/src/flexible/run_service/{mod,types,state,store,core,service}.rs` | 内核：类型 / 事件归并 / 状态表 / 命令循环 / 宿主门面 / 组装 |
| `crates/agent-gui/src/services/run_service.rs` | GUI 适配：`start_run_service` + `start_run_with_template` + 两个 hook（v1 曾是「两个接缝实现」） |
| `crates/agent-gui/src/services/flexible_run_manager.rs` | **已删除**（可从提交 `850b31a` 恢复） |
| `main.rs` / `context/mod.rs` / `pages/plan/page.rs` / `left_panel/{left_panel,pipeline,stats}.rs` | 消费点迁移 |

### 实现期对设计的细化（与上文表述冲突时以本表为准）

| # | 设计稿 | 实际 |
|---|---|---|
| 1 | `RunCommand` = `Start` / `Stop` / `Event` | 只有 `Start` / `Event`：取消通道放进 `RunStore`，`RunService::stop` **同步**返回 `bool` |
| 2 | 执行任务 `tokio::spawn` | 用 `FuturesUnordered` 在服务循环内轮询：内核不要求宿主提供 tokio runtime，只要求能驱动 `core.run()` 这一个 future |
| 3 | `use_run_subscription(session_id: Option<String>, plan_id)` | `use_run_subscription(session_id: Signal<Option<String>, SyncStorage>)`：参数改成**信号**，`use_effect` 才能随「切换会话」自动改订；不需要 `plan_id` |
| 4 | hook 内用 `spawn_forever` 消费订阅 | 用 dioxus `spawn`：任务挂在组件 scope，卸载即取消（与 `use_drop` 注销双保险） |
| 5 | 首帧可能空 | `use_signal_sync` 的初值用一次同步查询播种（`peek` 会话 id → `service.snapshot`），已执行过的会话不闪空帧 |
| 6 | 重入拒绝「写 error」 | 重入只 `tracing::warn` 并忽略，**不动进行中的快照**（否则会把正在展示的进度写坏） |
| 7 | ROOT scope 仍有 Signal | 服务不持有任何 dioxus 类型 → `context/mod.rs` 的「唯一例外」段已删除，全部 context 统一在 `ReadyShell` 注入 |
| 8 | `Start` 内同步读模板 | （v1）模板读取放进 `RunSession` 任务、`Start` 先写占位快照（`RunSnapshot::preparing`）挡重复启动 → **v2 已取消**：模板由宿主解析，`Start` 全同步 |
| 9 | 订阅消费任务只靠注销隔离 | 任务内再按 `update.session_id` 过滤：`unsubscribe` 只保证「不再 push」，通道里**已缓冲**的旧会话消息仍会被读到（否则切会话会串台） |
| 10 | `start_run_service` 在组件体直接调用 | 包 `use_hook` 记忆化：`ReadyShell` 重渲染绝不重建服务与常驻循环 |
| 11 | `start(session_id, template?, params?)` | **v2**：`start(RunRequest)` —— 模板 / 参数 / 客户端 / 配置全由调用方给齐（§12） |
| 12 | 启动失败写快照 | **v2**：宿主侧 `start_run_with_template` 返回 `Err`，UI 用 toast 提示；快照不再承载「没启动」（§12） |
| 13 | 执行任务用 `tokio::spawn` | **v2**：仍在 `FuturesUnordered` 里轮询，外加 `catch_unwind`（任务 panic 不可穿过 `core.run`）与「无终态就补发 `Failed`」的收敛兜底 |

### 验证

**v1（接缝形态）**

- 内核：`cargo test -p planned-agent --lib flexible::` → **58 passed / 0 failed**；当时新增 25 个用例
  （`state` 6 / `store` 9 / `core` 10，覆盖归并、回放、过滤、取消、迟到事件、启动失败、重复启动、单步失败）
- 宿主：`cargo check -p planned-agent-gui --all-targets` 通过，`run_service` 无新增警告
- 审查后修复三处并重跑：会话切换串台（#9）、`start` 读模板冻住执行任务（#8）、`start_run_service`
  未记忆化（#10）

**v2（零端口，当前）**

- 内核：`cargo test -p planned-agent --lib flexible::` → **55 passed / 0 failed**（删掉 4 个已移交宿主侧
  的用例，新增 `stale_terminal_event_keeps_new_cancel_channel` 覆盖「迟到终态不清掉新一轮取消通道」）
- 宿主：`cargo test -p planned-agent-gui --bins` → **55 passed / 0 failed**；
  `cargo check -p planned-agent-gui --all-targets` 通过（改动的三个文件零新增警告）
- 两轮独立审查：首轮报 3 个 must-fix（执行任务 panic 会拖死整个服务、切会话串台、取消覆写已成功的
  终态）并全部修完；复核通过；末轮再审提出的「文档残留旧函数名」同轮改掉
- 未做（延后）：执行记录落库、首页「直接跑会话」入口与 `active_plans` 全局进度总览、
  `StepThought` 完整轨迹（当前只留最近一条）

## 12. 重构记录：内核零端口（v1 → v2，**已完成**）

### 12.1 结论与理由

评审意见：`flexible` 只应当接收 `FlexiblePlanTemplate`；「会话是否存在 `parameterized_task`、模板能否
反序列化」是调用方的事。**采纳。**

论证：

- flexible 的输入契约本就是 `template` + `params` + `Arc<dyn AiClient>`（后三样都是
 `FlexibleExecutor::new` / `run` 的参数），三个在调用点上都拿得到；
- `TemplateLoader` 把三个**宿主领域概念**（会话存在性、落库列是否为空、JSON 反序列化失败）带进了
 执行器 crate；
- 仓库既有习惯是「谁调用谁取依赖」：`services/planner_service.rs:32` 直接在调用点
 `ai.manager.default()`，并未给 planner 造端口；`left_panel.rs:235` 的 `can_run` 也已把「模板就绪」
 当 UI 判定用了 —— 本次的端口形态才是例外。

代价（当初引入端口的动机）：「通过服务一条 `start(session_id)` 启动已有任务」的便利要让给宿主侧一个
薄编排函数。实际只有「首页直接跑某会话」需要它；左面板传的本来就是编辑态模板。

**`ExecutorConfig` 的归属**（同一次评审的第二问）：`FlexibleExecutor::new(ai, tools, cfg)` 的三个入参里，
只有 `tools` 是服务的**长期环境**（工具可见范围），`ai` 与 `cfg` 都是**本次执行的入参** —— 故 `cfg` 与
`client` 同侧，一并进 `RunRequest`，服务构造只剩 `store` + `tools`。
（实测 `ExecutorConfig` 四个字段的语义并不统一，见 §12.6。）

### 12.2 目标形态

```rust
/// 一次执行的完整输入：能执行所需的一切都由调用方给。
pub struct RunRequest {
    pub session_id: SessionId,
    pub template: FlexiblePlanTemplate,   // 必填：能执行就是能执行
    pub params: PlanRunParams,            // 必填：默认值由调用方决定（`PlanRunParams::from_template`）
    pub client: Arc<dyn AiClient>,
    pub config: ExecutorConfig,           // 与 client 同侧：都是 `FlexibleExecutor::new` 的入参
}

impl RunService {
    pub fn start(&self, request: RunRequest);
    // stop / snapshot / snapshots / subscribe / unsubscribe 不变
}

/// 内核构造只剩「服务环境」：`tools` 是长期可见范围，其余随每次请求走。
pub fn new_run_service(
    store: Arc<RunStore>,
    tools: Arc<ToolRegistry>,
) -> (Arc<RunService>, RunServiceCore);

pub enum RunCommand {
    Start { request: RunRequest },
    Event { session_id: SessionId, run_id: u64, event: PlanRunEvent },
}
```

`RunRequest` 手写 `Debug`（打印 `client.provider_name()` / `model_name()`）—— `Arc<dyn AiClient>` 没
有 `Debug`，derive 会失败。

宿主侧的编排落点（`agent-gui/src/services/run_service.rs`）：

```rust
/// 取 AI 客户端 → 组 `RunRequest` → 交服务；失败原因返回给 UI 呈现（toast）。
/// 模板由调用方给：页面本来就已经从库 `load_template` 过一遍（数据在手上，无须二次读库）。
pub fn start_run_with_template(
    service: Arc<RunService>,
    ai: Arc<AiContext>,
    session_id: String,
    template: FlexiblePlanTemplate,
    params: PlanRunParams,
) -> Result<(), String>;
```

> **实施时的进一步简化**：原计划的 `start_session_run(session_id, params) -> Result<(), String>`
> （内核去读库）已取消 —— 那等于把「读库 + 三态解析」又搬回执行路径，违背本节的分层初衷；
> 而且它无需 `async`，反而逼出了「`spawn` 里跨 scope 弹 toast」的额外复杂度。
> 将来首页要「直接跑某个会话」时：调用方先 `load_template`、再调本函数即可。

### 12.3 与 v1（当前代码）的差异

| 项 | v1 | v2（目标） |
|---|---|---|
| 模板来源 | `TemplateLoader` trait（async，内核 `await`） | 调用方解析后放进 `RunRequest.template` |
| AI 客户端 | `AiProvider` trait（延迟取） | 调用方取好放进 `RunRequest.client` |
| 未定稿 / 模板损坏 / 未配 AI | 内核 `abort` 写 `RunSnapshot::failed` | 宿主 UI（toast / 置灰），**不进服务快照** |
| `RunSnapshot::preparing` | 挡住「读模板期间的重复启动」 | 删除：`start` 全程同步，直接写 `started` 快照 |
| `RunSnapshot::failed` | `abort` 使用 | 删除（无调用点；执行期失败由 `Failed{index:None}` 事件覆盖） |
| `RunSession` / `abort` | 任务里做「读模板 + 取 AI + 起跑」 | 删除：任务里只剩 `executor.run(...)` |
| `TemplateLoad` | 三态枚举 | 删除 |
| `ExecutorConfig` | 服务构造参数（`new_run_service(store, tools, cfg)`） | 随请求走：`RunRequest.config` |
| 宿主侧 | 无需解析 | 新增 `start_run_with_template`（取客户端 + 组请求）+ 失败提示 |

`core.rs` 于是退化为：**重入检查 → 登记取消 → 写 `started` 快照 → 派任务跑 executor**（无 await、无 IO）。

### 12.4 迁移步骤（已执行）

1. **内核**：删 `core.rs` 的 `TemplateLoader` / `TemplateLoad` / `AiProvider` / `RunSession` / `abort`；
   `start` 改同步收 `RunRequest`；`new_run_service` 去掉 `ai` / `loader` / `cfg`，只剩 `store` + `tools`；
   `types.rs` 删 `RunSnapshot::preparing` / `RunSnapshot::failed`，`RunCommand::Start` 改带 `RunRequest`。
2. **GUI 适配**：`start_run_service` 只传 `store` / `tools`（`ExecutorConfig` 随请求走）；新增
   `start_run_with_template`（取 AI 客户端 → 组 `RunRequest`）；左面板 `on_run` 改为同步调它。
3. **左面板提示**：启动失败改用 `components/toast` 的 `use_toast`；`run_snapshot.error` 不再承载
   「没启动」（该 prop 可只留给执行期失败）；`can_run` 口径不变。
4. **测试**：删 `FixedLoader` / `FixedAi`（`SlowAi` 保留，用于取消与重复启动）；
   删「未定稿 / 模板损坏 / AI 不可用 → Failed 快照」三个用例（覆盖改由 GUI 侧验证）；
   重入、取消、run_id 过滤、报告覆盖、进度推进等用例保留。
5. **文档**：把本节合并回 §5 / §6 / §7，§11 注明 v1 → v2 的演进。

### 12.5 验收

- `cargo test -p planned-agent --lib flexible::` 全绿（用例数会因删桩而减少，覆盖不降）
- `cargo check -p planned-agent-gui --all-targets` 通过
- 手工：会话未定稿时点执行 → toast 提示，且**没有任何服务状态被写入**；未配 AI 同理；
  正常执行 / 停止 / 切会话与重构前一致

### 12.6 长期清理项：`ExecutorConfig` 语义混杂

`ExecutorConfig` 四个字段分属三层：`temperature` / `max_tokens` 是「LLM 请求覆盖」（本质上是
`client.default_config()` 之上的一层 override）、`allowed_tools` 是「工具可见性」（与 `tools` 同侧）、
`max_rounds_per_step` 是「执行编排策略」（与 AI、工具都无关）。

当前形态（一个 struct 全塞）在只有 `Default` 一个取值时无害；但要支持「按会话配工具白名单」或
「按次覆盖温度」时，就会被逼着整包传来传去。届时拆成三处（或拆出 `LlmOverrides` / `ToolScope` /
`RunPolicy`）—— **本次重构不拆**：那要改 `executor.rs` 的公开 API，超出范围。
