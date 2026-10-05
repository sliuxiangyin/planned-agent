# 步骤固化（flexible）—— 把「会做了」的步骤固化成脚本，逐步把 LLM 移出执行环

> 状态：**设计待审**（未动代码）。
> 本文合并两处既有设计 —— `docs/chat-flexible-模板种子设计.md` 的 `StepFixation`、
> `docs/planned-agent/plan-storage-design.md` §4.3/§4.4 的渐进式固化 —— 并补上它们没有的两块：
> **参数的三类绑定** 与 **参数 schema 提取**（§4.2）。
>
> 上游 / 相邻文档：
> [`flexible-tool-chain-memory.md`](./flexible-tool-chain-memory.md)（工具链记忆 = 本稿的**原料**）、
> [`flexible-executor.md`](./flexible-executor.md)（执行器本体）、
> [`flexible-run-service.md`](./flexible-run-service.md)（执行服务 · 内核零端口）。

---

## 1. 目标与非目标

**唯一目标：减少 token。** 说得更准：**把 LLM 从「每次执行都要参与的环节」里逐步移出去。**

| | 每步的 LLM 次数 | 省的是什么 |
|---|---|---|
| 现状 | 1 次起（工具循环几轮就几次） | — |
| 软提示（`flexible-tool-chain-memory.md` 档 1） | 仍是 1 次起，但**轮数可能降** | 试错轮数 |
| **固化（本稿）** | **0 次**（或降级成一次「无工具定义、单轮」的轻调用） | **整次请求：工具定义 + system + 上下文，以及全部轮数** |

软提示只能省「试错」，**每步的固定开销一分没省**。要数量级地降 token，只有固化这一条路。

**非目标**（先说清，避免走偏）：

1. **不做通用脚本引擎**。仓库依赖里没有 rhai / lua / quickjs（`Cargo.toml` 已核），且**不需要**——理由见 §5。
2. **不追求「全部步骤都固化」**。那等于把灵活模式改造成周密模式。固化是**逐个步骤、可失败、可撤销**的。
3. **不改步骤间的数据传递方式**。仍然是文本（§4.2 末）。

---

## 2. 现状：地基已零散备好

| 已有 | 位置 | 对本稿的意义 |
|---|---|---|
| 每步的工具链（`tool` / `args` / `ok`，按序） | `exec/report.rs` `ToolCallRecord` | **固化的原料** |
| 反参数化（实参值 → `${name}`） | `exec/recipe.rs` `shape_arguments` | **区分「计划参数」与「字面量」的直接证据**（§4.2 靠它） |
| 过滤规则（只整次成功 / 只 `ok=true` / 去 `#RESULT`） | `exec/recipe.rs` `recipes_from_report` | 候选链的质量门槛 |
| 环境事实段（OS / shell / 本机可用命令） | `exec/prompt.rs` | 固化到 shell 时的前提**已知**（不必猜平台） |
| 结构化文件工具 23 个（含 `builtin_search_files` / `builtin_search_files_content` / `builtin_move_file`） | `tool-manager/src/builtin/filesystem/` | 大多数「找文件 / 搜内容 / 搬文件」**一个工具调用就够**，不需要 shell |
| `builtin_execute_command`（`command` + `args`，**默认不套 shell**） | `tool-manager/src/builtin/system_tools.rs:100` | 工具表达不了的聚合 / 循环的**兜底出口**，且仍在工具系统内 |
| 每步上游产出可按 `#En` 取回 | `exec/executor/mod.rs` 的 `store: HashMap<String, StoredOutput>` | `upstream` 绑定的落点 |
| 请求规模诊断日志 | `exec/step/llm.rs` `RequestShape` | 量「工具定义占多少」→ 定固化收益 |
| **依赖里没有脚本引擎** | `Cargo.toml` | §5 的形态约束由此而来 |

---

## 3. 方案总览

执行一个 step 时的决策：

```text
该步有固化脚本？（且 revision 匹配 —— §8）
├─ 有 → 绑定参数（§4.2）
│        ├─ 三类绑定全部成功 → 依次执行脚本
│        │     ├─ 执行成功 + 产出通过校验 → 产出文本，进入下一步   【0 次 LLM】
│        │     └─ 执行失败 / 产出校验不过 → 降级（见下），并降低该脚本置信度
│        └─ `extract` 绑定失败 → 降级（见下）
└─ 无 → 现状：LLM + 工具循环
           └─ 结束后照旧写 `flexible_run_history`（记忆原料）
                  └─ 满足固化条件时 → 生成脚本 + 验证（§6）
```

**降级**＝该步改走 LLM（现状路径），本步产出照常；同时记一次「固化失败」。这是**灵活模式与周密模式的分界**：固化失败永远能退回 LLM，功能不退化。

---

## 4. 参数从哪来 —— 三类绑定（本稿核心）

固化的脚本要有明确参数，参数的**来源**只有三种：

| 绑定 | 含义 | 例子（§4.3） | 固化后还要 LLM 吗 |
|---|---|---|---|
| **`plan:<name>`** | 直接取计划参数（`PlanRunParams`） | `${base_dir}` / `${file_extension}` / `${error_keyword}` / `${logs_subdir}` / `${summary_file}` | **不用**（查表即可） |
| **`upstream:<ref>`** | 上游某步的输出**整体** | `#E2` 的 `content` = `#E1` 的输出 | **不用**（原样搬） |
| **`extract:<描述>`** | 需要从上游文本里**挑出一部分** | 只取「总条数」这一个数字 | **要一次轻调用** |

### 4.1 为什么 `plan` / `upstream` 认得出（反参数化的红利）

`exec/recipe.rs` 的 `shape_arguments` 已经把实参里的计划参数值换成了 `${name}`。于是固化时：

- 形状里出现 `${base_dir}` → 该参数**来自计划**，直接 `plan:base_dir`；
- 形状里是一段**字面量**（不是任何计划参数）→ 它要么是**常量**（如 `limit: 100`，原样固化），要么是**上游派生**的值 → 需要 `upstream` 或 `extract`。

**哪一类，由固化时的 LLM 判断**（它读 `intent` + `expected_output` + 形状）。这正是 §6 那个 agent 的活，也是「反参数化」在固化场景下比在软提示场景下更值钱的原因。

### 4.2 `extract` 的实现 —— 一次「无工具、单轮」的轻调用

这是本稿相对既有设计**新增**的一块，也是「步骤间仍传文本」这个前提能成立的关键。

```text
输入：上游输出文本（可能很长）
提示：从下面的文本里提取以下字段，只输出 JSON：
      - total_count: 整数 —— 匹配到的总条数
      …（由脚本的参数 schema 生成，每个字段一句「要什么」）
输出：{"total_count": 42}   ← 解析后填进脚本参数
```

三个要点：

1. **不带 `tools`**。省掉的正是工具定义那一大块 —— 本稿省 token 的主要来源之一。
2. **单轮、输出极小**。不做工具循环、不迭代。
3. **失败即降级**（解析不出 / 字段缺失 / 类型不符）→ 该步退回 LLM 模式，不是报错。

> ⚠️ **别高估这里的收益**：`extract` 的**输入仍是上游文本**。它省的是「工具定义 + 多轮」，**不是**上游文本。
> 所以判断某一步固化后到底省多少，答案是：`工具定义占比 + 轮数`。这正是 `exec/step/llm.rs`
> 那条诊断日志要回答的 —— 若工具定义占大头，固化是**正对着最大那块**打的。

### 4.3 用示例 4 步套一遍

示例计划（`${...}` 均为计划参数）：

| 步 | intent | 绑定分析 | 固化后 LLM 次数 |
|---|---|---|---|
| `#E1` | 列出 `${base_dir}` 下所有 `${file_extension}` 文件的路径 | 全 `plan` | **0** |
| `#E2` | 逐个读取 `#E1` 中的文件，分别统计含 `${error_keyword}` 的行数 + 总条数 | `#E1` 输出（`upstream`）+ `plan:error_keyword` | **0** |
| `#E3` | 在 `${base_dir}` 下新建 `${logs_subdir}`，把 `#E1` 的文件移进去 | `#E1` 输出（`upstream`）+ `plan:logs_subdir` | **0** |
| `#E4` | 将 `#E2` 的汇总结果写入 `${summary_file}` | `#E2` 输出（`upstream`）+ `plan:summary_file` | **0** |

**4 步全部可以 0 次 LLM** —— 因为这 4 步的参数**没有一处需要「从上游文本里挑片段」**，全是整体搬运。

> 这就是可判定的判据：
> **一步只要不出现 `extract` 绑定，固化后就是 0 次 LLM。**
> 一旦出现，多一次 `extract` 轻调用（仍然远低于现状）。

---

## 5. 固化产物的形态（`StepScript`）

```rust
/// 一个凝固下来的步骤做法。
pub struct StepScript {
    /// 对齐 `steps[].result_reference`。
    pub result_reference: String,
    /// 写这份脚本时的 `plans_flexible_sessions.revision`（§8 的判据）。
    pub revision: i32,
    /// 参数声明。执行时逐个绑定（§4.2 的三类）。
    pub params: Vec<ScriptParam>,
    /// 动作：**有序的工具调用**，允许含 `builtin_execute_command`。
    pub calls: Vec<ScriptCall>,
    /// 产出规则（见下）。
    pub output: OutputRule,
    /// 固化置信度 0~1；失败一次就降，降到阈值以下撤销固化。
    pub confidence: f32,
}

pub struct ScriptParam {
    pub name: String,
    pub kind: ParamKind,       // string / integer / array ...
    pub bind: ParamBind,       // Plan(name) | Upstream("#En") | Extract(描述)
}

pub struct ScriptCall {
    pub tool: String,
    /// 参数模板：值里可含 `${param_name}` / `${#En}`，执行前展开。
    pub args: Value,
}

pub enum OutputRule {
    /// 最后一条调用的输出即本步产出（最简单，覆盖大多数）。
    LastCall,
    /// 指定第 n 条调用的输出。
    Call(usize),
    /// 对若干调用的输出做拼接（需给出拼接方式）。
    Concat(Vec<usize>),
}
```

**为什么不引入脚本引擎**（三条，依次递弱）：

1. **不需要**。§2 已核实：`#E1` 用 `builtin_search_files`、`#E3` 用 `builtin_create_directory` + `builtin_move_file`、`#E4` 用 `builtin_write_file` 都是**单个结构化调用**。真正需要控制流的是 `#E2` 这种聚合 —— 而它可以用一条 `builtin_execute_command`（把 shell 名填 `command`、脚本放 `args`）表达。
2. **落在现有工具系统里**：有 schema 校验、有超时、有日志、UI 可见、受 `allowed_tools` 管。新开一个解释器等于新开一条**平行的执行通道**，安全面与可观测性都要重新做一遍。
3. **生成质量更可控**：让 LLM 生成「工具调用 JSON」比生成「一段可执行代码」**可靠得多**，且错了能被 schema 立刻挡住。

**明确不做的**：不生成 `.ps1` / `.sh` 文件再执行。要走 shell 就**走 `builtin_execute_command`**（显式声明、有审计），不绕。

> 注意 `builtin_execute_command` 的**默认不套 shell** 约定：要管道 / 重定向必须显式
> `command` = 本机 shell 名、`args = ["<脚本>"]`（本机 shell 由运行环境段给出）。固化生成时要照这个写。

---

## 6. 固化从哪来（判定 → 生成 → 验证）

三步，缺一不可：

### 6.1 候选判定（沿用既有设计）

`plan-storage-design.md` §4.4 的判据：**连续 N 次执行的迭代数 ≤ 阈值 → 可固化**（默认 `N = 3`、阈值 `2`，可配置）。落到本仓库，就是读 `flexible_run_history` 的 `tool_chain` + 报告里的 `rounds`：

- 同一 `(session_id, result_reference, revision)` 下，最近 N 次的工具链**结构一致**（工具名序列相同）→ 候选；
- 且这 N 次的 `rounds` 都低 → 候选。

⚠️ 这一步**只筛候选，不决定固化**。工具链一致不等于「这个做法对」——它只说明「做法稳定」。

### 6.2 生成（那个 agent 的活）

由 agent 读「`intent` + `expected_output` + 上游各步的输出样例 + `tool_chain` 形状」，产出 §5 的 `StepScript`：把形状里的 `${name}` 归到 `plan`、把字面量归到常量或 `upstream`/`extract`，再决定 `OutputRule`。

### 6.3 验证 —— **本稿新增，且是这套机制的门**

既有设计里没有这一环：它是「低迭代 → 固化」的**间接**判据。本稿补上**直接**判据：

> 在真实环境跑一次这份脚本，拿真实产出与 `expected_output` 比对，**过了才固化**。

- 比对由 LLM 做（`expected_output` 是自然语言）——**但只在固化时一次**，此后 0 次。
- 不过 → **不固化**（留在候选池，下次再来）。这比「固化错了再回退」便宜得多。
- 通过 → 落库，带上 `confidence`（首次给一个保守值，或按验证结果给）。

**这是整个方案能安全落地的原因**：错误在**固化前**被拦住，而不是在执行时。

---

## 7. 失败与回退（安全网）

| 情形 | 处置 |
|---|---|
| 脚本执行失败（工具报错 / 超时 / shell 不存在） | **降级**：该步改走 LLM；`confidence` 减分 |
| 产出校验不过（`expected_output` 没满足） | 同上 |
| `extract` 绑定失败 | 同上 |
| `confidence` 降到阈值以下 | **撤销固化**（删 `StepScript`），回到候选池 |
| 平台变了（shell / 路径分隔符） | 同上路径，无需特判 |

两条纪律：

1. **降级绝不能变成失败**。降级后该步走的是现状路径，行为与「从没有过固化」一致。功能只增不减。
2. **降级要可见**。UI 上该步应显示「固化未命中 / 已降级」，否则用户看到的是「莫名其妙变慢了」。

---

## 8. 数据落点与生命周期

- **`StepScript` 存哪**：待拍板（§10-Q1）。倾向**新表或 `flexible_run_history` 的扩展列**，不写进 `parameterized_task` —— 沿用 `flexible-tool-chain-memory.md` 已拍板的「模板不被执行事实污染」。
- **与 `revision` 的关系**：脚本带 `revision`，沿用记忆表同一套判据 —— **`steps` 内容换过一代，脚本一律失效**（为什么用 `revision` 而不是别的，见 `flexible-tool-chain-memory.md` §4.2）。
- **原料与产物的上下游**：`flexible_run_history`（原料，每次都写）→ 固化 agent → `StepScript`（产物）。两者不是一回事，别混在一张表里塞。

---

## 9. 与既有设计的关系（合并说明）

| 出处 | 借用的 | 明确**不**照搬的 |
|---|---|---|
| `docs/chat-flexible-模板种子设计.md` `StepFixation`（`:211-222`） | 字段思路：`bypassLlm`（＝本稿的「脚本命中」）、`canFix`、`fixType`（`tool_call` / `condition` / `code` / `response`）、`confidence`、`fallback`；以及 `onError: 'fallback' \| 'llm'`（`:197`） | 那份 template 有 `edges` / `runtimeRecordSchema` / `metricsSchema`，与现有 `FlexiblePlanTemplate { task, inputs, steps, output_schema }` **不一致**；本稿只借字段语义，不引入那套结构 |
| `docs/planned-agent/plan-storage-design.md` §4.3 / §4.4 / §4.5 | 回放逻辑（`fixed` 直接调工具、失败降级）、渐进固化阈值（连续 3 次迭代 ≤ 2）、版本管理思路 | 它讲的是 `plans.todos` + `traces/` 文件，与现在的 flexible 表结构不同 |
| `flexible-tool-chain-memory.md`（本会话已落地的原料层） | `tool_chain` / 反参数化 / 五条过滤 / `revision` 判据 | — |
| **本稿新增** | **参数三类绑定**（§4.2）、**`extract` 轻调用**（§4.2）、**固化前验证**（§6.3）、**`StepScript` 的具体形态**（§5） | — |

---

## 10. 待拍板

| # | 问题 | 倾向 |
|---|---|---|
| Q1 | `StepScript` 存哪？新表 / `flexible_run_history` 扩展列 / 模板 | 新表或扩展列（不写模板） |
| Q2 | 输出规则：默认 `LastCall` 是否够用 | 先用 `LastCall`，不够再加 `Call(n)` |
| Q3 | `extract` 用什么模型 | 复用会话默认 provider；是否给「更便宜的模型」选项待定 |
| Q4 | 验证由谁判：LLM 判 `expected_output` 是否满足 | LLM（固化时一次） |
| Q5 | `calls` 是否限定工具白名单 | 倾向**限定**（只允许内置文件工具 + `builtin_execute_command`），避免固化出不安全调用 |
| Q6 | 固化是**自动**触发还是用户点按钮 | 自动进候选池 + 用户确认固化（与 `flexible_revise` 的「保存前问」一致） |
| Q7 | 是否允许「把上游动作内联」（如 `#E2` 自己再列一遍目录） | 默认**不允许**（会破坏幂等性，`#E3` 移动文件尤其危险） |
| Q8 | UI 如何显示「这一步是固化跑的 / 已降级」 | 至少要有标记（§7 纪律 2） |
| Q9 | 需要多少真实数据才敢开做 | 先用 `exec/step/llm.rs` 的诊断日志看「工具定义占比」，占比小则优先级下降 |

---

## 11. 落地顺序建议（若拍板要做）

| 阶段 | 内容 | 能回答的问题 |
|---|---|---|
| **A** | 手工固化 `#E1` 这一类（**纯 `plan` 参数**、单个结构化工具调用），把「命中 → 跑 → 校验 → 失败降级」闭环打通 | 收益多少？回退手感如何？ |
| **B** | 加 `upstream` 绑定（`#E4` 那种整体搬运） | 步骤间文本传递是否真的够用 |
| **C** | 加 `extract`（轻调用） | 轻调用到底省不省 |
| **D** | 固化 agent（判定 + 生成 + 验证）自动化 | 才轮到「增加一个 agent」 |

**先别建 agent**：它产出什么，取决于 `StepScript` 长什么样。先手工定出形态，否则会写一个产出格式靠猜的贵 agent。
