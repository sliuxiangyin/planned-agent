# Flexible 执行器加固设计稿（**不新增持久化**部分）

> **本稿性质**：设计稿，**待用户逐条确认后才动代码**。
> **范围边界**：只收录「进程内可完成」或「复用现有模板落库」的优化 —— 凡需要**新增持久化路径 / 新建表**的，一律不进本稿（见 §1.2），单独排期。
> **上游依据**：`docs/planned-agent/flexible-execution-audit.md`（现状检查）、`flexible-execution-improvements.md`（下称 **imp**，待做清单）、`flexible-run-service.md`。
> 检查日期：2026-09-28。

---

## 1. 范围与边界

### 1.1 本稿覆盖

A 组执行正确性、B 组执行状态机、C 组工具收窄（不落库部分）、D 组守卫 —— 共 16 项，全部可在 `crates/planned-agent/src/flexible/`（少量在 `tool-manager`）内完成。

### 1.2 明确不在本稿（需要新增持久化）

| 事项 | 缺什么 | 出处 |
|---|---|---|
| 从成功轨迹学 `preferred_tools` 并**回写计划** | `PlansFlexibleSessionsRepo::update_step_improvement`（**当前不存在**，已 grep 确认） | `imp` 条目 7 / Q4·Q5 |
| 写回端口 + 学/用开关 + `GuiImprovementStore` | `ImprovementStore` trait + `RunRequest.improvements` | `imp` 条目 6 |
| `intent` 改进建议写回 | 同上（写回路径 + 审核入口） | `imp` 条目 8 |
| 执行记录落库（跨执行度量） | 新表结构（待定案） | `imp` 条目 9 / `flexible-executor.md` 阶段 6 |

> **关键澄清**：`PlanStep` 加字段**不属于**这一类 —— 模板是整段 JSON 写进 `plans_flexible_sessions.parameterized_task`（`save_callback.rs:9`、`plans_flexible_sessions_repo.rs:105-123`），加字段跟着现有 save 流程走，**无新增持久化路径**。所以 C3 / C4 在本稿内。

### 1.3 已作废 / 已否决（不得复活）

| 事项 | 状态 |
|---|---|
| `imp` 条目 3：工具 `description` 按平台生成 | **已否决** —— `system-tools-redesign.md` §N1「description 动态注入平台 → 不采纳」+ §3.4 终审「不注入」（平台事实归**执行期环境段**，`core::host::environment`）。本稿**不收**。 |
| `ExecutorConfig.step_timeout` | 曾在执行器内核阶段被**主动删除**；B1 若要加回须说明理由（这次有实测证据），见 B1。 |
| `imp` 条目 2（环境事实注入） | **已实施**（`prompt::step_system_prompt` + `RuntimeEnvironment`），不重复。 |

---

## 2. 批次与实施顺序

| 批次 | 内容 | 状态 | 理由 |
|---|---|---|---|
| **P0** | C1（工具序列进报告）、A1、A2 | A1 ✅ · A2 ✅ · C1 ✅ | C1 零风险且是后续一切观测的前提；A1/A2 是硬缺陷 |
| **P1** | D1、D2、B1a（取消即时）、B1b（超时 + 超时重试） | B1a ✅ · B1b ✅ · D1/D2 未做 | 确定性守卫 + 一个真实卡死风险 |
| **P2** | A3、A4、A5、B2、C2、C3 | A3 ✅ · A4 ✅ · A5 ✅ · 其余未做 | 语义/预算类，需先定参数 |
| **P3** | B3、B4、D3、C4 | B3 ✅ · 其余未做 | 动 UI / 动其它 crate / 只是文档约定 |

> **A 组（A1–A5）已全部完成**（2026-09-28）。A1/A2/A3 按本节设计直接实施；
> **A4/A5 不是按本节的原设计实现的** —— 原方案（硬截断 + 预算）被判定「不比截取」而放弃，
> 改为**产出落文件 + 下游按需分页读取**，见 `flexible-step-output-spill.md`。
> 回归基线（实测）：`cargo test -p planned-agent --lib flexible::` = **95 passed**；
> `cargo test -p planned-agent-gui --bins` = **63 passed**。

> 每项完成后单独跑 `cargo test -p planned-agent --lib flexible::`；D3 跑 `cargo test -p planned-agent-tool-manager`。

---

## 3. 逐项设计

### A 组 · 执行正确性

#### A1（高）`expected_output` 里的 `${name}` 从未展开  ✅ 已完成（2026-09-28）

- **现状**：`executor.rs:128` 只 `render_step_intent(step, params)`；`step.rs:107` 把 `input.step.expected_output` **原文**交给 `build_step_task`，进 user message。
- **设计**：`StepInput` 增加 `expected_output: &str`（**已展开**）。`executor.rs:128` 处一并渲染 `step.expected_output`，失败走与 `intent` **完全相同**的 `Failed` 路径（同一 match 分支）。
- **刻意不动的**：`StepRunRecord.expected_output`（`step.rs:316`）与 `StepSnapshot`（`types.rs:159`）**仍存原文** —— 记录/UI 展示模板原样是既定语义（`report.rs:40-41`）。**只改发给 LLM 的那一份**。
- **验收**：模板 `expected_output: "写入 ${path}"`，断言请求 user message 含展开值、**不含** `${path}`；`None` 环境段时输出与今天逐字一致（除该字段）。

#### A2（高）空输出被判成功  ✅ 已完成（2026-09-28）

- **现状**：`step.rs:190-202` 收敛时 `output = Some(answer)`，`answer` 可能为空串；`step.rs:295-299` 只看 `output.is_some()` → `Done`。
- **设计**：收敛分支先判 `answer.trim().is_empty()` —— 空则 `error = Some("模型未产出内容（空回答）")`、`output` 保持 `None`（→ `Failed`）。
- **连带**：`deliverable_output`（`executor.rs:412-421`）按「有键」取交付输出，A2 保证 store 里不再出现空串，故**不必改**；但需在代码注释里写明这一依赖关系。
- **验收**：`FakeAiClient` 返回空 content + 空 reasoning → 该步 `status == Failed`、`report.result == None`。

#### A3（中）`dependencies` 指向不存在 / 未产出的引用被静默丢弃  ✅ 已完成（2026-09-28）

- **现状**：`executor.rs` 的 `collect_prior` 用 `filter_map`，未命中直接跳过，**无日志无痕迹**。
- **关键修正（实施时确认，原稿此处写错）**：「前序失败 → 依赖没产出」**走不到 `collect_prior`** ——
  一有步骤非 `Done`，其后每步都直接记 `Skipped`（`executor.rs` 的 `blocked_earlier`），
  而 `Done ⇒ output.is_some() ⇒ 产出一定在 store 里`。所以未命中**只可能是模板静态写错**：
  引用不存在 / 自依赖 / 依赖后面的步骤 / 环 —— **四种都可在 start 时判定**。
- **设计（已按此实施）**：
  1. **主防线 · start 时静态校验**：`collect_dependency_issues(template)` 维护「已出现过的 reference」集合，
     检查每个 `dependencies` 是否都**在本步之前出现**；一条规则覆盖上述四种错误
     （线性执行 + 只允许引用前面 ⇒ 环必然表现为「依赖后面的步骤」）。`run` 开头逐条 `warn`，**不阻断**。
  2. **运行期兜底**：`collect_prior` 改显式循环，未命中时 `warn`（防御性，正常不可达）。
- **不做的**：不因依赖写错失败（对齐 `imp` Q10「默认仅警告」）。
- **前提**：结论依赖「依赖只指向已执行完的前序步骤」+「严格按数组顺序执行」；将来若支持并行 /
  跳步重跑，需重算。
- **验收**：`dependency_issues_cover_missing_self_and_forward_refs`（三种错误都报）、
  `dependency_issues_accept_backward_refs`（合法依赖不报）、`bad_dependency_warns_but_does_not_block`（不阻断执行）。

#### A4（中）`prior` 无长度预算  ✅ 已完成（2026-09-28，经 `flexible-step-output-spill.md` 实现）

> **⚠️ 实际实现与本节的「原设计」不同**：原方案是「硬截断 + 预算」（`PRIOR_ITEM_MAX_CHARS` /
> `PRIOR_TOTAL_MAX_CHARS`），被判定「不比截取」而**放弃**；改为**产出落文件 + 下游按需分页读取**。
> 下面的原设计**保留供追溯，未实现**。

- **现状**：`step.rs:301-302` 明确「`StepRunResult.output` 不截断」；`executor.rs:158` 存入 store → `prompt.rs:154-163` 原样进下游 user message。
- **设计（原方案，未采用）**：新增两个常量，在 `collect_prior` 拼装处生效：
  - `PRIOR_ITEM_MAX_CHARS`（单条上限，**建议 4 000**）；
  - `PRIOR_TOTAL_MAX_CHARS`（总量上限，**建议 12 000**）。
  - 触顶时截断并追加「…（已截断，共 N 字符）」。
- **实际方案（已完成）**：见 `flexible-step-output-spill.md` —— 产出超阈值就写
  `<cache_dir>/<run>/step-<index>.txt`，下游 `prior` 只拿「文件说明 + 预览」，用 `builtin_read_file` 按需读。
- **验收（已完成）**：`large_output_spills_to_file_and_prior_gives_path`、`spill_threshold_boundary_is_inclusive`（见新稿 §12）。

#### A5（中）输出整理步只看到 200 字符摘要  ✅ 已完成（2026-09-28，经 `flexible-step-output-spill.md` 实现）

> **⚠️ 实际实现与本节的「原设计」不同**：原方案（各步改用 `record.output` + 沿用 A4 的总量控制）
> 随 A4 一起放弃。实际做法：各步产出走**同一套「文件说明 + 预览」渲染**，交付产出落盘时
> 整理步改用 `builtin_read_file` 读回。

- **现状**：`executor.rs:350-357` 整理步的 `prior` = 交付步完整输出 + 各步 `output_summary`（`SUMMARY_MAX_CHARS = 200`，`step.rs:25`）。
- **设计（原方案，未采用）**：各步改用 `record.output`（`OUTPUT_MAX_CHARS = 8000` 上限），并沿用 A4 的**总量**控制（可给整理步单独、更宽的额度，如 24 000）。
- **实际方案（已完成）**：整理步 `prior` 改用 `render_prior_output`（未落盘给全文，落盘给「文件说明 + 预览」），
  并给整理步 `builtin_read_file` 工具 —— 原「不带工具」在落盘机制下不成立。
- **验收（已完成）**：`resolve_step_prior_points_at_spilled_file`（见新稿 §12）。

---

### B 组 · 执行状态机

#### B1（高）取消不能即时生效；LLM 请求无超时  ✅ 已完成（2026-09-28）

**实现**（B1a + B1b 一起做，B1b 含超时重试）：

- **落点**：`step.rs` 新增 `request_llm()`（超时 + 超时重试 + 取消即时）、`wait_cancel()`、`with_timeout()`；原裸调用 `ai.chat_completion(request).await` 改为 `request_llm(...)`。模板步与输出整理步**共用**这一条路径。
- **B1a 取消即时**：`tokio::select! { biased; _ = wait_cancel(cancel) => Err("用户取消"), result = with_timeout(...) => result }`。⚠️ `wait_cancel` 在 `cancel` 为 `None` 或发送端已 drop 时**永久挂起** —— 否则该分支会立即完成，把每次调用都误判成「已取消」。
- **B1b 超时 + 重试**：`ExecutorConfig` 新增 `llm_timeout: Option<Duration>`（默认 `Some(180s)`）与 `llm_timeout_retries: usize`（默认 1）。**超时→重试**，用尽次数后该步 `Failed`（错误文本含「已尝试 N 次」）。
- **⚠️ 超时的语义（关键）**：`llm_timeout` 是「**一次 `AiClient::chat_completion` 调用**的墙钟上限」。该调用在 `ai-openai` 内部**本身已有 3 次重试**（`client.rs:558-578`，任何错误都重试），所以超时**包住整次调用**；也正因如此，超时后的重试必须在**这一层**做 —— `timeout` 会把内层 future 一并丢掉，内层没有机会再重试。
- **只重试超时**：其它失败（4xx/5xx/网络）在 `ai-openai` 内部已重试过，这一层**不再叠加**（避免倍数放大：外层 N × 内层 3）。
- **配置**：GUI `[flexible]` 段 `llm_timeout_secs`（**0 = 不限制** ↔ 内核 `None`）与 `llm_timeout_retries`。
- **验收（已完成）**：`cancel_interrupts_in_flight_llm_request`（`HangingAi` + 取消 → 5s 内结束且错误为「用户取消」）、`llm_timeout_retries_and_then_succeeds`（第一次挂 → 重试成功，恰好 2 次尝试）、`llm_timeout_exhausts_retries_then_fails`（一直挂 → 用尽重试后失败，错误含超时与次数）。

#### B2（中）思考轨迹无上限，且随快照全量 clone 广播  ⏸ 暂缓（2026-09-28）

> **决定（2026-09-28，用户）**：**先不处理**。后期若要做，方向是**持久化**（轨迹落库）——
> **不是**本节原设计的「截断」，也**不是**「边显示边丢」。
> 原因：轨迹只用于 UI 显示（生产消费点已核实只有 GUI；内核那两处是 `#[cfg(test)]` 断言），
> 但它**被迫**留在内存 —— `RunSnapshot` 同时兼任「UI 推送源」与「回看源」
> （`store.rs:112-115`：订阅注册后**立刻回放当前快照**），所以快照必须自包含。
> 一旦轨迹持久化，这个自包含约束就解开了，内存只需保留窗口。

- **现状**（2026-09-28 核实）：
  - `step.rs:182-188` 把**完整** `thought` 发进 `StepThought`（有 reasoning 用 reasoning，否则用 content）；
  - `state.rs:28-33` 原样 push 进 `track`，**不截断、不限条数**；`state.rs:45-47` 还**特意**保住轨迹
    （否则 `StepFinished` 的 `from_record` 整体覆盖会把它清掉）；
  - `RunStore::update` 每次做 **3 次**完整 clone：存回表（`store.rs:76`）+ 打包更新（`:86`）
    + **每个订阅者再 clone 一份**（`:94`，在循环内）；
  - 叠加效应：轨迹无限长 × 每次全量复制 ⇒ **平方级**（1+2+…+N ≈ N²/2）。
  - 另有一半同源开销：`step.rs:136` 每轮 `messages.clone()` 也随轮数线性增长（**不发给 LLM**，见下）。
- **两个已核实、将来必踩的事实**：
  1. **reasoning 会变成步骤产出** —— `step.rs:193` 的 `let answer = if !content.is_empty() { content } else { reasoning };`。
     所以将来**若**截断，只能截发给 `StepThought` 的那一份，**不能**连带截断产出。
  2. **reasoning 不进上下文** —— 内存里 `messages.push(message)`（`step.rs:232`）确实带着 `reasoning_content`，
     但 `ai-openai` 转换时 Assistant 分支只取 `content` / `tool_calls` / `name`
     （`client.rs:280-287`，其余被 `..Default::default()` 吃掉）—— **不回传给 LLM**。
- **设计（原方案，暂缓）**：新增 `THOUGHT_MAX_CHARS`（建议 2 000），在 `step.rs:182` 发事件前截断并加「…（已截断）」。
- **后期方向（持久化）**：轨迹落库后 `RunSnapshot` 不必再自包含，内存只留窗口 + 从库回看。
  与 §1.2「需要新增持久化」那一类同级 —— 要**建表 / 定保留策略 / 定回看语义**（例如：轨迹只对本次执行有效，
  还是结束后仍可读回；跨执行是否保留）。

#### B3（中）`start()` 被忽略时宿主无感  ✅ 已完成（2026-09-28）

- **现状（改造前）**：`service.rs` 无返回值；`core.rs:121-128` 重复启动只 `tracing::warn!`，服务未运行也只 `warn!`。
  两条路径都**只写日志**：宿主点了没反应，分不清「服务挂了」与「已在跑」。
- **设计（已实现）**：
  - `RunUpdate` 由**结构体改为枚举**：`Snapshot { session_id, snapshot }` / `Notice { session_id, notice }`。
    两者互斥，用枚举让宿主的 `match` 天然穷尽（漏处理**编译不过**，而非运行期静默）。
  - 新增 `RunNotice::StartRejected { reason: StartRejectReason }`，`reason` = `ServiceNotRunning` | `AlreadyRunning`。
    **只给原因、不给文案**：内核不产出 UI 文本，提示语由宿主决定。
  - `RunStore::notify()`：只广播、**不动 `runs`**（与 `update()` 共用私有 `broadcast()`，
    锁顺序 `runs → subs` 不变）。订阅回放时因此不会冒出一条假快照。
  - 两处拒绝点各发一条：`core.rs` 重入检查（`AlreadyRunning`）、`service.rs` 命令送不出去（`ServiceNotRunning`）。
    ⚠️ `service.rs` 那处必须**先 clone `session_id`** —— 命令送不出去时 `request` 会随 `SendError` 一起被退回。
  - **不**把拒绝写进 `RunSnapshot`：快照的语义是「某一次执行的状态」，而拒绝意味着这次根本没发生。
- **宿主侧**：`use_run_subscription` 内 `use_toast()`，收到 `Notice` → `toast.error(...)`；
  文案在 GUI 的 `notice_text()`（「执行服务未在运行…」/「该计划正在执行中…」）。
- **验收（已通过）**：
  - `rejected_start_notifies_subscriber` —— 重复 `start()` → 订阅者收到 `StartRejected{AlreadyRunning}`，
    且快照**仍是在跑的那次**（拒绝不改状态）；
  - `notify_does_not_touch_snapshot` —— 通知不改快照，且不以假快照形式出现；
  - `notify_only_reaches_matching_subscribers` —— 不串台。

#### B4（低）`PlanRunReport` 无终态字段（建议**不加字段**）

- **现状**：`report.rs:76-91` 只有 `success`；「取消」只体现在 `RunSnapshot.status`（`core.rs:227-229`）。
- **设计**：**不加 status 字段**（避免「报告」与「快照」两个真值来源），改为在 `report.rs` 的文档注释里**明确约定**：「报告不含终态；取消/失败的区分一律读 `RunSnapshot.status`」。
- **验收**：文档约定落地；无代码行为变化。

---

### C 组 · 工具收窄（本稿核心）

#### C1（P0）工具序列进报告  ✅ 已完成（2026-09-28）

- **现状（改造前）**：`step.rs` 已发 `StepToolCall` 事件，`state.rs` 存进快照 `track`；
  但 `StepRunRecord` 只有 `tool_calls: usize`（**次数**）—— 「那 5 次是哪 5 个工具、在第几步、传了什么」
  一个都答不出来。
- **设计（已实现）**：
  - `report.rs` 新增 `ToolCallRecord { tool, args, ok }`（`Serialize/Deserialize`）；
    `StepRunRecord` 加 `#[serde(default)] pub tool_sequence: Vec<ToolCallRecord>`。
  - **采集点**：`step.rs` 发 `StepToolCall` 事件**同一处** push 进局部 `Vec`，末尾塞进 `record`
    —— **一处采集、两条出口**（事件 → 快照 `track` → think box；这里 → 报告）。
  - ⚠️ 实现细节：`tool_name` / `args_line` 原本是**被 move 进事件**的，所以 push 必须在 `emit`
    **之前** clone，否则拿不到。顺手把 `ok = !is_error` 提成一个局部变量，两处共用。
  - **与 `imp` 条目 1 的偏差（需知）**：`imp` 写「数据从 `StepSnapshot.track` 派生（单一数据源）」，
    但 `StepRunRecord` 由 `step.rs` 产出、`track` 在 `state.rs`（executor 之外），跨层派生反而要新增通路。
    **改为在采集点同处产出**，仍是单一来源、更简单。
  - **`args` 用哪个**：记录用 `describe_arguments`（全量 JSON，排查用），事件/快照 `$` 行继续用
    `describe_tool_args`（120 字符，展示用）—— 两者用途不同，**不合**。
- **收尾**：`executor.rs` 的「步骤结束」/「步骤失败」日志新增 `tools=` 字段
  （`summarize_tools`：去重后的工具名，按首次出现顺序）—— 此前只有 `tool_calls=3`（次数），
  看不出「调了什么」。这是本项在 UI 之外的**直接可见出口**。
- **验收（已通过）**：`report_records_tool_sequence_per_step` —— 一步内两次调用（一成功、一工具层报错），
  断言两条记录**按发生顺序**、工具名/入参/`ok` 都对得上；未调工具的步骤为空序列。
- **⚠️ 仍未做的消费点**：GUI 的 `stats.rs` 只读 `report.tool_calls`（仍是数字），**没有**读 `tool_sequence`；
  `StepSnapshot` 也**未**带上该字段。要做「界面里看见工具序列」需另开一项（见下）。

#### C2（P2）按「整次任务」收窄工具表（形态待定）

- **动机**：这是唯一**不需要知道每步要什么**的收窄手段 —— 只需知道「这不是浏览器任务 / 这不是设备任务」。日志里 24 个 playwright 工具全带、一次没用，就靠这条排掉。
- **难点**：`select_tools_by_tokens`（`chat/tools/mod.rs:52-93`）只有**正向** token，`"all"` 仅排除 `Utility`/`SubAgent`，**无法表达「排掉 Browser」**。
- **三个候选形态（见 §5-Q1）**：
  | 形态 | 做法 | 代价 |
  |---|---|---|
  | ① 宿主给正向列表 | 宿主/用户配 `allowed_tools = ["File","System","Text"]` | 宿主得知道要什么；但零新机制 |
  | ② 引入负向 token | `"-Browser"` 追加语义；改 `select_tools_by_tokens` | 改公共语义，影响 chat 侧 |
  | ③ 运行时判断 | 执行前按任务/intent 关键词或一次 LLM 判定 | 精度差 / 引入执行期 LLM |
- **建议**：先做 ①（零新机制，且 `ExecutorConfig.allowed_tools` 已存在，只是宿主现在传 `None`）；②③ 留到 `imp` Q2/Q3 定案。
- **验收**：给 `["File","System"]`，断言请求 `tools` 不含任何 `Browser` 类工具。

#### C3（P2）`PlanStep.allowed_tools` + 执行器按步算

- **引用**：`imp` §5.3 / 条目 4 已给字段定义（token 语义复用 `select_tools_by_tokens`），本稿**只补三点**：
  1. **`Some([])` 语义必须钉死**：`select_tools_by_tokens` 对空 tokens 返回**空工具表**（`mod.rs:88-92`），而 planner 的 `resolve_tools` 对空返回**全部**（`tool_executor.rs:38-42`）——**两侧相反**。`flexible` 取前者（`Some([])` = 纯推理步），须在 `template.rs` 文档注释里写明，防「照抄 planner」写反。
  2. **`None` 必须回落 `cfg.allowed_tools`**（`executor.rs:283-286`），保证旧模板行为逐字不变。
  3. **落点**：`tool_definitions()`（`executor.rs:281-288`）→ `tool_definitions_for(step, cfg)`；`run()` 里 `let tools = self.tool_definitions();`（`executor.rs:101`）**移进循环**（每步一次；轮内复用，不每轮重算）。
- **与计划期契约的冲突（必须知）**：`prompts/flexible/flexible_plan.toml:25-26` 明令「不指定具体工具名…执行器自行决定」，`:35` 规定 steps「恰好四个字段」。→ 若让 LLM 产出要走改 prompt；否则只能由用户手填（C4）。**这正是 `imp` Q3 未决的原因。**
- **验收**：`None` 回落 / `Some([])` 无工具 / `Some(["File"])` 取子集 / 未知 token 忽略（4 个用例）。

#### C4（P3）左面板手填每步工具集

- **依赖 C3**。落点 `crates/agent-gui/src/pages/plan/left_panel/`（PIPELINE 每步加「工具范围」：分类多选 + 精确名）。落库走现有 save 流程（模板整段 JSON）。
- **验收**：手填后重载会话，工具范围仍在；执行时该步请求 `tools` 随之变化。

---

### D 组 · 守卫

#### D1（P1）重复调用守卫

- **先例（照抄）**：`planner/react/default_react_agent.rs:478-502` —— `action_sig = "{tool}:{parameters}"`，连续相同则 `repeat_count += 1`，`>= max_repeats(3)` 即中断并记 failure。
- **设计**：`step.rs` 循环内维护 `(tool, 规范化 args)` 签名，连续 3 次相同 → 中断该步，`error = Some("检测到重复调用同一工具同参数 3 次")`。签名用 `describe_arguments`（全量 JSON）保证同参数可判。
- **验收**：`FakeAiClient` 连续返回同一 tool_call → 该步在 3 次内 `Failed`，不撞 `max_rounds_per_step`。

#### D2（P1）启动前 lint

- **设计**：`executor.run` 开头（或新 `flexible/lint.rs`）校验，**默认仅警告**（对齐 `imp` Q10）：
  - `result_reference` 唯一；
  - `dependencies` 引用的 reference 存在；
  - 无环；
  - `intent` / `expected_output` 的 `${name}` 都有 `inputs` 定义（复用 `placeholder::validate`，`placeholder.rs:93`）；
  - **新增**：`allowed_tools` 的 token 合法（分类名 or 存在的工具名 or `"all"`）。
- **验收**：每条违反各有一个可复现单测，且**不阻断执行**（只 warn）。

#### D3（P2，跨 crate）工具层参数校验 + 结构化错误

- **落点**：`crates/tool-manager/`（内置工具），把 OS 原文错误换成结构化原因（如「path 是相对路径，请传绝对路径」）。
- **本稿只登记，不细化** —— 跨 crate，单独排期（`imp` 条目 5c）。

---

## 4. 验收总表

| 项 | 命令 / 方式 |
|---|---|
| A1 / A2 / A3 / A4 / A5 | `cargo test -p planned-agent --lib flexible::` 新增用例 |
| B1a | `SlowAi` + `stop()`，断言终态在 1s 内到达 |
| B2 | 构造超长 reasoning，断言快照尺寸受控 |
| C1 | `--lib flexible::report` + 一次真跑看 report |
| C2 / C3 | `--lib flexible::executor` 新增用例（工具表随步变化） |
| D1 / D2 | 各自可复现单测 |
| D3 | `cargo test -p planned-agent-tool-manager` |

> **回归基线**：`cargo test -p planned-agent --lib flexible::`（现 55 项）+ `cargo test -p planned-agent-gui --bins`（现 59 项）。
> 注意 `planner::coarse::llm_planner` 有 3 个**既有失败**（prompt 目录漂移），与本稿无关。

---

## 5. 待拍板点

| # | 问题 | 我的建议 |
|---|---|---|
| **Q1** | C2 按整次任务收窄用哪种形态？① 宿主给正向列表 ② 引入负向 token ③ 运行时判断 | **① 宿主给正向列表**（零新机制；`ExecutorConfig.allowed_tools` 已在，只是宿主现在传 `None`） |
| **Q2** | 本批范围？只做 P0（C1+A1+A2）／ 做到 P2 ／ 全做（含 C3、C4、D3） | 先 **P0+P1**，P2 里的 A4/A5/B2/C2 需先定参数 |
| **Q3** | ~~B1b 是否加「单次 LLM 请求超时」？~~ | **已拍板（2026-09-28）**：加，且**超时后重试**。默认 `180s` + 重试 1 次；GUI 侧 `llm_timeout_secs = 0` 表示不限制。 |
| **Q4** | A4 的预算值（`PRIOR_ITEM_MAX_CHARS` / `PRIOR_TOTAL_MAX_CHARS`） | 建议 4 000 / 12 000（整理步单独 24 000） |
| **Q5** | `max_rounds_per_step` 是否从 50 调小？调到多少 | 建议 15，但**不属本稿必做**，可放到 C 组之后 |

---

## 6. 不在本稿（需要新增持久化的部分）

从成功轨迹「学」工具集 → **回写计划** 是唯一真正需要新增持久化的一环：

```
执行 → C1 汇总工具序列（本稿，不落库）
     → 提取「首次 ok=true 的工具」          ← 纯函数，不落库
     → 回写 PlanStep.preferred_tools        ← 需要 update_step_improvement（新方法）+ 开关 + 写回端口
     → 下次执行作为「软提示」注入            ← 不落库，但当次由模板读入
```

**结论**：本稿把这条链的「上游（观测）」做到位；「回写」等持久化方案定案后再开工。这也符合 `imp` §9 的依赖链（条目 1 是一切观测的前提）。

---

## 附：本稿与 `imp` 的编号对照

| 本稿 | `imp` 出处 | 关系 |
|---|---|---|
| A1 / A2 / A3 / A4 / A5 | §2.1-A2·A4·B2·D4 | 本稿**新发现**，`imp` 只把它们列为成因/只观测 |
| B1 | §2.1-F | `imp` 归「不可治」，本稿认为可确定性修（B1a） |
| B2 | — | 本稿新增（宿主侧资源） |
| B3 / B4 | — | 本稿新增 |
| C1 | 条目 1 | **同一件事**，本稿补采集点偏差说明 |
| C2 | §2.1-C1 | 本稿新增「负向 token 缺失」这一难点 |
| C3 | §5.3 / 条目 4 | **同一件事**，本稿补 3 个语义要点 |
| C4 | 条目 4 的 UI 面 | 同源 |
| D1 | §2.1-D1 / 条目 5a | 同一件事 |
| D2 | §2.1-B5 / 条目 5b | 同一件事 + 新增 `allowed_tools` 校验 |
| D3 | §2.1-C4 / 条目 5c | 同一件事，跨 crate |
| ~~E1~~ | ~~条目 3~~ | **已否决**（§1.3），不纳入 |
