# Flexible 执行器现状检查（诊断稿）

> **本稿性质**：对 `crates/planned-agent/src/flexible/` 的**只读审计**，不改代码。
> 依据 = 一份真实执行日志（`crates/agent-gui/logs/gui.log.2026-09-28`，一次 4 步 / 45.6s / 83891 prompt tokens 的成功执行）
> ＋ 逐文件读码。**未运行测试、未改任何文件。**
> **待用户逐条拍板**，拍板前不动代码（见 §5）。
>
> 检查日期：2026-09-28。

---

## 1. 执行结果速览（日志事实）

| 项 | 值 |
|---|---|
| 终态 | `Succeeded`（`run_service/core.rs` 记「执行到达终态 status=Succeeded」） |
| 模板步 | 3 步；实际执行 4 步（含末尾追加的**输出整理步**） |
| 轮数 | step1=3、step2=1、step3=4、整理步=1 → 共 9 轮 LLM 请求 |
| 工具调用 | 5 次（全部为 `builtin_execute_command` / `builtin_read_file`） |
| token | prompt=83891、completion=1698 —— **≈9.3k prompt tokens / 轮** |
| `allowed_tools` | `None`（→ 全部工具，日志「总计 44 工具（内置 19 / MCP 24）」） |

**一句话**：链路是通的，但**「读一个文件尾行 + 追加一行」花了 8.4 万 prompt tokens**，且下面有一批正确性 / 状态管理问题未覆盖。

---

## 2. 与既有设计稿的关系（先划清，避免重复立项）

本仓`docs/planned-agent/` 已有两份高度相关的稿子，本轮的发现分三类：

| 类别 | 含义 | 本稿处理 |
|---|---|---|
| **已立项·未实施** | `flexible-execution-improvements.md` 的待做清单 / 拍板点里已列，代码里还没做 | §3 逐条引用条目号，**不重复设计** |
| **新发现** | 上述稿子未覆盖的问题 | §4 给出证据与建议修法 |
| **文档漂移** | 稿子描述与当前代码不符 | §4 末尾单列，并指明以代码为准 |

> 特别提示：`flexible-execution-improvements.md` §2.1 已把「假成功」「无重复检测」「工具选择试错」「网络/挂起」列为**成因**，
> 但把它们归到了 **D4「只观测」/ F「不可治」**。本稿 §4 认为其中两条（空输出判成功、无超时）**是可确定性修掉的**，不属「只观测 / 不可治」。

---

## 3. 已立项但尚未实施（现状即「已知缺口」）

以下每条都已在 `flexible-execution-improvements.md`（下称 **imp**）里立项，此处只记「现状 + 与日志的对照」，**不另出设计**。

| # | 事项 | imp 出处 | 现状（代码） | 本轮日志佐证 |
|---|---|---|---|---|
| P1 | **按步收窄工具** | §5.3、条目 4、Q2/Q3 | `ExecutorConfig.allowed_tools` 是**整次执行级**（`executor.rs:35`）；`tool_definitions()` 一次算全集（`executor.rs:283-287`） | `allowed_tools=None` → 24 个 playwright 浏览器工具全带，一次没用 |
| P2 | **重复调用守卫** | §2.1-D1、条目 5a、Q9 | `flexible/` 全模块**无重复检测**（planner 有：`default_react_agent.rs` `max_repeats=3`） | 本次无触发；风险仍在（同类工具反复试） |
| P3 | **启动前 lint（依赖存在性 / `result_reference` 唯一 / 无环 / 占位符有定义）** | §2.1-B5、条目 5b、Q10 | 无 lint；模板只做 serde 解析（`template.rs`） | — |
| P4 | **工具调用序列进报告 / UI**（D1/D3/D4 的观测前提） | §2.1-D3、§2.3、条目 1 | 序列只活在 `StepSnapshot.track`（`state.rs:34-40`），`StepRunRecord` 无该字段（`report.rs:33-66`） | think box 有轨迹，STATS 无序列 |
| P5 | **工具 `description` 按平台生成** | §2.1-C1、条目 3 | 静态描述（另见记忆 `system-tools-description-no-platform-injection`） | — |
| P6 | **`max_rounds_per_step = 50` 是否调小** | §2.1-D2、Q11 | 默认 50（`executor.rs:41`） | — |
| P7 | **执行记录落库（阶段 6）** | `flexible-executor.md` 阶段 6 | 未做（「待表结构定案」） | — |

> `flexible-run-service.md` §12.6 另有「`ExecutorConfig` 语义混杂」列为**长期清理项**，与本稿 §4-B 组同域。

---

## 4. 本轮新发现

### A 组 · 执行正确性

#### A1（高）`expected_output` 里的 `${name}` **从不展开**

- **证据**：契约明确允许占位符出现在 `expected_output`（`placeholder.rs:5,17` 的 `PLACEHOLDER_FIELDS`，且 `validate()` 会校验它），
  但执行期**只渲染 `intent`**（`executor.rs:128` `render_step_intent(step, params)`），
  而发给 LLM 时用的是**模板原文**：`step.rs:107` `build_step_task(input.intent, &input.step.expected_output, input.prior)`。
- **后果**：`expected_output` 含 `${file_path}` 的模板，会在「## 期望产出」段里把**未展开的 `${file_path}`** 交给模型。
  本仓自带样例就长这样：`template.rs:96` `"expected_output": "${file_path} 末尾新增一行"`。
- **注**：`report.rs:41` / `run_service/types.rs:136` 都注明记录侧「原样保留，未展开」——**记录保留原文是对的，但 prompt 用的应是展开值**，当前两者共用一个字段。
- **建议修法**：执行期单独渲染一份 `expected_output`（复用 `params.render`），渲染失败按 `intent` 同路径记 `Failed`；记录仍存原文。

#### A2（高）**空输出被判成功**

- **证据**：收敛分支 `step.rs:191-202` 在「无工具调用」时直接 `output = Some(answer)`，其中 `answer` 可能为空串
  （`content` 与 `reasoning` 都为空时）；状态判定 `step.rs:295-299` 只看 `error.is_none() && output.is_some()` → `Done`。
- **后果**：`Some("")` 会 ① 进 `store`（`executor.rs:176-178`）传给下游 `prior`；② 被 `deliverable_output` 选为交付输出
  （`executor.rs:412-421`，它按「有键」而非「非空」判定）；③ 成为 `report.result`。
- **关系**：这是 imp §2.1-D4「假成功」的**硬特例**（连输出都没有也算成功）。imp 把 D4 归为「只观测」，本稿认为**该条可直接守住**。
- **建议修法**：`output` 取 `answer` 前 trim，空则视为该步失败（`error = "模型未产出内容"`）。

#### A3（中）`dependencies` 指向不存在 / 未产出的引用被**静默丢弃**

- **证据**：`executor.rs:445-454` `collect_prior` 用 `filter_map`（`store.get(reference)` 为 `None` 直接跳过）。
- **后果**：模板写了 `dependencies: ["#E9"]`（不存在），或前序步失败导致 `#En` 未进 store 时，
  该依赖**无声消失**，模型只看到「没有前序结果」→ 可能臆造数据。
- **关系**：imp §2.1-A2/B5 的 lint 可覆盖「引用了却没写进 dependencies」这一侧；**「写了但不满足」这一侧未被 lint 覆盖**（P3 未实施，故现状是静默）。
- **建议修法**：`collect_prior` 对每个未命中的 reference 记 `tracing::warn!`，并在该步 `record.error` 或事件里留痕；是否让该步直接失败由 P3 的 lint 策略（Q10「仅警告 vs 拒绝启动」）统一决定。

#### A4（中）`prior` **无长度上限**，且与 record 的 8000 截断不一致

- **证据**：`step.rs:301-302` 明确「`StepRunResult.output` 保持**不截断**（下游 `prior` 依赖它）」，
  而执行记录里存的是 `truncate_chars(text, OUTPUT_MAX_CHARS=8000)`（`step.rs:31,303-309`）。
  `prior` 经 `executor.rs:158` → `prompt.rs:154-163` 原样进 user message。
- **后果**：一步若读到很大的文本（大文件 / 大 JSON），完整内容会全部塞进**下游每一步**的 user message，可能撑爆上下文或撞 provider 上限。
- **文档漂移**：imp §2.1-A4 写「输出传播是全量（**≤8000 字符**）」，与代码不符 —— **实际不截断**。以代码为准（见 §4-D1）。
- **建议修法**：给 `prior` 定总预算（如每步 N 字符 + 标明截断），与 record 的 8000 分开决策。

### B 组 · 执行状态机 / 宿主侧（`run_service`）

> 这一组 imp 完全未覆盖 —— 它只谈执行质量，不谈宿主侧状态与资源。

#### B1（高）状态表**只增不减**，且每次事件**全量 clone + 广播**

- **证据**：
  - `RunStore.runs` 是 `HashMap<SessionId, RunSnapshot>`，**没有删除 / 淘汰路径**（`store.rs:28-31`；`update` 只在闭包返回 `None` 时移除，`store.rs:67-81`）。
  - 每次 `update` 都 `snapshot.clone()` 并**广播给所有匹配订阅者**（`store.rs:83-107`）。
  - `StepTrackLine::Thought { text }` 存的是**完整 reasoning 文本、无截断**（`types.rs:113-124`，写入见 `state.rs:28-33`；`step.rs:183-188` 直接传完整 `thought`）。
- **后果**：MiniMax-M3 这类推理模型 + 多会话长跑，`RunSnapshot`（每步 `output`≤8000 + 每步 `track` 全量 reasoning）会**单调增长**，
  且每个事件都触发一次 O(snapshot) 复制 + 广播。这是**内存与推送量的双重无界**。
- **建议修法**：① `track` 单行/单步加上限（与 `output` 同规格）；② 终态后可回收 `track` 或整份快照（或给 `runs` 加容量 / TTL）；③ 评估「增量推送 vs 全量快照」是否要改（涉及 UI 契约，需拍板）。

#### B2（高）取消**不能立即生效**；LLM 请求**无超时**

- **证据**：
  - 取消只在两个检查点被检查：轮开始前（`step.rs:121-125`）与工具循环中（`step.rs:223-227`）；
    进行中的 `ai.chat_completion(request).await`（`step.rs:138`）**没有被 `select!` 包住**。
  - `ai-openai` 全 crate **没有任何请求超时**（grep `.timeout(` 无命中；`client.rs:575` 是唯一一处 `sleep`，属重试）。
- **后果**：① 用户点「停止」后，最坏要等当前那一次 LLM 请求返回（日志里 step1 单轮最长 ~16s）；② provider 若挂起不返回，**会话被永久锁死**，`stop()` 也救不回来。
- **关系**：imp §2.1-F 把「网络 / 超时噪声；工具本身慢或挂」列为**「不可治」**。本稿认为「给 LLM 请求配超时 + 把 await 包进 `select!`」是**确定性可做**的，不应归入不可治。
- **建议修法**：给 `ExecutorConfig` 加 `request_timeout`（或复用 provider 配置），执行期用 `tokio::select! { res = chat, _ = cancel.changed() => ... }`。

#### B3（中）`start()` 被忽略时**宿主无任何回传**

- **证据**：`RunService::start` 无返回值（`service.rs:33-37`）；重复启动只在 `core.rs:121-128` `tracing::warn!`；
  服务未运行时只在 `service.rs:34-36` `tracing::warn!`。
- **后果**：宿主（GUI）点了「执行」但被拒（同会话已在跑 / 服务未起），界面**无感**，只能看日志。
- **建议修法**：`start` 返回 `Result` 或加一个「启动被拒」事件，由宿主呈现。

#### B4（低）`PlanRunReport` **无终态字段**，取消与失败同形

- **证据**：`report.rs:76-91` 只有 `success: bool`，没有 status；「取消」只体现在 `RunSnapshot.status`（`run_service/core.rs:227-229`）。
- **后果**：只看报告无法区分「执行失败」与「用户取消」；UI 的 STATS 块若取报告会显示成同一种「失败」。
- **建议修法**：给报告加一个终态枚举，或明确约定「报告不含终态，一律读 `RunSnapshot.status`」并写进文档。

#### B5（中）输出整理步**只看到各步 200 字符摘要**

- **证据**：整理步的 `prior` = 交付步完整输出 + 各步 `output_summary`（`executor.rs:350-357`），而摘要上限 `SUMMARY_MAX_CHARS = 200`（`step.rs:25`）。
- **后果**：若关键信息在**非交付步**（例如 step1 读到的原始数据），整理步只拿到 200 字符，可能因此「未获得」或结论失真。
- **关系**：imp §2.1-A4 提到「摘要另有一份 200 字符」但归为「只观测」，未指出「整理步因此可能看不到数据」这一具体后果。
- **建议修法**：整理步的 `prior` 改用各步**完整 output**（或提高摘要上限并标注截断）。

### C 组 · 可观测性

#### C1（低）`task` 日志截断 80 字符且与多行 `system_prompt` 糊行

- **证据**：`executor.rs:94` `template.task.chars().take(80)`；`executor.rs:97` `system_prompt = %system_prompt`（`%` Display，多行原样输出）。
- **现场**：日志开头 `…获取最后一行的 system_prompt=你是一个计划执行助手…` —— task 后半句被截，且与 system prompt 首行连成一行。
- **建议修法**：task 日志不要截断（或截断到词边界并加省略号）；多行字段单独成行或加分隔标记。

---

## 5. 待拍板点

> 建议按「先修确定性小缺陷 → 再动状态机 → 最后碰效率策略」的顺序拍板。

| # | 问题 | 默认建议 |
|---|---|---|
| D1 | **A1 `expected_output` 展开**是否本轮就修 | **修**（小改动、语义明确、可加回归测试） |
| D2 | **A2 空输出判成功**是否本轮就修 | **修**（trim 后判空即失败） |
| D3 | **A3 依赖缺失**的处置：仅告警 / 该步失败 / 交给 P3 的 lint 统一 | **仅告警 + 留痕**（与 imp Q10 一致，避免误判挡路） |
| D4 | **A4 `prior` 预算**：定多少、超限如何标注 | 需你给上限值；我建议「与 record 同规格 8000 + 明确标注截断」 |
| D5 | **B1 状态回收**：`track` 加上限 / 快照 TTL / 改增量推送 | 先做**最小项**：`track` 单行加上限；推送形态留待评估 |
| D6 | **B2 超时**：超时值、超时后语义（该步失败 vs 重试） | 加 `request_timeout`（值待定）＋ `select!` 让取消即时 |
| D7 | **B5 整理步输入**：改用各步完整 output 是否可接受（token 上升） | **改**（结果正确优先） |
| D8 | **B3/B4** 是否本轮一并处理 | 可延后（不改执行语义，只影响宿主可见性） |
| D9 | 是否把 §3 的 P1（按步收窄工具）**提前**到本轮——它是 8.4 万 token 的**主因** | 需你拍板（imp Q2/Q3 未定，属跨稿决策） |
| D10 | 本稿全部**只诊断不改码**，是否等你逐条确认后再开实施 | **是** |

---

## 6. 复现 / 验证方式（供拍板后实施时用）

- **A1**：构造 `expected_output: "写入 ${path}"` 的模板跑 `FlexibleExecutor::run`，断言第三次请求的 user message **不含** `${path}`（当前会含）。
- **A2**：`FakeAiClient` 返回空 content + 空 reasoning，断言该步 `status == Failed`（当前为 `Done`）。
- **A3**：模板 `dependencies: ["#E9"]`，断言有 warn（当前无）。
- **A4**：造超 8000 字符的 step1 输出，断言 step2 的 user message 长度受控（当前不受控）。
- **B1**：`RunStore` 单测——反复 `update` 后断言快照尺寸 / `track` 行数有上限（当前无上限）。
- **B2**：`SlowAi` 永不返回，断言 `stop()` 后会话能在超时内到终态（当前会挂住）。
- **B5**：非交付步产出长文本，断言整理步请求里含该步完整输出（当前只有 200 字符）。

---

## 附：关键证据行号索引

| 结论 | 位置 |
|---|---|
| `allowed_tools` 整次级；工具定义一次算全集 | `flexible/executor.rs:35`、`:283-287` |
| 每轮重发全部工具 | `flexible/step.rs:131` |
| `expected_output` 原样进 prompt | `flexible/step.rs:107`；`flexible/prompt.rs:143-165` |
| 只渲染 `intent` | `flexible/executor.rs:128`；`flexible/params.rs:73-77` |
| 空输出 → `Some("")` → `Done` | `flexible/step.rs:191-202`、`:295-299` |
| 依赖缺失静默丢弃 | `flexible/executor.rs:445-454` |
| `prior` 不截断 / record 截 8000 | `flexible/step.rs:301-309`、`:31` |
| 摘要 200 字符进整理步 | `flexible/step.rs:25`；`flexible/executor.rs:350-357` |
| 整理步无工具 | `flexible/executor.rs:370-384` |
| 取消检查点（非 await 内） | `flexible/step.rs:121-125`、`:223-227` |
| LLM 调用无超时 | `crates/ai-openai/src/client.rs`（全 crate 无 `.timeout(`） |
| `track` 无截断、无上限 | `flexible/run_service/types.rs:113-124`、`state.rs:28-33` |
| 快照全量 clone + 广播 | `flexible/run_service/store.rs:67-108` |
| `runs` 无淘汰 | `flexible/run_service/store.rs:28-31` |
| `start` 无回传 | `flexible/run_service/service.rs:33-37`；`core.rs:121-128` |
| 报告无终态字段 | `flexible/report.rs:76-91` |
| `task` 不进任何 prompt | `flexible/prompt.rs:143-165`（只组 intent/expected_output/prior） |
| 日志 `task` 截 80 | `flexible/executor.rs:94`、`:97` |
