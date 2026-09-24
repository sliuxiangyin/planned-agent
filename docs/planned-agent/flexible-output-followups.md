# flexible 后续两项：面板展示 `output_schema` ＋ 本次执行结果直接展示

> 设计稿。状态：**已实施**（2026-09；含执行期的「输出整理步」，见 §3）。范围：**面板展示 + 本次结果展示**，不落库（持久化 / 反哺另列 §4）。
> 前序：`docs/planned-agent/flexible-output-step.md`。

---

## 0. 现状（已核实）

### 0.1 左侧面板

- 容器 `crates/agent-gui/src/pages/plan/left_panel/left_panel.rs`；各块 `params.rs` / `pipeline.rs` / `stats.rs` / `history.rs` / `dialogs.rs`；模块声明 `mod.rs:17-24`。
- 数据源：`left_panel.rs:159` `service.load_template(session_id)`（`use_resource` `:153-163`）→ `PlanTemplateState`（`services/plans_flexible_service.rs:29-37`）。**按 props 传**，解构在 `left_panel.rs:174-200`（当前只取 `inputs` / `steps`，未取 `task` / `output_schema`）。
- 四块挂载：`left_panel.rs:373-422`（`ParamsView` / `StatsView` / `PipelineView` / `HistoryView`）。
- 渲染范式：`params.rs:47-66` 用 `for … in …iter()`（**不是 `<For>`**）+ 空态 `div { class: "plan-bento-empty" }`（`:44-45`）。
- **`output_schema` 数据已就绪**：`FlexiblePlanTemplate.output_schema: Option<Value>`（`crates/planned-agent/src/flexible/template.rs:28`）。

### 0.2 执行链路（已通 UI，但结果不落库）

- 链路：`left_panel.rs:264` `start_run_with_template`（on_run）→ `services/run_service.rs` → 内核 `run_service/mod.rs:51` → `run_service/core.rs:158` 构造 `FlexibleExecutor`；启动 `main.rs:249`。
- GUI 侧执行态：`run_service.rs:92-158` 的 `use_run_subscription` → `Signal<Option<RunSnapshot>>`（组件本地，不经 context；context 只放 `Arc<RunService>`）。`use_effect`（`:106-149`）订阅 `RunUpdate`，每次 `snapshot.set(Some(update.snapshot))`（`:141-148`）—— **全量快照推送**，GUI 不区分事件类型。
- 内核快照：`RunSnapshot`（`run_service/types.rs:207-222`）、`StepSnapshot`（`:127-145`，**无任何 output 字段**）、`StepTrackLine`（`:113-123`，`Thought` / `Tool{tool,args,ok}`）、`PlanRunReport`（`report.rs:71-80`，纯指标）、`StepRunRecord`（`report.rs:33-60`，其 `output_summary`(:57) 是唯一的输出字段）。
- serde 边界：`RunSnapshot` / `StepSnapshot` / `PlanRunEvent` **无 serde**；`PlanRunReport` / `StepRunRecord` 有 serde 但**全库无 storage 引用**（即未落库）。→ **改字段只影响内核单测的字面量构造点**，不动任何持久化。

### 0.3 「结果」到不了 UI 的三个断点

1. **完整 output 不外泄**：`step.rs:155` 产生 `StepRunResult.output`（`:42-47`），唯一消费方是 `executor.rs:158-159` 塞进内存 `HashMap`（`:90`）喂下游 `prior`（`collect_prior` `:254-263`）。
2. **进报告的只有 200 字**：`SUMMARY_MAX_CHARS = 200`（`step.rs:25`），`step.rs:255` 用 `summarize`（`:389-396`）截断 → `StepRunRecord.output_summary`。
3. **连这 200 字都没人看**：`from_record`（`types.rs:166-181`）写 `StepSnapshot` 时**不读** `output_summary`；`pipeline.rs:117-134` 每步只画 `intent` / `expected_output` / `track`；全仓 `grep output_summary` 在 `crates/agent-gui` **零匹配**。

所以 UI 上现在最接近「结果」的只有 STATS 的聚合指标，**看不到任何一步产出了什么**。

---

## 1. 左侧面板展示 `output_schema`（无依赖）

| 位置 | 改什么 |
|---|---|
| `left_panel/output_schema.rs` | **新建**：渲染块 + 宽容解析（范式照 `params.rs`） |
| `left_panel.rs:174-200` | 解构多取 `template.output_schema`，解析成视图结构 |
| `left_panel.rs:373-422` | 挂第五块（置于 `PipelineView` 与 `HistoryView` 之间） |
| `left_panel/mod.rs:17-24` | 加 `mod output_schema;` |

解析层（纯函数 + 单测，**永不 panic** —— 这段 JSON 由 LLM 生成）：

```rust
struct OutputSchemaView {
    kind: String,            // 六值之一；未知值原样显示，不报错
    description: String,
    detail: Option<String>,
    required: Vec<String>,
    wanted: Vec<String>,
}
```

| 情况 | 渲染 |
|---|---|
| `None`（跳过 / 选「定不了」） | 空态：`未定义输出（可跳过，或在对话里说「定义输出」）` |
| `kind` 未知 | 原样显示该文本 |
| 正常 | `kind` 徽标 + `description` + `detail`（有才显示）+ `required` / `wanted` 两组 chip（空组不显示） |

不引入 Markdown 组件（字段都是短文本，chip 比 Markdown 直观）。

---

## 2. 本次执行结果的直接展示（不落库）

### 2.1 要传的「上下文」——按数据流五层（这是本节的核心）

| # | 层 | 现状 | 要变成 |
|---|---|---|---|
| 1 | 步骤结果 | `step.rs:42-47` `StepRunResult{record, output}`，`output` 不外泄 | 给 `StepRunRecord`（`report.rs:33-60`）加 `output: Option<String>`（**完整**，带上限与截断标记），保留 `output_summary` 作紧凑摘要 |
| 2 | 事件 | `StepFinished{index, record}`（`event.rs:39-42`）已带 record；`RunFinished{report}`（`:44`） | **不变** —— record 带上了 output 后，事件自动携带 |
| 3 | 快照 | `StepSnapshot`（`types.rs:127-145`）无 output；`from_record`（`:166-181`）不读它 | `StepSnapshot` 加 `output: Option<String>`；`from_record` 填充 |
| 4 | 终态重建 | `state.rs:41-48`（StepFinished 写快照）、`:49-79`（RunFinished 用 report **整体重建** steps） | 因第 1 步把 output 放进 `StepRunRecord`，重建路径自动带上，**不会丢** |
| 5 | 任务级结果 | 无此概念 | `RunSnapshot` 加 `result: Option<String>`：终态时取「**出度为 0 的步骤**」的 output（无下游依赖的步 = 交付步）；`step` 依赖关系已在模板 `dependencies` 里，可在 executor 汇总时算 |

另可在 report 里加 `final_output`，但「结果」放快照语义更顺（report 是指标集合）。

**刻意不传的东西**（避免无谓体积）：prompt 全文（内核本就没存）、工具原始返回值（`StepTrackLine::Tool` 只有 `tool/args/ok` —— 见 §2.4）。

### 2.2 展示形态

- **每步**：`pipeline.rs:117-134` 的 step body 里加一个**默认收起的折叠区**展示该步完整 output。折叠范式直接照抄 `tool_view/component.rs:96-125`（`use_signal(open)` + header toggle + `data-open` + 展开区用 `pre` 显示长文本）；若内容是 markdown 可复用 `components/markdown.rs:34`。`RenderedStep`（`left_panel.rs:36-49`）需加 `output: Option<String>`，叠加循环 `:229-237` 取值。
- **任务级**：`StatsView` 之后新增一个 Bento 块「执行结果」，展示 `RunSnapshot.result`；空态区分三种：未执行（`None` 且无快照）/ 执行中 / 执行完但无输出。

### 2.3 体积与推送约束

`RunUpdate` 是**全量快照推送**（`run_service.rs:141-148` 每次 `set` 整个 snapshot），所以每步 output 会随每次事件重复推送。约束：

- 每步 output 上限 **64 KB**（UTF-8 字节），超出截断并在字段上标记 `output_truncated: bool`（UI 显示「已截断」）；
- 该上限写在 `step.rs` 的常量旁（与 `SUMMARY_MAX_CHARS` 并列），便于后续调。

### 2.4 本期不做（明确出界）

- **工具原始返回值**：`StepTrackLine::Tool{tool,args,ok}`（`types.rs:113-123`）不保存返回内容，所以本次「结果」= **步骤的收敛回答（output）**，不是工具拿到的原始数据。要展示「真实抓到的内容」需扩 `StepToolCall` 事件与 `StepTrackLine` —— 留给「执行结果反哺」那期一起做。
- **持久化**：`RunStore` 内存（`run_service/store.rs:26-32`）不动，HISTORY 块继续为空。
- **反哺输出步**：见 §4。

---

## 3. 影响面清单

**第 1 件**
- 新建 `left_panel/output_schema.rs`；改 `left_panel.rs`（`:174-200` 解构 + `:373-422` 挂载）、`left_panel/mod.rs`

**第 2 件（内核 → UI 顺序）**
- `crates/planned-agent/src/flexible/report.rs`：`StepRunRecord` 加 `output` / `output_truncated`
- `crates/planned-agent/src/flexible/step.rs`：`summarize` 旁加 full output 采集 + 上限常量；`StepRunResult` 组装处填值
- `crates/planned-agent/src/flexible/run_service/types.rs`：`StepSnapshot` 加 `output`（+ `from_record` 填充）；`RunSnapshot` 加 `result`
- `crates/planned-agent/src/flexible/run_service/state.rs`：终态算 `result`（出度为 0 的步）
- `crates/planned-agent/src/flexible/executor.rs`：若 `result` 在 executor 汇总更自然，则改此处（待定，按实现时的最小改动选）
- `crates/agent-gui/src/pages/plan/left_panel/left_panel.rs`：`RenderedStep` 加 `output`，叠加循环 `:229-237`
- `crates/agent-gui/src/pages/plan/left_panel/pipeline.rs`：step body 加折叠结果区
- 新建 `left_panel/run_result.rs`（任务级结果块）+ `left_panel.rs:373-422` 挂载 + `mod.rs`
- 测试：内核 `report.rs` / `types.rs` / `state.rs` / `step.rs` 的字面量构造点需补新字段（`report.rs:110`、`state.rs:132`、`executor.rs:272`、`step.rs:258` 等）

---

## 4. 后续（本期不做，另行拍板）

- **执行记录持久化**：新表 `flexible_runs`（status / result / trace / report / 时间戳）+ `PlansFlexibleService::save_run` / `load_latest_run` + 订阅端落库；顺带让 HISTORY 块有内容。
- **执行结果反哺输出契约**：把 `last_run_result` / `execution_trace` 注入 `flexible_output`，提示词在 `question` 里列出真实观察到的字段，选项加 `from_run`；触发走现有「指定步骤直达」（手动），不自动弹窗。
- **工具原始返回值落库**（`TracePipeline` 或扩 `StepTrackLine`）。

---

## 5. 已定（用户拍板）与实施修正

- **结果展示位**：每步折叠区 + 任务级「执行结果」块 ✅
- **折叠区默认状态**：默认收起（与既有 think box / 工具视图的习惯一致）✅
- **任务级「结果」取法**：~~取某一步的 output~~ → 用户修正为「**新增一个输出整理步**，对最后一步的输出做分析，按 `output_schema` 产出结果」。交付步取「最后一个拿到输出的模板步骤」（线性骨架下即最后一步）。

### 实施结果（2026-09）

| 落点 | 文件 |
|---|---|
| 契约定义与校验（唯一处） | `crates/planned-agent/src/flexible/output_schema.rs`（新） |
| 整理步提示词 | `prompt.rs`：`OUTPUT_RESOLVE_SYSTEM_PROMPT` / `build_output_contract_text` |
| 整理步执行 | `step.rs`：`run_output_resolve`（复用 `run_step_with_prompt`）；`executor.rs`：`resolve_result` |
| 结果承载 | `report.rs`（`output` / `output_truncated` / `result`）、`run_service/types.rs`（`StepSnapshot.output`、`RunSnapshot.result`）、`state.rs` |
| 契约块 | `left_panel/output_schema.rs`（新） |
| 结果块 | `left_panel/run_result.rs`（新） |
| 每步产出 | `left_panel/pipeline.rs`：`StepOutput`；`left_panel.rs`：`RenderedStep.output` |
| 占位符覆盖契约 | 两份 `placeholder.rs`：`collect_from_schema`；`save_callback.rs`：`validate(steps, schema, inputs)` |

以下为当时的备选项（留档）：

1. **结果展示位**：每步折叠区 + 任务级「执行结果」块（推荐）／只做任务级块／只做每步折叠？
2. **任务级「结果」取法**：出度为 0 的步骤的 output（推荐，= 交付步）／最后一步（index 最大）／最后**成功**的步骤？
3. **折叠区默认状态**：默认收起（推荐，与 `tool_view` 一致）／成功收起失败展开／默认展开？
