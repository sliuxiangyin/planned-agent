# 工具链记忆（flexible）—— 从「上次成功的那条路」抄配方

> 状态：**设计待审**（存储层 + 换版判据 + 提炼侧已落地）。**已落地（2026-10-05）**：
> ① §6 的存储层两块 —— `plans_flexible_sessions.revision` 迁移、`flexible_run_history` 建表
> （entity + repo + 三处注册，含 3 个内联测试；`cargo test -p planned-agent-gui --bins` 97 例全绿）；
> ② §4.2 的换版判据 —— `save_snapshot` 比新旧 `steps` 算 `bump_revision`、`produce` 据此决定
> `revision` 是否 `+1`（含 4 个内联测试）；③ §4.1/§5 的**提炼侧**（`flexible/exec/recipe.rs`）——
> 反参数化 `shape_arguments` + 五条过滤规则 `recipes_from_report`（11 个内联单测）。
> **其余未动**：写端口接线（`core.rs` 收尾）、注入（读）、学习开关。
> 本文是 [`flexible-execution-improvements.md`](./flexible-execution-improvements.md) 的 **P2 细化稿**
> （原稿 §5.5「成功配方」+ 条目 6/7/9），并且**改了原稿的一处默认**：载体从「写回模板字段」
> 改为「**新建执行记录表**」（原稿 Q2/Q4/Q7 的默认建议被本轮拍板覆盖）。
>
> 上游契约：[`flexible-executor.md`](./flexible-executor.md)（执行器本体）、
> [`flexible-run-service.md`](./flexible-run-service.md)（执行服务 · 内核零端口）。
> 继承而不重述：原稿的**原则 1「收窄 > 提醒」/ 原则 2「事实 > 归因」/ 原则 3「治本 > 打补丁」**
> （`flexible-execution-improvements.md:122-131`）。
>
> **已拍板（本轮）**：
> 1. 先做 **档 1：软提示**（把上次的成功路径作为「供参考、可偏离」注入）；
> 2. 载体 = **新建执行记录表**（不写回 `plans_flexible_sessions.parameterized_task`）；
> 3. 路径里的工具实参 **反参数化**（把本次参数值还原成 `${name}` 后再存）；
> 4. 本表**只存每一步的 `tool_chain`**，以步骤为单位 —— **不存 `steps` 的副本**（`steps` 的唯一权威来源
>    是 `plans_flexible_sessions.parameterized_task`），需要完整视图时**按 `result_reference` 合并**
>    成「steps json 每项多一个 `tool_chain` 字段」；
> 5. **版本判据 = `steps` 内容指纹**：`produce` 时比对**新旧 `steps`**（只比 `steps`，全字段含顺序），
>    **不一致才把 `plans_flexible_sessions.revision` +1**；一致（例如只改了 `inputs` / `output_schema`）
>    则不动。唯一键仍是 `session_id + result_reference`（`revision` 只作判据列）—— 内容变过即**整代失效**。

---

## 1. 需求

用户原话（本轮）：

> 当执行一次任务后，能不能将每个 step 步骤执行的计划合并为「工具链升级」策略，提升执行效率？
> 意思就是**直接给出上次执行成功的工具调用路径**，然后让 LLM 直接参考，而减少 LLM 猜测和弯路？

拆成三句可落地的话：

| # | 要求 | 落点 |
|---|---|---|
| R1 | 每次执行后，把**每步实际用了哪些工具、按什么顺序、带什么参数**留下来 | §4.1 形状化 + §4.2 表 |
| R2 | 下次执行**同一步**时，把上次那条路喂给模型 | §4.3 注入 |
| R3 | 效果是**减少试错**（不是消除概率性失误） | §8 验收 |

> 「合并」有两读，本稿两读都覆盖：**(a) 单步内**多次调用归一条路径（同一步的工具序列）；
> **(b) 跨步**把整条模板归纳成「每步用什么工具」。载体是同一个表，注入粒度按 `result_reference` 对齐。

## 2. 现状：地基已备一半，缺的是载体与口径

| 事实 | 位置 | 状态 |
|---|---|---|
| 每步的工具**序列**（`tool` / `args` 全量 JSON / `ok`，按发生顺序）已进报告 | `exec/report.rs` `ToolCallRecord` / `StepRunRecord.tool_sequence` | ✅ |
| 采集点与 UI 的 `$` 行**同处同源**（一处采集、两条出口） | `exec/step/mod.rs`（`tool_sequence.push` 与 `StepToolCall` 相邻） | ✅ |
| `args` 是**实参原文**（`describe_arguments`，800 字符封顶） | `exec/step/render.rs` `describe_arguments` | ✅（也是 §4.1 的靶子） |
| 报告整份**留在快照**里（`RunSnapshot.report`） | `run_service/types.rs` | ✅ |
| 环境事实段（OS / shell / 本机可用命令）已落地 | `core::host::RuntimeEnvironment` → `exec/prompt.rs` `step_system_prompt` | ✅ |
| 单步 user 文本只有三段（子目标 / 期望产出 / 前序结果）→ **本稿加第四段** | `exec/prompt.rs` `build_step_task` | 待改 |
| `PlanStep` 只有 4 个字段（无 `allowed_tools` / `preferred_tools`） | `plan/template.rs` | 待议（档 2） |
| 执行报告**不落库**（数据源是快照，不是终态报告） | `agent-gui/src/pages/plan/left_panel/stats.rs` | ❌ 本稿补 |
| `RunRequest` 已在消费「宿主手上、内核不认识的东西」的先例 | `agent-gui/src/services/run_service.rs` `start_run_with_template`（`environment` 就是这么塞的） | ✅ 本稿照抄同构形态 |

**结论**：数据在采、报告在传、环境注入有先例 —— 缺的只是**一个存储载体**和**一条注入通路**。
这与原稿 §2.3「零可观测」的判断已不同：**观测那一步（P0）已经做完了**，本稿是站在它上面。

## 3. 三个已拍板决定，以及它们各自排除了什么

| 决定 | 排除了 | 理由 |
|---|---|---|
| **档 1 软提示** | 硬收窄（`allowed_tools`）、免 LLM 固化（`bypassLlm`） | 后两者要么有过窄卡死风险，要么是大工程（复刻周密模式的固化能力）。原稿原则 1 说「收窄 > 提醒」，但原稿自己也把第一版默认定为只做软提示（`:634`）。**硬收窄留到拿到数据后**（§7-Q6）。 |
| **新建执行记录表** | 写回 `parameterized_task` | ① 模板是「计划骨架」，路径是「执行事实」，语义层级不同，混一起会污染模板；② 写回模板需要内核在跑的时候写库，触发「不得复用 `produce`」（`storage/repository/plans_flexible_sessions_repo.rs:105` 会置 `produced`+`closed_at`，那是**定稿**语义）等一串纪律；③ 新表**只存 `tool_chain`**、以步骤为单位，与 `steps` 各司其职（合成视图按 `result_reference` 合并）。 |
| **反参数化** | 记原值 / 只记形状 | 记原值会把「上次那个文件」注入给模型（下方 §4.1 是命门）；只记形状丢掉了「参数从哪来、怎么拼」的信息。反参数化两者兼顾：**存形状，注入时按本次参数重新展开**。 |
| **版本判据 = `steps` 内容指纹** | 只比骨架（`result_reference`）/ 每次 `produce` 都 +1 | 见 §4.2 末。比骨架**严**（`intent` 一改就换版，挡住「编号没变、含义变了」）；比「每次 `produce` 都 +1」**松**（只改 `inputs` / `output_schema` 不算换版，记忆保住）。代价：判据是**计划级**的，改一步会让其余步骤的记忆一起失效。 |

## 4. 数据形态

### 4.1 形状化（反参数化）—— 本稿的命门

`tool_sequence[].args` 是 `describe_arguments` 渲染的**实参原文**（`render.rs` `arguments.to_string()`），
里面嵌着**本次展开后的具体值**：上次那个绝对路径、上次的索引号、上次的时间戳。

原样存 + 原样注入 → 本次参数换了文件，模型很可能**照抄旧路径**，把「减少弯路」变成「制造新错误」。
这就是原稿 §11 那条风险的真正根因（「上次参数恰好对 → 被当成标准答案」）。

**形状化算法**（纯函数，已落 `flexible/exec/recipe.rs`）：

```text
输入：args_json: &str（实参原文）  +  params: &PlanRunParams
输出：args_shape: String（把本次参数值换写成 ${name}）
规则：
  1. 按 JSON 递归遍历，只处理 String 叶子；
  2. 叶子值「等于」或「包含」某参数的文本值（PlanRunParams 的 value_to_text）→ 用 ${name} 替换该片段；
  3. 参数值文本长度 < MIN_LEN（建议 4）的**不参与替换** —— 数字 `1`、短串会误伤一大片；
  4. 非参数值的字面量（时间戳、非参数覆盖的路径）**原样保留**（见 §9 隐私）；
  5. 缺省：数字 / 布尔类型的参数**第一版不还原**（可选增强，见 §7-Q3）。
```

- 反向替换是**新增**能力（`PlanRunParams` 现在只有正向 `render`，`plan/params.rs`）。
  实现放 **`exec/recipe.rs`**（而非 `plan/recipe.rs`）：它消费 `PlanRunReport`（运行时类型），
  放 `plan/` 会让「静态模板层」反向依赖运行时报告。`PlanRunParams::as_text_map` 已改 `pub(crate)`。
- 形状化在**交给写端口之前**由内核完成（内核手上有 `params`，宿主不必重实现）。

### 4.2 表结构（宿主侧）

**一步一行**，表里**没有 `steps` 的副本** —— `steps` 的权威来源只有
`plans_flexible_sessions.parameterized_task`，本表只回答「这一步（在**这一版**计划里）上次用了哪条工具链」。
表名按拍板用 **`flexible_run_history`**（后续还要往里加别的字段）。

照 `flexible_state` 的形态（entity + migration + repo + `context/storage.rs` 注册）：

| 列 | 类型 | 说明 |
|---|---|---|
| `id` | string PK | UUID |
| `plan_id` | string FK → `plans.id` | 级联删除 |
| `session_id` | string FK → `plans_flexible_sessions.id` | 级联删除（与 `flexible_state` 一致） |
| `revision` | i32 | 写这一行时的 `revision` —— **判据列**，不参与唯一性 |
| `result_reference` | string | 步骤标识（`#E1`），与 `parameterized_task.steps[].result_reference` 对齐 |
| `tool_chain` | TEXT | **反参数化后**的形状（JSON 数组），见下 |
| `created_at` / `updated_at` | string | |

唯一键 `(session_id, result_reference)` —— **每步只留最新一份（upsert 覆盖）**。

> **读写如何配合**：写时把当时会话的 `revision` 一并写进行里；注入时只认「**行内 `revision`
> == 会话当前 `revision`**」的行。`steps` 内容换过一代 → 旧行全部自动失配、一条都不注入；
> 新一次执行会把它们覆盖掉（唯一键不含 `revision`）。所以表**不膨胀**，也**不需要清理策略**。

`tool_chain` 列的内容（一条成功路径）：

```json
[
  { "tool": "read_file", "args_shape": "{\"path\":\"${file_path}\"}" }
]
```

**合成视图（消费侧，不是表结构）**：要展示 / 导出「带工具链的 steps」时，把
`parameterized_task.steps` 与本表（`revision` 匹配的那些行）按 `result_reference` 合并 ——
合成结果即 **steps json 的每一项多出一个 `tool_chain` 字段**。`intent` / `expected_output` /
`dependencies` 一律取自 `parameterized_task`，本表**不重复存**；`rounds` / `tool_calls`
这类观测指标也不进表（需要时从报告 / 快照取）。

**「内容指纹」比什么、怎么算**：

| 候选判据 | 判断 |
|---|---|
| 只比 `result_reference`（骨架） | ❌ 不够：`intent` 改了而编号没变（这恰恰是 `flexible_revise` 的常态，`revise_callback.rs:516-554`）→ 旧路径被注入到**含义已变**的步骤上 |
| 比整个 `parameterized_task` | 过严：改一个 `inputs` 默认值就让全部记忆失效 |
| **比 `steps`（全字段 + 顺序）** | ✅ **选定** |
| `plans_flexible_sessions.version` | ❌ 它是**「会话即版本」**的语义化版本号（`plans_flexible_sessions.rs:17`），只在 `create` 新会话时分配；`produce`（`..._repo.rs:109-129`）**根本不碰它** → 拿它当判据等于不判 |
| `updated_at` 当戳 | 也能判「改过没有」，但 `update_title` 也刷它（`:100`）→ 改标题会误失效 |
| 每步各自的指纹 | 更精细（只失效被改的那一步），本版不做（回退路径见 §9） |

落地要点：

1. `plans_flexible_sessions` 加列 `revision: i32`（default 0），走**新迁移**（`AlterTable add_column`）；
   老数据得 0（无害：首次 `produce` 后变 1）。
2. **比较放宿主 service 层**（`PlansFlexibleService::save_snapshot`），**不放 repo** —— repo 不该认识
   模板结构。做法：旧行的 `parameterized_task` 与新 JSON 各取 `["steps"]`，直接比
   `serde_json::Value`（object 的 key 顺序不敏感、**数组顺序敏感**，正好）。写成小纯函数 + 单测。
3. `produce` 收一个 `bump_revision: bool`，为真才 `am.revision = Set(current + 1)` —— 它本来就已
   `find_by_id` 拿到 model，只多这一句。**必须**同步更新它的 doc 注释。
4. **边界**：旧列为 `None`（首次定稿）或旧 JSON 解析失败 → 一律 `bump = true`（保守：宁可失效）。
5. **影响面已核**：`produce` 只有一个仓库层调用点 `PlansFlexibleService::save_snapshot`
   （`plans_flexible_service.rs:62-64`），且当前**没有任何消费者读**修订号 → 纯增量、不改既有行为。
6. 执行期读模板时**一并读 `revision`**（`PlanTemplateState` 带上它，或另加一个读方法），写记录时
   带上；注入时比对。执行开始时读一次即可 —— 执行途中用户又 `produce`，那条记录仍归属**旧**
   `revision`，不会污染新版。

### 4.3 注入文本（user 段，不是 system）

- **落点**：`exec/prompt.rs` 的 `build_step_task`（它现在写三段：子目标 / 期望产出 / 前序结果）。
  环境段走 system（全局事实、每步相同，`step_system_prompt`）；「上次本步的做法」是 **per-step 事实**
  → 跟 `intent` 一起进 user，与原稿 §5.5 一致。
- **先渲染再注入**：形状里的 `${name}` 用**本次** `params` 正向展开（复用 `PlanRunParams::render`）。
  缺值时用 `render_lenient`（`plan/placeholder.rs`）保留占位符并标记，**不得**因一条旧路径展开失败就判该步失败。
- 文本草案：

```text
## 本步上次的做法（供参考，可偏离）
上次这类子目标是这样完成的（工具 → 关键入参）：
1. read_file  {"path": "C:/a/b.txt"}
仅供参考；本次请依据「本次子目标」与当前实际情况判断，不要照搬上次的具体取值。
```

- 措辞纪律：**必须**带「供参考、可偏离」与「不要照搬取值」两句 —— 否则就是在和 `intent` 抢注意力
  （原稿原则 3 的靶心）。
- **`#RESULT` 整理步不注入**（它不碰工具，只整理数据；与环境段同一处置）。

## 5. 数据流（两条链路）

**落库（写）**：

```text
执行结束 → FlexibleExecutor::run 拿到 report
   │  report.success == true 时：对每个 Done 模板步，用本次 params 把 tool_sequence 形状化
   ▼
RunRequest.history: Option<Arc<dyn RunHistoryStore>>   ← 由宿主注入（None = 开关 OFF）
   │  await save(session_id, revision, [{ result_reference, tool_chain }, …])
   ▼
GUI 实现 → upsert flexible_run_history（UNIQUE(session_id, result_reference)，行内记 revision）
```

（`revision` 由宿主在**执行开始时**从会话行读出、随请求传入 —— 见 §4.2 落地要点 4。）

> **只写「整次都成功」的执行**：表是覆盖语义，一次失败的执行若写入，就会把上次那条
> 好路径冲掉（§7-Q1 / §9）。

**注入（读）**：

```text
左面板点执行
  └─ 宿主读 flexible_run_history WHERE session_id = ? AND revision = <当前> → 本版各步路径
       └─ RunRequest.previous_recipes: Option<Vec<ToolStepRecipe>>（revision 一起带上）
            └─ FlexibleExecutor::run 每步：
                 · 按 result_reference 取本步配方（没有就不加这段）
                 · 用本次 params 正向渲染 → 追加到 build_step_task 的 user 文本
```

**为什么落库用「内核写端口」而不是「宿主订阅驱动」**（沿用原稿 §7 的论证）：
执行是后台常驻的（`spawn_forever` 投 ROOT scope），**页面可能已不在**，而页面订阅在卸载时会注销
（`agent-gui/src/services/run_service.rs` 的 `use_run_subscription`）。因此写端口由宿主在
`start_run_with_template` 组装请求时注入 —— 与 `Arc<dyn AiClient>` / `environment` **同构**
（都是「内核需要的能力由调用方给」，内核仍然**不查库、不认识 `sea_orm`**）。

## 6. 改动清单

| 层 | 文件 | 改动 |
|---|---|---|
| 内核 · 提炼（保存侧） | `flexible/exec/recipe.rs` | ✅ **已落地**（批 1）：`ToolCallShape` / `StepToolRecipe` + `shape_arguments`（反参数化）+ `recipes_from_report`（五条过滤规则）；11 个内联单测。**未做**：注入文本渲染（读侧） |
| 内核 | `flexible/plan/params.rs` | ✅ `as_text_map` 改 `pub(crate)`（反参数化的反向查表） |
| 内核 | `flexible/mod.rs` | ✅ 导出 `StepToolRecipe` / `ToolCallShape` / 两个函数 |
| 内核 | `run_service/types.rs` | `RunRequest` 增 `plan_id` / `revision` / `history: Option<Arc<dyn RunHistoryStore>>`（写侧）+ `previous_recipes`（读侧） |
| 内核 · 写端口 | `run_service/history.rs`（新） | `RunHistoryStore` trait：`save(plan_id, session_id, revision, recipes)` |
| 内核 · 收尾 | `run_service/core.rs` | **写侧接缝**：`Ok(Ok(_report)) => return` 现在把报告丢弃 → 改为先写端口再 `return`（失败只 warn） |
| 内核 | `run_service/mod.rs` | 导出 `RunHistoryStore` |
| 内核 | `exec/prompt.rs` | `build_step_task` 增可选配方参数；新增注入段的拼装 |
| 内核 | `exec/step/mod.rs` | `run_step` 透传本步配方；`StepInput` 增字段 |
| 内核 | `exec/executor/mod.rs` | 读侧：每步按 `result_reference` 取配方 |
| 宿主 · 存储 | `storage/entities/flexible_run_history.rs`、`migrations/m*_create_flexible_run_history.rs`、`repository/flexible_run_history_repo.rs`、`storage/migrations/mod.rs`、`storage/entities/mod.rs`、`storage/repository/mod.rs`、`context/storage.rs` | 新表全套（照 `flexible_state` 的同名结构）；repo 提供 `upsert(session_id, result_reference, revision, tool_chain)` 与 `list_by_session_revision(session_id, revision)`。✅ **已落地** |
| 宿主 · 迁移 | `storage/migrations/m*_add_plans_flexible_sessions_revision.rs` + `storage/migrations/mod.rs` | `plans_flexible_sessions` **加列 `revision`（i32, default 0）**。✅ **已落地** |
| 宿主 · 实体 / 仓库 | `storage/entities/plans_flexible_sessions.rs`、`storage/repository/plans_flexible_sessions_repo.rs` | ✅ **已落地**：实体字段已加、`create` 补默认 0、`produce` 收 `bump_revision` 并在为真时 `revision + 1` |
| 宿主 · 指纹比较 | `services/plans_flexible_service.rs` | ✅ **已落地**：`save_snapshot` 读旧值→纯函数 `steps_unchanged`（比新旧 `steps` 的 `serde_json::Value`，object 顺序不敏感 / 数组顺序敏感）→ 传给 `produce`。3 个判据单测 + 1 个端到端 `revision` 单测 |
| 宿主 · 端口 | `services/run_service.rs` | 实现 `RunHistoryStore`（写库）；`start_run_with_template` 前读本会话**当前 `revision`** 的各步 `tool_chain` 塞进请求 |
| 宿主 · UI | `pages/plan/left_panel/` | 「学习」开关（默认 OFF）+「上次路径」只读展示（可选，第二轮） |

## 7. 待拍板

| # | 问题 | 默认建议 |
|---|---|---|
| Q1 | 落库的**触发条件** | 只记 `report.success == true` 的执行的 Done 步 —— **必须如此**：表是覆盖语义（唯一键不含 `revision`），一次失败执行若写入就把同版的好路径冲掉了。「失败路径」默认**不落**（失败观测走日志）。 |
| Q2 | 注入的**内容粒度** | 第一版给「工具名 + 关键入参形状」；若太吵，退到「只给工具名序列」。 |
| Q3 | 数字 / 布尔参数是否也反参数化 | 默认**不还原**（误伤风险 > 收益），列为可选增强。 |
| Q4 | 单步内是否过滤**探路调用**（`ls` / `cat` 之类） | 默认**不过滤**（档 1 是软提示、可偏离）；「提炼必要步骤」留到有 LLM 的 P2 后半段。 |
| Q5 | 表里**旧 `revision` 的行**怎么处理 | **不需要清理**：唯一键是 `(session_id, result_reference)`，同一 `#E` 只留最新一份；失配的旧行会被下一次执行覆盖。孤儿 `result_reference`（步骤已被删）留下的行无害（永不匹配），也不清理。 |
| Q6 | 何时上**档 2（按步收窄 `allowed_tools`）** | 有了本表的数据、且「开关前后探测试错次数下降」可测之后（原稿 §8 的硬闸）。 |
| Q7 | `tool_chain` 内是否也记**失败调用**（`ok=false`） | 默认**不记**：只留走通的那条路（原则 2：抄成功配方）。 |
| Q8 | 「学习」开关的默认态与是否持久化 | 默认 **OFF**、**不持久化**（离开页面回到 OFF），与原稿 §7 一致。 |

## 8. 验收

第一阶段（零 LLM、不改变任何执行行为的那半）：

- 单测：形状化往返（实参 → 形状 → 用**另一组**参数渲染 → 得到新参数的正确实参，且**不含**旧值）；
- 单测：**合成视图** —— `parameterized_task.steps` + 本表按 `result_reference` 合并后，每项都带上
  `tool_chain`，且 `intent` / `expected_output` 仍取自 `parameterized_task`（不来自本表）；
- 单测：`previous_recipes = None` 时 user 文本与今天**逐字一致**（回归保护，与环境段同一手法）；
- 单测：缺值的旧形状用 `render_lenient` 保留占位符，**不**导致该步失败；
- 集成：跑一次模板 → 表里出现一条记录 → 再跑一次 → 断点看请求里确实带上了上一段的注入文本。

第二阶段（软提示的效果，回答 R3）—— 对比数据取**每次执行的报告 / 快照**（`rounds` / 工具调用次数
不在本表里），不靠本表：

- 同一模板原样执行多次，比较**每步 `rounds` 与工具调用次数**（`StepRunRecord` 已有）：
  注入开关 ON / OFF 各若干次，看「探测性调用次数」是否下降；
- **硬闸**：若测不出差异 → 说明假设不成立，**停手**，不要继续上档 2（沿用原稿 §8 的纪律）。

## 9. 风险与边界

| 风险 | 说明 | 缓解 |
|---|---|---|
| **照抄旧值** | 形状化漏掉某个参数值 → 旧路径被当成标准答案 | MIN_LEN 阈值 + 注入措辞「不要照搬取值」；单测锁「注入文本不含旧值」 |
| **旧路径与新环境冲突** | 装在 `allowed_tools` 之外的工具没了、平台变了 | 形状只是**提示**，永不作为约束；环境段（`step_system_prompt`）仍然优先 |
| **抢注意力** | 第四段指令削弱 `intent` 的权威 | 严格措辞 + 默认 OFF 开关 + 原稿原则 3：真正治本是改 `intent` |
| **隐私外发** | `args_shape` 里非参数值的字面量（用户名路径、时间戳）会随 prompt 发给 provider | 与环境段 `working_dir` 同一纪律；可提供「只记工具名」的严格模式（§7-Q2） |
| **失败执行冲掉好路径** | 表是覆盖语义（唯一键不含 `revision`）→ 一次失败的执行若写入，会把同版成功那条覆盖掉 | 落库条件锁死 `report.success == true`（§7-Q1）；单测锁「失败 run 不写表」 |
| **`produce` 多了一步比较** | 它不仅要写定稿，还要判断 `steps` 指纹、决定是否推进修订号 | 比较是**纯函数 + 单测**；边界保守（旧值缺失 / 解析失败 → `bump = true`）；唯一调用点已核（`save_snapshot`） |
| **指纹误判 → 误 +1** | `steps` 里出现无意义差异（例如 object key 顺序）也会换版，浪费一代记忆 | 比 `serde_json::Value`（object 的 key 顺序**不敏感**）；单测锁「语义相同的两份 JSON 不换版」 |
| **改一步 → 整代记忆失效** | 判据是**计划级**的（比「每步各自的指纹」粗） | 已知取舍（用户已确认）；回退路径 = 把指纹下沉到步骤级（每行存该步指纹），不动表其余结构 |
| **写库拖慢执行** | 落库在 `run` 收尾 | 只 `warn` 失败、**不许**改 `report.success`、**不许**补 `Failed` 事件（原稿 §6 顺序三要点） |

## 10. 与既有设计的关系

| 关系 | 说明 |
|---|---|
| 细化 `flexible-execution-improvements.md` | 本文 = 它的 P2（§5.5 + 条目 6/7/9），但把「写回模板字段」换成「新表」（且只存 `tool_chain`、不复制 `steps`），并把它只记「首个成功工具」扩成「整条路径」 |
| 复用 `run_service` 的零端口边界 | 新增的是**写端口**（与 `Arc<dyn AiClient>` / `environment` 同类的能力端口），不是被否决的「读接缝」（`flexible-run-service.md` §12） |
| 与 `chat-flexible-模板种子设计.md` 划清 | 那份 spec 的 `fixation.bypassLlm` + `argsTemplate`（静态值进模板、动态值变 `{{var}}`）正是**档 3 免 LLM 固化**，与本稿的反参数化同源但目标不同 —— **另立项**，别混进来 |
| 触及 `plans_flexible_sessions_repo.rs` 的 `produce` | 本稿给它加了「按 `steps` 指纹决定 `revision` 是否 +1」的副作用（§4.2，新增 `bump_revision` 参数）。它在「会话即版本 + 定稿」链条上，`flexible-incremental-revise.md` 的保存路径也经它 —— 改动虽小，但要与那条链路的既有测试一起跑（GUI 侧 `--bins` + storage 相关） |
