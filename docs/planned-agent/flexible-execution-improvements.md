# Flexible Execution Improvements —— 让执行少走弯路

> 状态：**设计待审**（未动代码）。
> **本文取代 `flexible-guidance.md`**（该稿方向错误：把三类不同的问题压成一个「失败教训自由文本字段」）。
> 仍然成立、被本文继承的部分：写回端口（trait 接缝）、落库「不得复用 `produce`」、快照可展示。
>
> 上游契约：[`flexible-executor.md`](./flexible-executor.md)（执行器本体）、
> [`flexible-run-service.md`](./flexible-run-service.md)（执行服务 · 内核零端口）、
> [`skill-execution.md`](./skill-execution.md)（**per-step 工具约束的既有先例**）。

---

## 1. 需求（用户原话 + 两轮澄清）

1. `left_panel` 增加开关：开启后，每步执行完做总结并给出指导，**「下次执行就不会出现同样的问题」**；
   给 `parameterized_task` 的 `steps` 增加**指导字段**；总结要能通知 `plans_flexible_sessions`；
   通过 **trait 接口**在 `run_service` 中更新该字段。
2. **澄清一（本文的靶心）**：目标**不是**消除模型概率性失误，而是解决**走弯路** ——
   用户举的三个例子：
   - **工具选择试错**：不确定该用哪个工具，连续试了几个才找到对的；
   - **目标描述不清**：`intent` 没写清 → 模型理解偏 → 整条分支走错；
   - **环境不确定**：不确定当前是 Windows 还是 Linux，反复试探命令形态。

   **这三个只是示例，不是全集** —— 成因的完整清单与处置分类见 §2.1。
3. **澄清二**：指导要记录的是「**该用哪个工具**」「**哪条路不能走**」。

> 因此本文不再谈「失败率」。**走弯路 ≠ 失败**：试错 5 个工具最后成功，在现有系统里就是一次成功的执行。

## 2. 问题定义

### 2.1 成因全景表

**为什么要有这张表**：用户举的三个例子都属于「**事实缺失**」（不知道该用哪个工具 / 目标是什么 / 环境是什么）。
但还有三类成因 —— **结构**（跨步上下文）、**守卫**（循环保护）、**冲突**（prompt 内部矛盾）—— 它们恰好是
「记录指导」最无能为力的地方，也正是**确定性手段最有效**的地方。

> **选型纪律**：每条成因必须能标出处置类别（**守卫** / **计划期 lint** / **只观测**）。
> **标不出类别的，不进方案** —— 这是旧稿「一个自由文本字段打天下」的防复发条款。

#### A. 上下文与信息传递（`flexible` 结构性，最被忽视）

| 成因 | 代码依据 | 可观测信号 | 处置 |
|---|---|---|---|
| **每步是全新消息列表**，与前序只通过结果表交换数据 | `step.rs:1-5` | 同一工具 + 同参数**在不同步骤重复出现**（同一个文件被读两次） | 只观测（隔离性是设计，不是 bug） |
| **`prior` 只传 `dependencies` 显式列出的** | `executor.rs:434` `collect_prior` | 模型调工具去「找」本该由前序提供的数据 | 守卫（计划期 lint：引用了 `#En` 却没写进 `dependencies`） |
| `#En` 引用体系靠 prompt 文本段理解 | `prompt.rs:46-55` | 模型误以为要调工具去「取 `#E1`」 | 守卫（措辞：明写「以下是已完成步骤的真实产出，直接使用，不要重新获取」） |
| 输出传播是全量（≤8000 字符），摘要另有一份 200 字符 | `step.rs:31` / `step.rs:25` / `executor.rs:166` | 输出被截断时下游拿不到关键片段 | 只观测 |

> A 组是 `flexible` 隔离性设计的**固有代价**：解法不是「共享全量上下文」（那会毁掉隔离与可重试），
> 而是**把已确认的事实显式化**。

#### B. 计划期缺陷（执行期只是受害者）

| 成因 | 代码依据 | 处置 |
|---|---|---|
| `intent` 歧义 | `prompt.rs:35` `build_step_task` | 计划期改进（写回 `intent`，§5.6） |
| **`expected_output` 不可判定**（「确保正确」）→ 模型不知道何时停 → **过度自证** | `prompt.rs:35-57` | 计划期 lint + 改写建议 |
| 步骤粒度不当（一步多目标 / 切太碎） | — | 计划期改进（人工 / 候选） |
| 步骤重复（无去重） | — | 计划期 lint |
| **依赖链写错**（漏写 / 顺序错 / 环） | `executor.rs:434` | 守卫（lint：`result_reference` 唯一、依赖存在、无环） |

#### C. 工具与接口

| 成因 | 代码依据 | 处置 |
|---|---|---|
| 工具选择试错 | 整次级白名单 `executor.rs:31-34` | 守卫（按步收窄，§5.3） |
| 工具描述不清（含平台） | `system_tools.rs:19/23/29` | 守卫（按平台拼描述，§5.4） |
| **工具返回格式不可预测**（大 JSON / 分页 / 大文本） | — | 守卫（描述里写明返回结构） |
| **参数形态坑**（含空格路径、引号转义、编码） | `step.rs:242-269`（工具层报错只回灌） | 守卫（工具层校验 + 结构化错误：报「path 是相对路径」，而不是 OS 原文） |
| **能力缺失**：没有对应工具 → 用 shell 绕 | `system_tools.rs:37` `builtin_command_exists` | 环境段直接告知可用命令（§5.2） |

#### D. 模型行为（根治不了，但**能加确定性守卫**）

| 成因 | 代码依据 | 处置 |
|---|---|---|
| **重复调用同一工具同参数** | `flexible` **无重复检测**（grep 全 `flexible/` 无匹配）；planner **有**：`default_react_agent.rs:398-500`（`max_repeats = 3` 打断循环） | **守卫（照抄 planner）** |
| **空转直到撞上限** | `max_rounds_per_step = 50`（`executor.rs:40`），撞上限才判 Failed（`step.rs:204`） | 守卫 + 上限调小 |
| **过度自证**（做完反复读回验证） | — | 只观测 |
| **假成功**（有输出但不符合 `expected_output`） | `step.rs:294` 只看 `output.is_some()` | 只观测（产出可观测的自检信号） |

#### E. 信息冲突 / 噪声（最反直觉的一组）

| 成因 | 代码依据 | 处置 |
|---|---|---|
| prompt 内部规则互相矛盾 | 前车之鉴：step5「prompt 规则与它收到的数据三处硬冲突」 | 守卫（每次改 prompt，注入数据同步改） |
| 前序产出与 `intent` 描述冲突 | — | 只观测 |
| **旧的指导与新的 `intent` 冲突** | ⚠️ **旧稿 guidance 注入会新增的冲突源** | 纪律：写回的东西越多，冲突面越大 → 优先改 `intent`，不加并行字段（原则 3） |

#### F. 系统与元问题

| 成因 | 处置 |
|---|---|
| 网络 / 超时噪声；工具本身慢或挂 | 不可治（重试与超时策略） |
| **无跨执行记忆**（用户核心诉求） | 本文 P2 |
| **无反馈闭环**（执行完无人知晓） | §5.1 + §8 |
| **无回归基线**（改了一次不知道变好没有） | §8 |

### 2.2 处置分类：所有成因只有三种归宿

| 类别 | 覆盖成因 | 手段 | 特点 |
|---|---|---|---|
| **守卫**（确定性） | A2 A3 B5 C1–C5 D1 D2 E1 | 代码里堵死 | 零 LLM、当次生效、可测 |
| **计划期改进** | B1–B4、A2 的 lint | 计划期 lint + `intent` / `expected_output` 改写建议 | 治本，但需人工确认 |
| **只观测** | A1 A4 D3 D4 E2 | 落进度量，让人看见 | 不能根治，但能定位 |

> **选型顺序**：能守卫就别记指导；能改计划期就别在执行期打补丁；只能观测的，别硬塞给对方。

### 2.3 现状：**零可观测**（这是第一件要解决的事）

| 事实 | 位置 |
|---|---|
| 完成判定只看「有输出」 —— 试错几次、走了多少弯路**完全不可见** | `crates/planned-agent/src/flexible/step.rs:294` |
| 每步工具调用次数、轮数**有**，但**没有**调用序列 | `report.rs:33` `StepRunRecord { rounds, tool_calls, .. }` |
| 工具调用的**序列**只活在 think box 轨迹里（工具名 / 关键入参 / 是否成功） | `event.rs:27` `StepToolCall { tool, args, ok }` → `StepSnapshot.track` |
| 工具调用**不进报告、不落库** | `report.rs:33` 无该字段；storage 里没有执行记录表 |
| 执行记录落库被明确**延后**（「待表结构定案」） | `docs/planned-agent/flexible-executor.md:484`（阶段 6） |
| 工具描述里**没有任何平台信息**（模型只能猜） | `crates/tool-manager/src/builtin/system_tools.rs:19`：`description: "执行系统命令并返回输出（内置工具）"` |
| 工具白名单是**整次执行级**，不是按步 | `executor.rs:31-34` `ExecutorConfig.allowed_tools` |

**结论**：没有度量就没有改进。「不再走弯路」在没有工具调用序列落库之前，是一句无法验证的话。

## 3. 设计原则

| # | 原则 | 含义 |
|---|---|---|
| 1 | **收窄 > 提醒** | 与其事后注入「该用 X 工具」，不如**根本不给错的选择**（按步收窄可见工具）。确定性手段优先于提示。 |
| 2 | **事实 > 归因** | 从**成功路径**抄配方（「上次是 `read_file` 一次做成的」）远比从**失败**猜教训（「因为路径相对所以失败」）可靠 —— 后者样本量恒等于 1。 |
| 3 | **治本 > 打补丁** | `intent` 有歧义就**改 `intent`**，不要在它旁边加一段并行的「指导」去抢注意力（`prompt.rs:35-57` 只喂三段：子目标 / 期望产出 / 前序结果）。 |

> 原则 1 的极端形式值得记住：**「不要走错分支」最有效的手段不是提醒，而是不给那个分支。**

## 4. 手段分层总表

| 优先级 | 手段 | 确定性 | 生效时机 | 需要 LLM | 边际成本 |
|---|---|---|---|---|---|
| **P0** | 工具调用序列进报告 / UI（度量） | — | 观测 | 否 | 0 |
| **P0** | 环境事实注入（OS / shell / 分隔符） | 100% | **当次** | 否 | 固定段 |
| **P1** | 按步收窄工具（`PlanStep.allowed_tools`） | 100% | 当次 | 否 | 0（工具表变小） |
| **P1** | 工具 `description` 改进（含平台语法） | 100% | 当次 | 否 | 0（描述本就要传） |
| **P1** | 重复调用守卫（照抄 planner `max_repeats`，§2.1-D1） | 100% | 当次 | 否 | 0 |
| **P1** | 启动前 lint（依赖 / 占位符 / 引用，§2.1-A2/B5） | 100% | 启动前 | 否 | 0 |
| **P1** | 工具层参数校验 + 结构化错误（§2.1-C4） | 100% | 当次 | 否 | 0 |
| **P2** | 成功配方（`preferred_tools`，事实性） | 高 | 下次 | 是（提取） | 每步 1 段 |
| **P2** | `intent` 改进建议（治本） | 中 | 下次 | 是 | 0 |
| **P2** | 总结 AI = **候选发现器 + 审核入口** | 低 | — | 是 | 审核成本 |

**先把 P0 做完再谈 P2** —— P0 不需要 LLM，当次生效，且是 P2 的判断依据。

## 5. 逐条设计

### 5.1 P0 · 观测：工具调用序列

**已有的东西**：`StepToolCall { index, tool, args, ok }`（`event.rs:27`）已经携带全部所需信息（`args` 是 `describe_tool_args` 渲染好的一行，`step.rs:402`），且已攒进 `StepSnapshot.track`。

**要做的**：

```rust
// report.rs —— `StepRunRecord` 增一个字段（`#[serde(default)]` 兼容旧报告）
pub struct StepRunRecord {
    // …
    /// 本步的工具调用序列（按发生顺序）：名字 + 关键入参 + 是否成功。
    ///
    /// 这是「走弯路」的**唯一原始证据**：连续 ok=false、同一工具反复出现、
    /// 或「试了 3 个不同工具才成功」，都只能从这里看出来。
    #[serde(default)]
    pub tool_sequence: Vec<ToolCallRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub tool: String,
    pub args: String,
    pub ok: bool,
}
```

数据来源二选一（实现时定）：`step.rs` 的工具循环里顺手 `push`，或执行器收尾时从该步的 `track` 里 filter `StepTrackLine::Tool`。**推荐后者**（单一数据源，不新增第二份采集）。

**UI**：`PipelineView` 的 think box 已经显示这些行（`pipeline.rs` 的 `StepTrackLine` 渲染）—— 本次只补「**本步尝试了 N 个工具 / M 次失败**」的计数标签，让弯路**一眼可见**。

**落库**：先**不建表**（阶段 6 明确「待表结构定案」）。跑一段时间、拿到真实数据后再定表结构，见 §8。


### 5.2 P0 · 环境事实注入（直击「环境不确定」）

**这是用户例子（Win/Linux）的正解**：这类事实**可探测、100% 确定**，不该猜、也不该靠「上次执行学到」。

#### 5.2.1 数据形态

```rust
// flexible/environment.rs（新）
/// 本次执行的运行环境事实（全部可确定性探测）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeEnvironment {
    pub os: String,                        // std::env::consts::OS
    pub arch: String,                      // std::env::consts::ARCH
    pub path_separator: char,              // std::path::MAIN_SEPARATOR
    pub line_ending: String,               // CRLF / LF（由 cfg!(windows) 定）
    pub shell: Option<String>,             // COMSPEC / SHELL 环境变量
    pub console_encoding: Option<String>,  // 宿主补（Windows 常见 UTF-8 / GBK）
    pub working_dir: Option<String>,       // current_dir()；**默认不注入**，见 §5.2.5
    pub notes: Option<String>,             // 宿主 / 用户自由补充
}

impl RuntimeEnvironment {
    /// 零依赖探测（只用 `std`）：os / arch / 分隔符 / 行尾 / shell。
    pub fn detect() -> Self;
    /// 渲染成注入 prompt 的一段（不含前后空行，由调用方拼接）。
    pub fn render_block(&self) -> String;
}
```

**字段来源的三种处置**：

| 字段 | 来源 | 说明 |
|---|---|---|
| `os` / `arch` | **自动** | `std::env::consts`，确定性、零成本 |
| `path_separator` / `line_ending` | **自动** | `std::path::MAIN_SEPARATOR` / `cfg!(windows)` |
| `shell` | **自动** | `COMSPEC` / `SHELL` 环境变量；探测不到就 `None`（**不强猜**） |
| `console_encoding` | **宿主补** | Windows 下 cmd 输出可能是 GBK；内核无法可靠探测 |
| `working_dir` | **自动** | ⚠️ 含用户名路径 → **默认不注入**（§5.2.5） |
| `notes` | **宿主 / 用户** | 如「本机没装 python3，只有 python」；**会发给 provider** |
| ~~可用命令列表~~ | **不探测** | 要跑 `where` / `which`，代价大 → 改用 `notes` 人工补 |
| ~~时间 / 时区~~ | **不注入** | 每次都变 → 打穿 provider 的 prompt 前缀缓存（§5.2.4） |

#### 5.2.2 探测时机

**执行开始时探测一次**（在 `FlexibleExecutor` 构造时定死），**不落库、不缓存**：

- 每次执行重新探测 → 换机器 / 换 shell 立刻反映（§11）；
- 同一次执行内每步看到**同一份**事实 → prompt 稳定、可复现。

#### 5.2.3 注入落点与确切文本

落点：**system prompt 尾部**（环境是全局事实、每步相同）。

```rust
// prompt.rs
/// `None` → 原样返回 `STEP_SYSTEM_PROMPT`（**逐字不变**，见 §5.2.6）；
/// `Some(env)` → 常量 + 空行 + 环境段。
pub(crate) fn step_system_prompt(env: Option<&RuntimeEnvironment>) -> Cow<'static, str>;
```

环境段的格式（`render_block()` 输出草案，**待审**）：

```text
## 运行环境（事实，直接采用，不要试探确认）
- 操作系统：windows (x86_64)
- 路径分隔符：\ ，行尾：CRLF
- 默认 shell：powershell
- 执行命令时直接使用本平台语法（Windows：dir / type / where / findstr；不要用 ls / cat / which / grep），路径用绝对路径。
- 附加：本机没装 python3，只有 python
```

- `附加：` 一行**仅在 `notes` 非空时**出现；
- `console_encoding` 非空时插一行 `- 控制台编码：GBK（工具输出可能非 UTF-8）`；
- 字段缺失时**该行不出现**（不写「未知」占位）。

#### 5.2.4 为什么放 system、为什么砍掉时间

| 决定 | 理由 |
|---|---|
| 放 **system** 而非 user | 事实性约束权重更高；能明确写「不要试探确认」（正是砍掉「先跑 `uname` 再跑 `ver`」那句话） |
| **不含**时间 / 时区 | 每次执行都不同 → 打穿 provider 的 **prompt 前缀缓存**，白花钱；它也不是「环境事实」 |
| 每步都带（不只第一步） | 每步是**独立**消息列表（`step.rs:1-5`），后一步看不到第一步的 system |
| `#RESULT` 整理步**不注入** | 整理步不碰环境（无工具、只整理数据），注入纯浪费 |

#### 5.2.5 隐私（**必须定**，见 Q12 / Q14）

`working_dir` / `notes` / `shell` 里的路径**会随 prompt 发给 LLM provider**（如 `C:\Users\<用户名>\...`）：

| 字段 | 默认 | 说明 |
|---|---|---|
| `working_dir` | **不注入** | 需要时宿主显式打开（或只给目录名） |
| `notes` | 空 | UI 需提示「会发送给模型服务商」，**不要写凭据** |
| `shell` / `console_encoding` | 注入 | 通常不含隐私（`powershell` / `UTF-8`） |

> 安全侧硬要求：环境段是**外发内容**，与「本地日志」不是一个信任级别。

#### 5.2.6 兼容性与改动面

**兼容性关键**：`environment: Option<RuntimeEnvironment>`，`None` ⇒ **system prompt 逐字与今天一致**（`Cow::Borrowed`，零分配）→ 既有断言 system prompt 的测试**不用改**。宿主（GUI）默认传 `Some(detect())`，用户实际总能拿到注入。

| 文件 | 改动 |
|---|---|
| `flexible/environment.rs` | **新增**：结构 + `detect()` + `render_block()` + 单测 |
| `flexible/prompt.rs` | 新增 `step_system_prompt(env)`；`STEP_SYSTEM_PROMPT` **保持不动** |
| `flexible/step.rs` | `run_step` 增 `system_prompt: &str` 参数（由 executor 拼好传入）；`run_output_resolve` 不动 |
| `flexible/executor.rs` | `FlexibleExecutor::new` 增 `environment: Option<RuntimeEnvironment>`；每步取 `step_system_prompt(self.environment.as_ref())` |
| `run_service/types.rs` | `RunRequest` 增 `environment: Option<RuntimeEnvironment>` |
| `run_service/core.rs` | `start` 把 `request.environment` 透传给 `FlexibleExecutor::new` |
| `flexible/mod.rs` | 导出 `RuntimeEnvironment` |
| `services/run_service.rs`（GUI） | `start_run_with_template(..., environment)`；调用点默认 `Some(RuntimeEnvironment::detect())` |
| `left_panel/left_panel.rs` | `on_run` 组装 env（**不受开关影响**，§7） |

**成本**：约 5 行 ≈ 150 字符 ≈ 60–100 token / 每次 LLM 请求（N 步 × 轮数）；宿主不传即关闭。

#### 5.2.7 测试计划

| 用例 | 断言 |
|---|---|
| `environment::detect_is_consistent` | `os == std::env::consts::OS`；`path_separator == MAIN_SEPARATOR` |
| `environment::render_block` | 固定 env（含 / 不含 notes）→ 金标文本 |
| `prompt::no_env_is_verbatim` | `step_system_prompt(None) == STEP_SYSTEM_PROMPT`（**逐字**） |
| `prompt::env_appended` | `Some(env)` 以常量为前缀、含「不要试探确认」 |
| `step::system_prompt_reaches_request` | `FakeAiClient` 抓 `requests()[0].messages[0]`，断言含环境段 |
| `executor::env_none_no_block` | `RunRequest.environment = None` 时请求里**不含**环境段（回归保护） |

> **⚠️ 写代码时的坑**（本仓库踩过）：环境段含反斜杠 —— Rust 源码里写单引号包的双反斜杠字符字面量，提示词文本用 **raw string**（`r#"..."#`）；否则反斜杠会被当转义符。

### 5.3 P1 · 按步收窄工具（直击「工具选择试错」）

**先例已经有了**：`CoarseGrainedStep.recommended_tool_categories`（`crates/core/src/planner/coarse/coarse_types.rs:96`）在 planner/react 路径里**已按步收窄**（`crates/planned-agent/src/planner/react/tool_executor.rs:76`）。`flexible` 的执行器**只有整次级白名单**（`executor.rs:31-34`）。

```rust
// template.rs —— `PlanStep` 增字段
pub struct PlanStep {
    // …
    /// 本步可见的工具范围（**token 语义与 `ExecutorConfig.allowed_tools` 完全一致**）：
    /// `None` = 不限（用整次要执行的全局白名单）；`Some([])` = 不给任何工具（纯推理步）。
    ///
    /// token 三态（`chat/tools/mod.rs:44-51`，**不发明第二套规则**）：
    /// `"all"`（除 Utility/SubAgent 外的全部） / 分类名（`File` / `System` / `Browser` …，
    /// 见 `core/src/tool_registry/types.rs:71`） / 精确工具名（`builtin_execute_command`）。
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
}
```

- **执行器**：`tool_definitions()`（`executor.rs:269`）→ 改成 `tool_definitions_for(step)`；`None` 时回退到 `cfg.allowed_tools`（向后兼容，**旧模板行为完全不变**）。
- **收益**：模型看不到不合适的工具 → **试错不可能发生**。这是全表里最确定的一条。
- **风险（必须说清）**：过窄会导致**必然失败**（比如把 `builtin_execute_command` 排除了，可本步就是要跑命令）。因此：
  - **不由 LLM 自动产出**（计划期自动猜工具集很容易错）；
  - 建议**由用户在左面板编辑 + 人工确认**（`PlanStep` 的编辑 UI 已在 PARAMS/PIPELINE 有基础）；
  - 或者第一版只做「**软提示**」不做硬约束（见 §5.5）。

### 5.4 P1 · 工具 `description` 改进（全局、零边际成本）

现状（`crates/tool-manager/src/builtin/system_tools.rs:19`、`:23`、`:29`）：

```rust
description: "执行系统命令并返回输出（内置工具）",
// command 参数："要执行的命令"
```

**没有任何平台信息** —— 模型不知道 `dir` 还是 `ls`，于是试探。改进：

- 关键点：`SystemToolsProvider::tools()` 是**普通函数**，可以用 `cfg!(windows)` / `RuntimeEnvironment` 在**运行期**拼描述字符串（不是编译期常量），所以描述**可以按当前平台生成**；
- 描述里写明：本平台语法示例、路径要求（绝对路径）、「不要用其它平台的命令」；
- 参数描述里写明：「Windows 下用 cmd/PowerShell 语法」。

**为什么这条性价比最高**：工具描述本来就要随每次请求传给模型（`executor.rs:472` `to_tool_definition`），**零边际 token**，且对**所有计划、所有会话**生效 —— 不需要「学」。

### 5.5 P2 · 成功配方（事实性记忆）

**与旧稿的区别**：不写「失败教训」，只抄「**成功路径**」。

- **来源**：该步的工具调用序列里**首次 `ok = true`** 的那次调用（§5.1 已提供数据）。
- **形态**：

```rust
// template.rs
pub struct PlanStep {
    // …
    /// 上一次执行本步时**实际奏效**的工具（按成功顺序）。
    ///
    /// **事实记录，不是归因结论** —— 只说明「上次是这样成的」，不说「必须这样」。
    /// 因此要作为**首选提示**注入，不作为硬约束（硬约束用 `allowed_tools`）。
    #[serde(default)]
    pub preferred_tools: Option<Vec<String>>,
}
```

- **注入**：user task 的一段（与 §5.2 的环境段分工不同：环境是全局事实，这条是**本步**的既往做法）：

```text
## 本步上次的做法（供参考，可偏离）
上次这类子目标是用 read_file 一次完成的。
```

- **为什么比「教训」可靠**：它是**观察到的成功**，而不是**推断的原因**。不会出现旧稿说的负收益（「上次因目录不存在失败 → 写成『路径必须存在』→ 下次反而不去创建目录」）。
- **仍是概率性收益**：上次的成功可能是巧合（参数刚好对）。所以是「提示」不是「约束」，且**必须可删**。

### 5.6 P2 · `intent` 改进建议（治本）

**理由**：`build_step_task`（`prompt.rs:35-57`）只给模型三段 —— `## 本次子目标` / `## 期望产出` / `## 前序步骤结果`。加一个平行的「指导」字段，等于**第四段指令和 `intent` 抢注意力**，治标；而歧义是**计划期**就产生的。

- **触发信号**（来自 §5.1 的数据）：该步 `rounds` 明显高于同模板其它步 / 出现「探测性调用」（连续多次不同工具的同类调用）/ 多次失败后才成功；
- **产出**：一段更具体的 `intent` 文本（**不是**贴在外面的评语）；
- **落点**：写回 `intent` **需要谨慎**（用户精心写的 `intent` 可能被覆盖）→ 见待拍板 Q5：默认**只产出建议、不自动覆盖**。

### 5.7 P2 · 总结 AI 的角色重置

| | 旧稿（`flexible-guidance.md`） | 本文 |
|---|---|---|
| 输入 | 只该步轨迹 | 轨迹 + 工具序列 + 既有 `preferred_tools` / `intent` |
| 输出 | **自由文本**，直接注入模板 | **结构化建议**（见下） |
| 去向 | 自动写进 `guidance` 字段 | **候选 → 审核**（可配置为自动） |
| 生效 | 下次执行，作为 prompt 第四段 | 落到 `intent` / `preferred_tools` / `allowed_tools` **这些已有的语义位**上 |

```rust
/// 一次执行后，某步的**候选改进**（结构化，可审、可校验、可拒）。
#[derive(Debug, Clone, PartialEq)]
pub struct StepImprovement {
    pub session_id: String,
    pub run_id: u64,
    /// 定位键：用 `result_reference`，不用 `index`（报告里可能有 `#RESULT` 这类非模板步）。
    pub result_reference: String,
    /// 更清楚的子目标描述（`None` = 建议不改）。
    pub intent: Option<String>,
    /// 建议的可见工具范围（`None` = 建议不改）。
    pub allowed_tools: Option<Vec<String>>,
    /// 上次实际奏效的工具（事实，由 §5.1 直接抄）。
    pub preferred_tools: Option<Vec<String>>,
    /// 依据（**给人看的**，不进 prompt）：如「S3 试了 3 个工具，rounds=7」。
    pub evidence: String,
}
```

**关键纪律**：`evidence` 字段是硬要求 —— 每条建议都必须能说出「凭什么是这条」。这是防止 LLM 编造「教训」的最后一道闸。

## 6. 数据流

```text
左面板「执行」（开关 ON）
  └─ RunRequest { …, environment: Some(env), improvements: Some(port) }
       └─ FlexibleExecutor::run
            ├─ 每步：
            │    · system = step_system_prompt(env)            ← §5.2 当次生效
            │    · tools  = tool_definitions_for(step)         ← §5.3 当次生效
            │    · user   = task + 「本步上次的做法」           ← §5.5 下次生效
            │    · LLM ⇄ 工具（序列采集）                       ← §5.1
            │    · StepFinished(record{ tool_sequence })
            │    · 开关 ON → 提取 `preferred_tools`（事实，无需 LLM）
            │       +（可选）总结 AI → `StepImprovement`（候选）
            │         → emit StepImprovement{index}   ─────► 快照：UI 立即可见
            │         → port.save(improvement)       ─────► 写回模板字段 / 候选区
            │                                                → TemplateNotifier.notify()
            └─ 收尾：RunFinished { report }
```

**顺序三要点（与旧稿一致，仍然成立）**：
1. **先 emit、后写库**：UI 立刻可见，写库不拖住「本步已完成」；
2. **写库失败只 `warn`**：改进是增强，**不许**改 `report.success`、**不许**补 `Failed` 事件；
3. **每步 await 完再进下一步**（串行；内核不 `tokio::spawn`，理由见 `run_service/core.rs:8-9`）。

## 7. 写回通道（trait 接缝）与开关

**决策已定（用户第二轮）**：**不由 GUI 订阅驱动写库** —— 执行是后台常驻的（`spawn_forever` 投 ROOT scope），页面可能已不在。因此内核新增**写端口**，与 `Arc<dyn AiClient>` 同类（都是「内核需要的能力由调用方注入」，见 `flexible-run-service.md` §12 的边界说明 —— 内核仍然**不查库、不认识 `sea_orm`**）。

```rust
// flexible/improvement.rs
#[async_trait::async_trait]
pub trait ImprovementStore: Send + Sync {
    /// 写回某一步的候选改进（宿主决定落到模板字段还是候选区）。
    async fn save_step_improvement(&self, improvement: StepImprovement) -> anyhow::Result<()>;
}

// run_service/types.rs
pub struct RunRequest {
    pub session_id: SessionId,
    pub template: FlexiblePlanTemplate,
    pub params: PlanRunParams,
    pub client: Arc<dyn AiClient>,
    pub config: ExecutorConfig,
    /// 运行环境事实（`None` = 内核自行 `detect()`）。
    pub environment: Option<RuntimeEnvironment>,
    /// 改进写回端口（`None` = 开关 OFF，完全不产出改进）。
    pub improvements: Option<Arc<dyn ImprovementStore>>,
}
```

**开关落点**：PIPELINE 块头（`pipeline.rs:36`），与执行 / 停止同一排：`[🧭 改进] [▶ 执行] [⏹ 停止]`。默认 **OFF**；第一版**不持久化**（离开页面回到 OFF）。

**注意区分**：开关只管「**要不要产出并写回改进**」。§5.2 的环境注入、§5.3 的 `allowed_tools`、§5.4 的工具描述、§5.5 的既有 `preferred_tools` **一律不受开关影响** —— 它们是「用」，开关管的是「学」。否则「上次学到的」会因为这次没开开关而失效，正好违背需求。

同理，**§2.1 里处置为「守卫」的成因一律不进开关** —— 它们是系统本身的改进（重复调用守卫、启动前 lint、工具层校验），对所有计划、所有会话、永远生效。

**落库纪律（继承自旧稿，仍然成立）**：

| 纪律 | 理由 |
|---|---|
| **不得复用 `PlansFlexibleSessionsRepo::produce`** | 它会把 `status` 置 `produced` + 写 `closed_at`（`plans_flexible_sessions_repo.rs:109`）—— 那是**定稿**语义；执行后写改进不是定稿 |
| 只更新 `parameterized_task` + `updated_at` | 新增 `update_step_improvement(...)`；读-改-写走**纯函数**（同 `save_callback.rs:73` `build_payload` 的风格，便于单测） |
| 宿主侧写端口持一把 `Mutex` | 内核现在顺序调用；锁住「读-改-写」整段防将来的丢失更新 |

## 8. 度量与验收（先说清：现在做不到）

**要回答「是否不再走弯路」，最少需要**：

| 指标 | 来源 | 现状 |
|---|---|---|
| 每步 `rounds` | `StepRunRecord.rounds` | **已有** |
| 每步工具调用序列 / 失败次数 | §5.1 | 本次新增 |
| 「探测性调用」计数（连续不同工具的同类调用） | §5.1 派生 | 本次新增 |
| 是否注入了改进（`preferred_tools` / 环境段） | `RunRequest` | 本次新增 |
| 同模板多次执行的对比 | **执行记录落库** | **无**（阶段 6 待定案） |

**务实的第一步**：**先不建表**。§5.1 先把序列放进报告 + UI，人工观察几次真实执行，拿到数据再定表结构（这符合 `flexible-executor.md:484` 的「待表结构定案」）。

**验收（第一阶段）**：
- 关 / 开「环境注入」，同一模板原样执行：**探测性调用次数下降**（这是 §5.2 的唯一可测收益）；
- 关 / 开「按步收窄」，同一模板执行：**工具试错次数下降**；
- 若两条都测不出差异 → **说明这两个假设不成立，应当停手**，不要继续 P2。

## 9. 待做清单（编号 = 实施顺序）

> **依赖链**：`0 → 1 →（2 / 3 / 4 / 5 可并行）→ 6 → 7 → 8 → 9`。
> **条目 1–5 零 LLM 调用、当次生效、可独立验收** —— 建议先只做这五条。

### 0. 拍板（阻塞全部）

§10 的 Q1–Q11 定案。尤其 **Q2**（按步工具：硬约束还是软提示）、**Q4/Q5**（改进自动写回还是候选审核）、
**Q7**（是否建执行记录表）—— 不拍板，条目 4 / 6 / 7 / 8 的形态定不下来。

**验收**：用户确认。

### 1. 观测：工具调用序列（P0，无 LLM）

- **改**：`report.rs` 加 `ToolCallRecord { tool, args, ok }` + `StepRunRecord.tool_sequence: Vec<ToolCallRecord>`（`#[serde(default)]`）；
  数据从 `StepSnapshot.track` 的 `StepTrackLine::Tool` 派生（**单一数据源**，不新增第二份采集）
- **改**：`pipeline.rs` 每步加「尝试 N 个工具 / M 次失败」标签
- **验收**：`cargo test -p planned-agent --lib flexible::report`；手工看 PIPELINE
- **对应**：§2.1 **D1 / D3 / D4** —— 所有守卫与改进的观测前提

### 2. 环境事实注入（P0，无 LLM，当次生效）

- **新**：`crates/planned-agent/src/flexible/environment.rs` → `RuntimeEnvironment { os, arch, path_separator, line_ending, working_dir, shell, notes }` + `detect()`（纯 `std`）
- **改**：`prompt.rs` 加 `step_system_prompt(env)`；`step.rs` 换成它；`run_service/types.rs` 的 `RunRequest` 加 `environment`；GUI 侧补 `shell` / `notes`
- **验收**：`flexible::environment` / `flexible::prompt` 单测，**env 为 `None` 时输出与今天逐字一致**
- **对应**：§2.1 **C5** + 用户例子（Win / Linux）

### 3. 工具描述按平台生成（P1，无 LLM，全局生效）

- **改**：`crates/tool-manager/src/builtin/system_tools.rs` —— `builtin_execute_command` 等按运行平台拼 `description`（语法示例 + 绝对路径 + 「不要用其它平台的命令」）
- **验收**：`cargo test -p planned-agent-tool-manager`（描述含平台关键词）
- **对应**：§2.1 **C1 / C2** —— 试错的根源（描述里连平台都没有）

### 4. 按步收窄工具（P1，无 LLM，当次生效）

- **改**：`template.rs` 加 `PlanStep.allowed_tools: Option<Vec<String>>`（token 语义同 `ExecutorConfig.allowed_tools`，**不发明第二套**）；
  `executor.rs` 的 `tool_definitions()` → `tool_definitions_for(step)`，`None` 回落全局
- **验收**：`flexible::executor` 新用例（`None` 回落；`Some([])` 不给工具）
- **阻塞**：**Q2 / Q3**
- **对应**：§2.1 **C1**；先例 `core/src/planner/coarse/coarse_types.rs:96` + `planner/react/tool_executor.rs:76`

### 5. 守卫组（P1，无 LLM，当次生效）

- **5a 重复调用守卫**：照抄 `default_react_agent.rs:398` 的 `max_repeats = 3` → 同签名连续 3 次即中断并给诊断
- **5b 启动前 lint**：`#En` 引用 ⊆ `dependencies`、`result_reference` 唯一、依赖存在且无环、占位符有定义（默认**仅警告**，Q10）
- **5c 工具层参数校验 + 结构化错误**：报「path 是相对路径，请传绝对路径」，不是 OS 原文
- **验收**：单测三条各自可复现
- **对应**：§2.1 **D1 / D2、B5、C4**

### 6. 开关 + 写回端口（P2 前置）

- **新**：`improvement.rs` → `ImprovementStore` trait（**写端口**，与 `Arc<dyn AiClient>` 同类）+ `StepImprovement`
- **改**：`run_service/types.rs` 的 `RunRequest` 加 `improvements: Option<Arc<dyn ImprovementStore>>`
- **GUI**：`GuiImprovementStore`（写库 + `TemplateNotifier::notify()`，持 `Mutex` 串行化）
- **落库**：`PlansFlexibleSessionsRepo::update_step_improvement` —— **不得复用 `produce`**（会置 `produced` + `closed_at`），只更 `parameterized_task` + `updated_at`
- **UI**：开关放 `pipeline.rs:36` 块头；**只管「学」，守卫类不进开关**
- **验收**：`FakeImprovementStore` 能记录到调用；开关 OFF 时零调用
- **对应**：§7、「继承自旧稿」的写端口纪律

### 7. 成功配方 `preferred_tools`（P2，**纯事实提取，不需 LLM**）

- **改**：`template.rs` 加 `PlanStep.preferred_tools`；从条目 1 的 `tool_sequence` 取**首次 `ok = true`** 的工具
- **注入**：user task 加一段「本步上次的做法（供参考，可偏离）」
- **验收**：`flexible::executor` + `FakeImprovementStore`；`--lib flexible::` 全绿
- **对应**：§2.1 **A1 / A2** 的观测面 + §5.5

### 8. `intent` 改进建议 + 总结 AI + 候选审核（P2，需 LLM）

- **改**：`improvement.rs` 加 `summarize_*` + `StepImprovement { intent?, allowed_tools?, preferred_tools?, evidence }` —— **`evidence` 是硬要求**
- **改**：`prompt.rs` 加两个 system prompt 常量；候选审核 UI（默认候选，**不自动覆盖 `intent`**，Q4 / Q5）
- **验收**：手工走一遍「开关 → 候选可见 → 采纳 → 写回 → 下次执行带上」
- **对应**：§2.1 **B1–B3** + §5.6 / §5.7

### 9. 度量与执行记录落库（P2，看数据再定）

- 先观察条目 1 的真实数据，再决定是否建执行记录表（**Q7 默认不建**）
- **指标**：`rounds` / 工具序列 / 探测性调用计数 / 是否注入改进 / 同模板多次对比
- **硬闸**：**开关前后测不出差异 → 停手，不继续 P2**
- **对应**：§2.3、§8

## 10. 待拍板点

| # | 问题 | 默认建议 |
|---|---|---|
| Q1 | 环境事实注入落点 | **system prompt**（全局事实、每步一致） |
| Q2 | 按步工具：硬约束（`allowed_tools`）还是软提示（`preferred_tools`）还是两者 | 第一版**只做软提示**（硬约束有过窄卡死的风险） |
| Q3 | `allowed_tools` 由谁产出 | **用户手填 + 人工确认**，不由 LLM 自动产出 |
| Q4 | 改进建议是否自动写回 | 默认**候选 → 审核**（与用户「自动」的预期有出入，见 Q6） |
| Q5 | `intent` 改进是否自动覆盖 `intent` | 默认**只产出建议，不自动覆盖** |
| Q6 | 开关语义：只需「学」（产出改进）还是也要能关「用」 | 默认**只管「学」**（§7 末段）；若你要「一键全关」，加一个二级开关 |
| Q7 | 是否立刻建执行记录表 | 默认**不建**，先看条目 1 的真实数据 |
| Q8 | P0/P1 是否先单独交付（不等 P2） | 默认**是**（条目 1–5 无 LLM、当次生效） |
| Q9 | 重复调用守卫的阈值 | 照抄 planner：**同签名连续 3 次**即中断并给诊断 |
| Q10 | 启动前 lint 是「拒绝启动」还是「仅警告」 | 默认**仅警告**（左面板提示），不阻塞 —— 避免 lint 误判把本可跑的计划挡住 |
| Q11 | `max_rounds_per_step = 50`（`executor.rs:40`）是否调小 | 默认**不调**，先看 §8 的数据 |
| Q12 | `working_dir` 是否进环境段 | 默认**不注入**（含 `C:\Users\<用户名>`，属**外发**隐私，见 §5.2.5） |
| Q13 | 环境段是否含「可用命令列表」 | 默认**不含**（要跑 `where` / `which`，代价大）→ 用 `notes` 人工补 |
| Q14 | `notes` 由谁维护、是否给左面板加输入框 | 第一版**留空**由宿主填；左面板输入框列为后续可选项 |

## 11. 风险与边界

| 风险 | 说明 | 缓解 |
|---|---|---|
| **改错方向** | 若真实弯路主要来自别的成因，§5.2/§5.3 收益为零 | §8 的「测不出差异就停手」是硬闸 |
| 收窄工具**过窄** | 排除掉必需工具 → 必然失败 | Q2 默认只做软提示；硬约束需人工确认（Q3） |
| 环境事实**过期** | 换了机器 / 换了 shell | 每次执行**重新探测**（不落库、不缓存） |
| 成功配方**是巧合** | 上次参数恰好对 → 被当成标准答案 | 只作**提示**不作约束；可删；永不覆盖 `intent` |
| `preferred_tools` **膨胀** | 每次执行都追加 | **覆盖**语义（只留最近一次成功路径），不是累积 |
| 目标歧义**无法自动消除** | 有些歧义只有真跑到那一步才暴露 | 只产出**建议**，由人拍板（Q5） |
| 环境段占 token | 每步 system 多几十 token | 可接受（换掉的是几轮试错的 prompt） |
| 两个开关混淆 | 「学」与「用」分离，用户可能不解 | UI 上分开标注（开关 + 字段只读展示） |

## 12. 与既有设计的关系

| 关系 | 说明 |
|---|---|
| 取代 `flexible-guidance.md` | 该稿把三类问题压成一个自由文本字段；**继承**其中的写端口（trait 接缝）、「不得复用 `produce`」、快照可展示 |
| 复用 `skill-execution.md` 的先例 | `CoarseGrainedStep.recommended_tool_categories`（`core/src/planner/coarse/coarse_types.rs:96`）**已经**在做 per-step 工具收窄（`react/tool_executor.rs:76`）—— `flexible` 照抄形态，不发明新机制 |
| 对齐 `flexible-executor.md` 阶段 6 | 该阶段（执行记录）明确「待表结构定案」（`:484`）；§5.1 用报告 + UI 先给出度量数据，为定案提供依据 |
| 不违反 `flexible-run-service.md` §12 | 内核仍**不查库**；新增的是**写端口**（与 `Arc<dyn AiClient>` 同类的能力端口），不是被否决的「读接缝」 |
