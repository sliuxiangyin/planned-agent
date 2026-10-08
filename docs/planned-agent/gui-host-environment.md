# GUI 运行环境接线（需求分析与设计）

> 状态：**待审核**（本文只做需求分析与设计，未写任何实现代码）
> 相关：`crates/core/AGENTS.md`（`host/` 的职责边界）、`docs/planned-agent/flexible-run-service.md` §12（执行服务的接缝原则）

## 1. 需求

| # | 需求 | 来源 |
|---|---|---|
| R1 | GUI 侧能拿到运行环境数据（`RuntimeEnvironment`），供后续注入执行 | 用户 |
| R2 | **可刷新**：往后本机新增了环境变量 / 命令，要有**暴露的方法**能重新探测 | 用户 |
| R3 | **调用方取到的永远是最新值** —— 不能读到过期快照 | 用户 |
| R4 | 尽量用 dioxus 原生机制实现 | 用户 |
| R5 | 终点：执行时把环境交给内核，由内核决定怎么用（注入 prompt） | 隐含 |

## 2. 现状

**内核侧（`planned-agent`）**

- `core::host::RuntimeEnvironment` 已就绪（探测 + 结构体），**但没有任何调用点**。
- `RunRequest`（`crates/planned-agent/src/flexible/run_service/types.rs:347`）只有 5 个字段：`session_id` / `template` / `params` / `client` / `config` —— **没有 `environment`**。
- `flexible/prompt.rs` 没有"往 system prompt 里拼环境段"的入口。

**GUI 侧（`agent-gui`）**

- **没有任何**环境相关代码。
- 执行入口唯一：`crates/agent-gui/src/services/run_service.rs:55` `start_run_with_template(service, ai, session_id, template, params)` —— 全 GUI 只有这一处构造 `RunRequest`（`:69-75`）。
- 服务本身**只持长期环境**（`RunStore` + 工具表），AI 客户端与执行配置**随每次请求走**（`services/run_service.rs:37-41`）。→ 运行环境也应当**随请求走**，与 R3 天然契合。
- 已有的可复用范式：
  - `context/mcp.rs:29-46` `McpChangeNotifier`：`Signal<u64>` + `bump()`；
  - `services/run_service.rs:91` 起：「**首帧用同步查询播种，避免先空一帧再补上**」；
  - 跨任务写信号一律用 `use_signal_sync`（`Signal<_, SyncStorage>`），见 `pages/plan/shared/session.rs` 注释。

**dioxus 0.7 可用机制**（查 `dioxus.cn/learn/0.7`）

- `use_context_provider` / `use_context`：hook 版，必须是组件内。
- **`consume_context::<T>()`**：**动态消费，在事件处理器与异步任务里也能用**（原文：*"由于 Dioxus 运行时在运行处理器和轮询 future 时总是会设置主题上下文，因此这甚至在异步任务中也有效"*）。→ 刷新方法不必依赖 hook 上下文。
- **`GlobalSignal`**：`static X: GlobalSignal<T> = Signal::global(|| …)`，任何地方可读写、无需 provider；每个应用实例独立。
- 信号是 `Copy`（底层 `GenerationalBox`）→ 可直接 `move` 进异步任务，无需 `Arc`。
- ⚠️ `provide_context`（**动态替换**上下文）"**不是响应式操作**"，不可用于状态更新 —— 状态必须走 `Signal`。

## 3. 设计

### 3.1 状态载体：`Signal<_, SyncStorage>` + context（推荐）

```rust
// crates/agent-gui/src/context/environment.rs（新增）
#[derive(Clone, Copy)]                 // 内含 Signal，天然 Copy
pub struct EnvironmentContext {
    current: Signal<RuntimeEnvironment, SyncStorage>,   // 当前环境
    probing: Signal<bool, SyncStorage>,                 // 是否正在探测
}

impl EnvironmentContext {
    /// 在 `app()` 里调用一次（hook），随后 `use_context_provider` 注入。
    pub fn new() -> Self {
        // ① 同步播种"宿主事实"：detect_host() 零 spawn、无副作用，
        //    立即有值 —— 避免首帧空态（沿用 run_service.rs:91 的做法）。
        let current = use_signal_sync(RuntimeEnvironment::detect_host);
        let probing = use_signal_sync(|| false);
        let ctx = Self { current, probing };
        ctx.spawn_probe();              // ② 后台跑完整探测（含 spawn 子进程）
        ctx
    }

    /// 取**当下**最新值。调用方每次用之前都调它，不要缓存。
    pub fn snapshot(&self) -> RuntimeEnvironment {
        self.current.read().clone()
    }

    pub fn is_probing(&self) -> bool { *self.probing.read() }

    /// R2：手动刷新入口（设置页按钮 / 任何事件处理器）。
    pub fn refresh(&self) { self.spawn_probe(); }

    /// 重入守卫：已在探测则忽略，避免连点产生并发探测。
    fn spawn_probe(&self) {
        if *self.probing.peek() { return; }      // peek：只取值，不建立依赖
        self.probing.set(true);
        let mut current = self.current;          // Copy，可直接移入任务
        let mut probing = self.probing;
        spawn(async move {
            let fresh = RuntimeEnvironment::detect().await;   // 会 spawn 子进程
            current.set(fresh);
            probing.set(false);
        });
    }
}
```

**为什么不用 `GlobalSignal`**：仓库已有 6+ 处 `Signal + context` 先例（`McpChangeNotifier` / config / run snapshot），`GlobalSignal` 一处也没用过；且 `static` 的惰性初始化不好挂"启动即探测"。**若你更看重简洁，`GlobalSignal` 也能实现同样语义**（列为备选）。

### 3.2 初始值：`detect_host()` 播种 + `detect()` 覆盖

- 初值用 `detect_host()`（**同步、零 spawn、纯函数**）→ 首帧就有 `os` / `shell` / 路径分隔符这些最有用的宿主事实。
- 后台 `detect()` 跑完再整体覆盖（补上"哪些命令可用 + 版本"）。
- 语义代价：`executables` 会从空"演进"到有值 —— 用 `is_probing()` 让 UI 表达这个中间态。
- 替代方案（更简单但有空窗期）：初值 `Option::None`，探测完 `Some`。**待拍板。**

### 3.3 R3「永远最新」怎么保证

- `EnvironmentContext` 是 **Copy 句柄**；`snapshot()` 每次都读信号当下值 → 天然最新。
- **契约上只暴露方法、不暴露值**：文档明确写「**不要**把 `snapshot()` 的结果缓存进组件 state 长期持有」。
- 最强保证点在**执行入口**：`start_run_with_template` 在**组装 `RunRequest` 的那一刻**调 `snapshot()`

```rust
pub fn start_run_with_template(
    service: Arc<RunService>, ai: Arc<AiContext>,
    session_id: String, template: FlexiblePlanTemplate, params: PlanRunParams,
    environment: &EnvironmentContext,          // ← 新增
) -> Result<(), String> {
    // …取 client…
    service.start(RunRequest {
        session_id, template, params, client,
        config: ExecutorConfig::default(),
        environment: Some(environment.snapshot()),   // ← 组装那一刻取，必为最新
    });
    Ok(())
}
```

### 3.4 刷新只影响**下一次**执行

进行中的执行用的是**启动那一刻的快照**（`RuntimeEnvironment` 已 clone 进 `RunRequest`）→ 刷新**不会**改变正在跑的会话。
这是刻意的：执行中途换环境（含"某命令忽然可用"）会让上下文与已发生的步骤自相矛盾。
→ 刷新后需**重新执行**才生效；UI 文案要讲清这点。

### 3.5 内核侧通道（R5 的前置，**本期必须一起做**）

GUI 传了，内核得接得住。改动（都在 `planned-agent`，不动 `core`）：

```rust
// ① run_service/types.rs:347  RunRequest 加字段
pub struct RunRequest {
    pub session_id: String,
    pub template: FlexiblePlanTemplate,
    pub params: PlanRunParams,
    pub client: Arc<dyn AiClient>,
    pub config: ExecutorConfig,
    pub environment: Option<RuntimeEnvironment>,   // 新增；None ⇒ 行为与今天逐字一致
}

// ② flexible/prompt.rs  新增（"接收者自己根据环境生成"——符合 core/host 划定的边界）
pub fn step_system_prompt(env: Option<&RuntimeEnvironment>) -> Cow<'static, str>;
//   None        ⇒ Cow::Borrowed(STEP_SYSTEM_PROMPT)   ← 零分配，既有断言不变
//   Some(env)   ⇒ 在 STEP_SYSTEM_PROMPT 尾部拼"运行环境段"（平台 / shell / 可用命令 / 产出目录 / 平台命令语法提示）
//   ⚠️ 段内**不含** working_dir / probed_at（外发隐私，见 core/host 的字段注释）
//      唯一例外是 output_dir（产出目录）：同样含路径，但**刻意外发** —— 模型不知道产出落哪，
//      写出的文件下游工具（沙箱根 = 该目录）就读不到（2026-10-08 补，见 flexible-execution-improvements.md §5.2.5）

// ③ flexible/executor.rs  构造时算**一次** `step_system_prompt`，存 `Cow`，每步复用
//    —— 同一次执行内每步 system 完全相同，以命中 provider 的前缀缓存。
//    `#RESULT` 输出整理步继续用 OUTPUT_RESOLVE_SYSTEM_PROMPT，不拼环境段。
```

### 3.6 数据流全图

```
启动  app()
      ├─ EnvironmentContext::new()
      │    ├─ use_signal_sync(detect_host)     ← 同步播种，首帧即有值
      │    └─ spawn(detect())                  ← 后台补全 executables
      └─ use_context_provider(EnvironmentContext)

设置页/任何 UI
      └─ ctx.refresh()  ──spawn──▶ detect() ──set──▶ current   （R2，重入守卫）

执行  左面板"执行"按钮
      └─ start_run_with_template(…, &ctx)
             └─ RunRequest { environment: Some(ctx.snapshot()) }   ← R3：此刻最新
                    └─ FlexibleExecutor：step_system_prompt(env) 算一次 → 每步 system 拼环境段
```

### 3.7 明确**不做**（本期边界）

- 不落库（环境不持久化；重启应用重新探测）。
- 不做"环境变化的自动监听"（不轮询、不 watch 环境变量）；只提供显式 `refresh()`。
- 不改 `core`（`host/` 已完成，只产出结构体，不渲染）。
- 不做会话级环境（环境是**应用级**事实）。
- 不把环境注入 `#RESULT` 输出整理步。

## 4. 改动点清单

| # | 文件 | 改动 | 期 |
|---|---|---|---|
| 1 | `crates/core/src/host/**` | **不动** | — |
| 2 | `crates/planned-agent/src/flexible/prompt.rs` | 新增 `step_system_prompt(env)` | 内核 |
| 3 | `crates/planned-agent/src/flexible/run_service/types.rs` | `RunRequest` 加 `environment` | 内核 |
| 4 | `crates/planned-agent/src/flexible/executor.rs` | 构造时算一次、每步复用 | 内核 |
| 5 | `crates/planned-agent/src/flexible/step.rs` | 每步用传入的 system prompt | 内核 |
| 6 | `crates/agent-gui/src/context/environment.rs` | **新增** `EnvironmentContext` | GUI |
| 7 | `crates/agent-gui/src/context/mod.rs` | `pub mod environment;` + re-export | GUI |
| 8 | `crates/agent-gui/src/main.rs` | `app()` 注入 `EnvironmentContext` | GUI |
| 9 | `crates/agent-gui/src/services/run_service.rs` | `start_run_with_template` 加参数并填入 | GUI |
| 10 | 调用点（左面板执行处） | 传入 `&EnvironmentContext` | GUI |
| 11 | 设置页（可选） | 展示环境 + 刷新按钮 | GUI |

## 5. 决策（已确认）

| # | 决策 | 说明 |
|---|---|---|
| P1 | **`Signal + context`** | 见 §3.1。`GlobalSignal<T> = Global<Signal<T>>`，其 storage 类型文档未承诺可跨线程写，而探测要在别的线程写回 —— 仓库已验证的写法是 `use_signal_sync`（`SyncStorage`）。 |
| P2 | **值传递，内核侧一并打通** | 曾定「先只做 GUI 侧」，随后按次轮决策改为**值传递**并一次做完（改动点 2–8）。理由：`RuntimeEnvironment` 来自 core（L0），内核收它**不新增依赖边**；若改成内核定义 `EnvironmentProvider` trait，则属于 v2 已删除的「依赖接缝」（`run_service/mod.rs:9`、设计稿 §12.1）。 |
| P3 | **`detect_host()` 同步播种** | 首帧即有宿主事实，无空窗期。 |
| P4 | **只交付代码层方法**（不做 UI） | `snapshot()` / `is_probing()` / `refresh()` + `use_environment()`；刷新入口等 UI 需求出现时再接。 |
| P5 | **全量重探** | `refresh()` 即 `detect()` 全量；不做定点补探。 |
| P6 | **环境段拼在 system prompt 尾部、一次执行算一次** | 每步拿到完全相同的字符串 → 整段可命中 provider 前缀缓存；`#RESULT` 整理步**不拼**环境段。 |
| P7 | 渲染与「该用什么命令」的建议由**内核侧**生成（`flexible/prompt.rs`） | 守住 `core/host` 的边界：core 只给事实、不给策略。 |

## 6. 实施记录（本期）

全部落地，`planned-agent` 与 `planned-agent-gui` 测试均全绿。

**内核侧（`crates/planned-agent/src/flexible/`）**

| 文件 | 改动 |
|---|---|
| `prompt.rs` | 新增 `step_system_prompt(env) -> Cow` + `render_environment_into` + `command_hint`；`None` ⇒ `Cow::Borrowed(STEP_SYSTEM_PROMPT)` 零分配。**不渲染** `working_dir` / `probed_at`。3 个新单测 |
| `step.rs` | `run_step` 的 system prompt 由硬编码改为**入参** |
| `executor.rs` | `run(…, environment, …)`；开头算**一次** system prompt，每步复用 |
| `run_service/types.rs` | `RunRequest` 加 `environment: Option<RuntimeEnvironment>`（+ Debug 字段） |
| `run_service/core.rs` | 解构 `environment` 并传入 `executor.run` |

**GUI 侧（`crates/agent-gui/src/`）**

| 文件 | 改动 |
|---|---|
| `context/environment.rs` | **新增** `EnvironmentContext`：`new()`（`detect_host()` 播种 + 后台探测）、`snapshot()`、`is_probing()`、`refresh()` |
| `context/mod.rs` | 登记 + re-export `EnvironmentContext` / `use_environment` |
| `main.rs` | `app()` 里 `use_context_provider(EnvironmentContext::new)` |
| `services/run_service.rs` | `start_run_with_template` 加 `environment` 参数，在**组装请求那一刻** `snapshot()` |
| `pages/plan/left_panel/left_panel.rs` | 取句柄并传入（`Copy`，可重复用于多次点击） |

**实现取舍**：后台探测用**独立线程 + 独立 tokio runtime**（`new_current_thread().enable_all()`），而不是 dioxus 的 `spawn` —— 探测依赖 `tokio::process`（需要 process driver），GUI 里没有该先例（`kv.rs` 用的 `spawn_blocking` 只需 blocking pool）。写回走 `SyncStorage`，跨线程写正是它的用途。代价：每次刷新新建一次 runtime（低频，可接受）。

**验证**：`cargo test -p planned-agent --lib flexible::` → **72 passed / 0 failed**；`cargo test -p planned-agent-gui --bins` → **62 passed / 0 failed**。

**未做**：UI（刷新入口按 P4 暂不交付 —— 因此 `refresh()` / `is_probing()` 当前无调用点，是预期状态）。
