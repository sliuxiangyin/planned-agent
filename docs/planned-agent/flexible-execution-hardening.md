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

| 批次 | 内容 | 理由 |
|---|---|---|
| **P0** | C1（工具序列进报告）、A1、A2 | C1 零风险且是后续一切观测的前提；A1/A2 是硬缺陷 |
| **P1** | D1、D2、B1a（取消即时） | 确定性守卫 + 一个真实卡死风险 |
| **P2** | A3、A4、A5、B2、C2、C3 | 语义/预算类，需先定参数 |
| **P3** | B3、B4、D3、C4 | 动 UI / 动其它 crate / 只是文档约定 |

> 每项完成后单独跑 `cargo test -p planned-agent --lib flexible::`；D3 跑 `cargo test -p planned-agent-tool-manager`。

---

## 3. 逐项设计

### A 组 · 执行正确性

#### A1（高）`expected_output` 里的 `${name}` 从未展开

- **现状**：`executor.rs:128` 只 `render_step_intent(step, params)`；`step.rs:107` 把 `input.step.expected_output` **原文**交给 `build_step_task`，进 user message。
- **设计**：`StepInput` 增加 `expected_output: &str`（**已展开**）。`executor.rs:128` 处一并渲染 `step.expected_output`，失败走与 `intent` **完全相同**的 `Failed` 路径（同一 match 分支）。
- **刻意不动的**：`StepRunRecord.expected_output`（`step.rs:316`）与 `StepSnapshot`（`types.rs:159`）**仍存原文** —— 记录/UI 展示模板原样是既定语义（`report.rs:40-41`）。**只改发给 LLM 的那一份**。
- **验收**：模板 `expected_output: "写入 ${path}"`，断言请求 user message 含展开值、**不含** `${path}`；`None` 环境段时输出与今天逐字一致（除该字段）。

#### A2（高）空输出被判成功

- **现状**：`step.rs:190-202` 收敛时 `output = Some(answer)`，`answer` 可能为空串；`step.rs:295-299` 只看 `output.is_some()` → `Done`。
- **设计**：收敛分支先判 `answer.trim().is_empty()` —— 空则 `error = Some("模型未产出内容（空回答）")`、`output` 保持 `None`（→ `Failed`）。
- **连带**：`deliverable_output`（`executor.rs:412-421`）按「有键」取交付输出，A2 保证 store 里不再出现空串，故**不必改**；但需在代码注释里写明这一依赖关系。
- **验收**：`FakeAiClient` 返回空 content + 空 reasoning → 该步 `status == Failed`、`report.result == None`。

#### A3（中）`dependencies` 指向不存在 / 未产出的引用被静默丢弃

- **现状**：`executor.rs:445-454` `collect_prior` 用 `filter_map`，未命中直接跳过，**无日志无痕迹**。
- **设计**：`collect_prior` 返回 `(prior, missing: Vec<String>)`。`missing` 非空时：
  1. `tracing::warn!(step = index, missing = ?missing, "依赖未产出，已从 prior 中跳过")`；
  2. 在 `prior` 里**追加一条说明段**（如「注意：以下依赖未产出 —— `#E9`；若无数据请勿臆造」），让 LLM 也知道缺口。
- **不做的**：本步**不因此失败**（是否失败交给 D2 的 lint，对齐 `imp` Q10「默认仅警告」）。
- **验收**：模板 `dependencies: ["#E9"]`，断言产生 warn，且 user message 含「未产出」说明段。

#### A4（中）`prior` 无长度预算

- **现状**：`step.rs:301-302` 明确「`StepRunResult.output` 不截断」；`executor.rs:158` 存入 store → `prompt.rs:154-163` 原样进下游 user message。
- **设计**：新增两个常量，在 `collect_prior` 拼装处生效：
  - `PRIOR_ITEM_MAX_CHARS`（单条上限，**建议 4 000**）；
  - `PRIOR_TOTAL_MAX_CHARS`（总量上限，**建议 12 000**）。
  - 触顶时截断并追加「…（已截断，共 N 字符）」。
- **待定**：建议值需拍板（见 §5-Q4）。**注意与 A5 联动** —— 整理步要的是完整输出，不能与下游步骤同一预算（见 A5）。
- **验收**：造 10 万字符输出，断言下游 user message 长度 ≤ `PRIOR_TOTAL_MAX_CHARS` + 固定开销。

#### A5（中）输出整理步只看到 200 字符摘要

- **现状**：`executor.rs:350-357` 整理步的 `prior` = 交付步完整输出 + 各步 `output_summary`（`SUMMARY_MAX_CHARS = 200`，`step.rs:25`）。
- **设计**：各步改用 `record.output`（`OUTPUT_MAX_CHARS = 8000` 上限），并沿用 A4 的**总量**控制（可给整理步单独、更宽的额度，如 24 000）。
- **验收**：非交付步产出 500 字符 → 断言整理步请求含这 500 字符（当前只有前 200）。

---

### B 组 · 执行状态机

#### B1（高）取消不能即时生效；LLM 请求无超时

- **现状**：取消只在 `step.rs:121-125`（轮前）与 `:223-227`（工具循环中）被检查；`step.rs:138` 的 `ai.chat_completion(request).await` **未被 `select!` 包住**。`ai-openai` 全 crate 无请求超时（唯一 `sleep` 在 `client.rs:575`，属重试）。
- **设计（拆两条，分开拍板）**：
  - **B1a（建议做）取消即时**：把 `:138` 的调用包进 `tokio::select! { r = ai.chat_completion(req) => ..., _ = wait_cancel(cancel) => { error = Some("用户取消"); break } }`。纯收益、不改语义。
  - **B1b（待拍板）超时**：`ExecutorConfig` 加超时。⚠️ 该字段**历史上被删过**（当时理由：工作流可能天然很长，固定总时长会误杀）。若加回，建议是**「单次 LLM 请求超时」而非「整步/整次超时」**，且默认 `None`（不启用），由宿主显式开。见 §5-Q3。
- **验收**：B1a —— `SlowAi` 永不返回，`stop()` 后会话能在 <1s 内到终态；B1b —— 超时后该步 `Failed`、错误信息可辨认。

#### B2（中）思考轨迹无上限，且随快照全量 clone 广播

- **现状**：`step.rs:182-188` 把完整 `thought` 发进 `StepThought`；`state.rs:28-33` 原样 push 进 `track`；`RunStore::update` 每次都 `snapshot.clone()` 并广播（`store.rs:83-107`）。
- **设计（本稿只做「限流」，不做「回收」）**：
  - 新增 `THOUGHT_MAX_CHARS`（**建议 2 000**），在 `step.rs:182` **发事件前**截断并加「…（已截断）」—— 事件与快照自然一致，不产生第二个真值来源。
- **不在本稿**：`RunStore.runs` 的淘汰/TTL、快照增量推送（`store.rs:28-31`）—— 涉及 UI 回看语义，列入 §7 待评估。
- **验收**：构造超长 reasoning，断言 `RunSnapshot` 尺寸受控。

#### B3（中）`start()` 被忽略时宿主无感

- **现状**：`service.rs:33-37` 无返回值；`core.rs:121-128` 重复启动只 `tracing::warn!`，服务未运行只 `warn!`。
- **设计（最小实现）**：`core.rs` 拒绝时，向该 `session_id` 的订阅者投一条「启动被拒」的 `RunUpdate`（或新事件 `RunRejected { session_id, reason }`），宿主据此 toast。**不**把拒绝写进 `RunSnapshot`（那会污染「快照 = 一次执行的状态」这一语义）。
- **验收**：同会话连续 `start()` 两次，第二次宿主能收到可区分的提示。

#### B4（低）`PlanRunReport` 无终态字段（建议**不加字段**）

- **现状**：`report.rs:76-91` 只有 `success`；「取消」只体现在 `RunSnapshot.status`（`core.rs:227-229`）。
- **设计**：**不加 status 字段**（避免「报告」与「快照」两个真值来源），改为在 `report.rs` 的文档注释里**明确约定**：「报告不含终态；取消/失败的区分一律读 `RunSnapshot.status`」。
- **验收**：文档约定落地；无代码行为变化。

---

### C 组 · 工具收窄（本稿核心）

#### C1（P0）工具序列进报告

- **现状**：`step.rs:280-285` 已发 `StepToolCall` 事件，`state.rs:34-40` 存进快照 `track`；但 `StepRunRecord`（`report.rs:33-66`）**没有任何工具序列字段** —— 数据采集了，没人汇总。
- **设计**：
  - `report.rs` 加 `ToolCallRecord { tool: String, args: String, ok: bool }`（`Serialize/Deserialize`）与 `StepRunRecord.tool_sequence: Vec<ToolCallRecord>`（`#[serde(default)]`）。
  - **采集点**：`step.rs:280` 发事件**同一处** push 进局部 `Vec`，末尾塞进 `record` —— **一处采集、两条出口**（事件 + 记录）。
  - **与 `imp` 条目 1 的偏差（需知）**：`imp` 写「数据从 `StepSnapshot.track` 派生（单一数据源）」，但 `StepRunRecord` 由 `step.rs` 产出、`track` 在 `state.rs`（executor 之外），跨层派生反而要新增通路。**改为在采集点同处产出**，仍是单一来源、更简单。
  - **`args` 用哪个**：记录用 `describe_arguments`（800 上限，排查用），事件/快照 `$` 行继续用 `describe_tool_args`（120，展示用）—— 两者用途不同，**不合**（与 `flexible-think-box-track` 的既有约定一致）。
- **验收**：`cargo test -p planned-agent --lib flexible::report`；跑一次执行后 report 每步有工具序列。

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
| **Q3** | B1b 是否加「单次 LLM 请求超时」？ | **先只做 B1a（取消即时）**；超时默认不启用，值是后续话题 |
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
