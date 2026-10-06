# planned-agent/flexible 开发须知

> 面向后续开发的 LLM / 贡献者。**改 `flexible/` 之前先读这份。**
> 本文引用的路径除特别说明外均**相对本目录**（`crates/planned-agent/src/flexible/`）；
> 与代码冲突时**以代码为准**，并顺手更新本文。

## 0. 一句话

`flexible/` 是**灵活模式的执行内核**：把落库的**灵活计划模板**（`{ task, inputs, steps, output_schema }`）
连同本次运行参数跑起来 —— 逐步执行、大产出落盘、按输出契约整理最终结果，
再把**进度事件**与**执行报告**交给宿主。

它是**纯库**：只认 `AiClient` + `ToolRegistry` + 宿主递进来的模板与参数。
拿到模板 / 配好 AI 客户端 / 展示进度，都是宿主（目前只有 `crates/agent-gui`）的事。

两种执行模式的对照（根 `README.md:22-27`）：

| | 周密模式 (Thorough) | 灵活模式 (Flexible，本目录) |
|---|---|---|
| 路径 | 澄清 → Coarse 规划 → ReAct 探路 → 固化脚本 | 自由执行 → 轨迹提取 → Coarse 提炼 → 保存计划 |
| 执行器 | `core::planner` + `crate::chat` | **这里** |

## 1. 定位与边界

**实际依赖**（`crates/planned-agent/Cargo.toml`）：`planned-agent-core`
（`AiClient` / `host::RuntimeEnvironment` / `mcp::types::Tool` / `tool_registry`）、
`planned-agent-tool-manager`（`ToolRegistry`），外加 `tokio` / `serde` / `serde_json` /
`anyhow` / `tracing` / `futures` / `async-trait`。

| 是 | 不是 |
|---|---|
| 执行内核：吃「模板 + 参数 + 客户端 + 配置」 | 读库 / 落库（宿主 agent-gui 做） |
| 进度事件与执行报告的生产者 | dioxus / UI 组件 |
| `run_service`：常驻命令循环 + 状态表 + 订阅 | 策略决策（「会话有没有定稿模板」「有没有配 AI 客户端」由宿主解析成 `Err`） |
| `plan/`：模板的静态形态 | 模板的编辑 / 保存（GUI 侧） |

**硬边界**（`mod.rs:6-8`、`run_service/mod.rs:9-12`）：不依赖 `agent-gui`、不依赖 `dioxus`、
不依赖 `sea-orm`/storage、不依赖 `crate::planner`。已 grep 核实**无一条生产 `use` 违反**。

**对外路径**（`mod.rs:29-30`）：外部只写 `planned_agent::flexible::{PlanStep, ExecutorConfig, ...}`；
`run_service` **不做顶层 re-export**，统一走 `planned_agent::flexible::run_service::X`
（导出清单见 `mod.rs:42-53`、`run_service/mod.rs:30-37`）。**目录重组不得改变对外路径。**

**内核 / 宿主的接缝**：内核只收「可执行所需的四样」——模板 / 参数 / AI 客户端 / 执行配置
（`run_service/mod.rs:9-12`）。`RunRequest` 实际 6 个字段（另加 `session_id`、`environment`，
`run_service/types.rs:417-431`）：**没有存储句柄、没有 provider 查找、没有接缝 trait**。
`environment: None` 时行为与引入「运行环境段」之前**逐字一致**。

## 2. 目录地图

三段分组：「计划长什么样 / 怎么跑一次 / 宿主怎么驱动」（官方文本见 `mod.rs:10-27`）。

### `plan/` —— 计划期：落库模板的静态形态
只描述「计划长什么样」，不关心怎么跑。

| 文件 | 放什么 | 主要类型 / 函数 |
|---|---|---|
| `plan/template.rs` | 模板强类型，对应 `plans_flexible_sessions.parameterized_task` 列 | `FlexiblePlanTemplate{task,inputs,steps,output_schema}`(:12)、`PlanInput`(:33)、`PlanStep`(:46)、`from_json`(:60) |
| `plan/output_schema.rs` | **输出契约的唯一定义处**（保存校验 / 执行期整理 / GUI 展示都走这里） | `OutputKind`(:19，`ALL` 6 值 :36)、`OutputSchema`(:83)、`OutputSchema::parse`(:102) |
| `plan/placeholder.rs` | `${name}` 的收集 / 校验 / 替换 | `PLACEHOLDER_FIELDS`(:17)、`collect_placeholders`(:26)、`collect_from_steps`(:48)、`collect_from_schema`(:72)、`validate`(:93)、`render`(:126)、`render_lenient`(:154) |
| `plan/params.rs` | 本次运行的参数值表 + 步骤 `intent`/`expected_output` 展开 | `PlanRunParams`(:20)、`from_template`(:34)、`render_step_intent`(:77)、`render_step_expected_output`(:92) |

### `exec/` —— 执行期：怎么跑一次
`executor` 总编排，`step` 单步执行，`event` / `report` 是两条对外输出通道。

| 文件 | 放什么 | 主要类型 / 函数 |
|---|---|---|
| `exec/executor/mod.rs` | **总编排** | `FlexibleExecutor`(:47)、`new`(:54)、`run`(:65)、`resolve_result`(:346，输出整理步) |
| `exec/executor/config.rs` | 执行配置与默认常量 | `ExecutorConfig`(:26) + `DEFAULT_*`（见 §4-⑨） |
| `exec/spill.rs` | 大文本落盘 + 预览渲染（**跨步产出与步内工具输出共用**） | `SpillKind`(:31)、`StoredOutput`(:43)、`SpilledOutput`(:50)、`spill_text`(:69)、`spill_output`(:98)、`render_spill_reference`(:118)、`render_prior_output`(:145) |
| `exec/executor/resolve.rs` | 输出整理步的取数与兜底 | `RESOLVE_RESULT_REFERENCE = "#RESULT"`(:10)、`deliverable_output`(:17)、`resolve_failed_record`(:29) |
| `exec/executor/prior.rs` | 前序产出注入 + 依赖校验 | `collect_dependency_issues`(:19)、`collect_prior`(:43)、`placeholder_record`(:66) |
| `exec/executor/tools.rs` | 工具定义表组装 | `tool_definitions_for_names`(:8)、`to_tool_definition`(:18) |
| `exec/executor/logging.rs` | 日志渲染（压单行 + 封顶） | `log_output`(:10)、`summarize_tools`(:34) |
| `exec/step/mod.rs` | **单步执行**：一次「LLM ⇄ 工具」循环 | `OUTPUT_MAX_CHARS = 8_000`(:31)、`StepInput`(:34)、`StepRunResult`(:53)、`run_step`(:64)、`run_output_resolve`(:81) |
| `exec/step/llm.rs` | 单次 LLM 调用：超时 / 重试 / 取消 | `is_cancelled`(:12)、`wait_cancel`(:20)、`with_timeout`(:39)、`request_llm`(:58) |
| `exec/step/render.rs` | 消息与入参的渲染 / 摘要 | `tool_message`(:23)、`describe_arguments`(:57，全量 800)、`describe_tool_args`(:73，`$` 行 120)、`truncate_chars`(:116)、`summarize`(:123) |
| `exec/prompt.rs` | 内置提示词常量（**私有模块**，`exec/mod.rs:11`） | `STEP_SYSTEM_PROMPT`(:12)、`step_system_prompt`(:38)、`OUTPUT_RESOLVE_SYSTEM_PROMPT`(:134)、`build_step_task`(:147)、`build_output_contract_text`(:172) |
| `exec/event.rs` | 进度事件（唯一对外进度通道） | `PlanRunEvent`(:12)、`PlanRunSink`(:55)、`ChannelSink`(:63)、`NullSink`(:83) |
| `exec/report.rs` | 执行报告 | `StepStatus`(:11)、`CallUsage`(:24)、`ToolCallRecord`(:40)、`StepRunRecord`(:51)、`PlanRunReport`(:108) |

### `run_service/` —— 宿主侧：常驻执行服务（零依赖接缝）

| 文件 | 放什么 | 主要类型 / 函数 |
|---|---|---|
| `run_service/mod.rs` | 门面 + 组装函数 | `new_run_service(store, tools) -> (Arc<RunService>, RunServiceCore)`(:51) |
| `run_service/types.rs` | 纯数据：命令 / 请求 / 快照 / 订阅 | `SessionId`(:19)、`RunStatus`(:34)、`StepPhase`(:58)、`StepTrackLine`(:114)、`StepSnapshot`(:128)、`RunSnapshot`(:233)、`RunUpdate`(:342)、`RunNotice`(:373)、`StartRejectReason`(:386)、`SessionFilter`(:395)、`RunRequest`(:417)、`RunCommand`(:448) |
| `run_service/state.rs` | 事件 → 快照归并（**纯函数**，可脱离 tokio 单测） | `apply_event`(:16) |
| `run_service/store.rs` | 状态表 + 订阅登记 + 取消通道（三表合一） | `RunStore`(:27)、`update`(:67)、`notify`(:98)、`subscribe`(:137)、`cancel`(:171) |
| `run_service/core.rs` | 常驻命令循环 + 事件 sink | `RunServiceCore`(:37)、`run`(:65)、`RunLoop::start`(:109)、`on_event`(:214)、`ServiceSink`(:265) |
| `run_service/service.rs` | 宿主句柄（GUI / 将来的 CLI 唯一需要持有的东西） | `RunService::{start, stop, snapshot, snapshots, subscribe, unsubscribe}`(:40/:60/:65/:70/:78/:87) |

### `testing.rs` —— 测试桩
`#[cfg(test)] mod testing;`（`mod.rs:39-40`）挂载，成员全 `pub(crate)`，**不对外暴露**：
`FakeAiClient`(:20) / `text_response`(:83) / `tool_response`(:95)、`FakeTool` + `fake_tool`(:153/:162)、
`RecordingSink`(:204)。消费方只有三处测试（`exec/executor/tests.rs:14`、`exec/step/tests.rs:4`、
`run_service/core.rs:296`）。**新测试桩加这里。**

## 3. 一次执行怎么走

```text
plans_flexible_sessions.parameterized_task (JSON 文本)
   │ 宿主读库 + FlexiblePlanTemplate::from_json            plan/template.rs:60
   ▼
RunRequest { session_id, template, params, client, config, environment }   run_service/types.rs:417
   │ RunService::start → 命令队列                          run_service/service.rs:40
   ▼
RunServiceCore::run 常驻循环 → RunLoop::start → FlexibleExecutor::run
   │                                   run_service/core.rs:65/:109, exec/executor/mod.rs:65
   │ ① 依赖校验（只警告）                                   executor/prior.rs:19
   │ ② system prompt 整次只算一次                           exec/prompt.rs:38
   │ ③ 每步：展开 ${} → StepStarted → collect_prior → run_step
   │                                                     executor/mod.rs:108-256
   │ ④ 产出超阈值落盘                                      exec/spill.rs:98
   ▼
success = 模板每步都 Done（**在追加整理步之前算**）        executor/mod.rs:262
   ├─ 无契约   → 结果 = 交付步原文（不再调 LLM）           executor/mod.rs:355-363
   ├─ 契约非法 → 补一条 Failed 整理步，result=None，任务仍可 success   :365-378
   └─ 有契约   → 跑 #RESULT 整理步（只给 builtin_read_file_lines + builtin_grep_file）  :380-462
   ▼
PlanRunReport ─► PlanRunEvent::RunFinished ─► apply_event 归并进 RunSnapshot
                                          run_service/state.rs:16
```

被测试钉住的几条契约：
- **单步失败不抛错**：该步 `Failed`，其后各步 `Skipped`，报告 `success = false`；取消同理（`executor/mod.rs:112-129`）。
- **展开失败 = 该步失败**（缺参数 / 未定义占位符），**在发起 LLM 之前**就判（`executor/mod.rs:134-154`）。
- **`success` 不含整理步**：整理步失败只影响「有没有最终结果」（`executor/mod.rs:260-266`）。
- **记录存模板原文，prompt 用展开值**：`StepInput.step` vs `StepInput.expected_output`（`exec/step/mod.rs:34-50`）。
- **空产出（正文与思考都空）判 `Failed`**，避免空串进结果表传给下游（`exec/step/mod.rs`）。

## 4. 硬约定

**① `#[serde(default)]` 是向后兼容的命脉**：落库 JSON 是历史数据（`plan/template.rs:26-27`）。
新增模板字段**必须**带 `#[serde(default)]`（`Option<T>` 也要显式写）；**没有** `deny_unknown_fields`。
`output_schema: null` 与「字段缺失」**同义**（= 没有结果契约，消费方同等对待）。

**② 严格 vs 宽容是两条渲染路径**：执行用 `placeholder::render`（缺值即 `Err`）；
UI 边填边预览用 `render_lenient`（保留占位符 + 回报 missing，纯空白视同缺失）。
**执行路径不得用 lenient。**

**③ 事件是唯一进度通道，执行器不感知 UI**（`exec/event.rs:1-4`）：宿主只实现 `PlanRunSink`；
`emit` 是同步调用，**不得阻塞 / panic**；接收端 drop 后事件静默丢弃 —— 进度不该拖垮执行。

**④「渲染一次，多处复用」**：同一份工具入参刻意渲染成两种精度，**不合并** ——
`describe_arguments`（全量 JSON，800 字符封顶）进日志 / 报告的结构化记录；
`describe_tool_args`（命令行式关键入参，120 字符封顶）进快照 `track` 的 `$` 行与 `StepToolCall.args`
（`exec/report.rs:31-38`）。同理 `tool_sequence`（结构化，给报告）与 `track`（给人看，给快照）
**同源不同用途**。

**⑤ `run_service` 的并发模型**（改前必读 `run_service/store.rs:1-8`）：
- 三张表合一的 `RunStore` 让「查询 / 订阅 / 取消」**同步**完成、绕开命令队列；
- **锁顺序恒为 `runs → subs`**；`broadcast` 的调用方**不得**持 `runs` 锁；
- **状态的唯一写入口是服务循环**（`run_service/core.rs`），别处只读或发通知；
- **通知（`RunNotice`）不写快照**：快照描述「某一次执行的状态」，被拒 = 这次执行没发生。

**⑥ 错误处理**：默认 `anyhow::Result`。**单步失败不向上抛**，转成记录里的 `Failed`/`Skipped`；
落盘 IO 失败同样只记该步 `Failed`，不静默降级。执行任务的 panic 由 `catch_unwind`
收敛为「本次执行失败」（`run_service/core.rs:168-179`）。

**⑦ 测试**：内联 `#[cfg(test)] mod tests`；体量大的用同目录 `tests.rs`
（`exec/executor/tests.rs`、`exec/step/tests.rs`，经 `mod tests;` 挂载）。**测试桩只放 `testing.rs`。**

**⑧ 提示词不走 `prompt-manager`**：执行器自带常量（`exec/prompt.rs:1-3`），
宿主无需为执行器加载任何 prompt 文件。

**⑨ 默认值常量**（`exec/executor/config.rs`）：`max_rounds_per_step = 50`、`DEFAULT_CACHE_DIR = "./data/cache"`、
`DEFAULT_SPILL_THRESHOLD_CHARS = 8_000`（与 `OUTPUT_MAX_CHARS` 对齐）、`DEFAULT_SPILL_PREVIEW_CHARS = 800`、
`LOG_OUTPUT_MAX_CHARS = 2_000`、`DEFAULT_LLM_TIMEOUT_SECS = 180`、`DEFAULT_LLM_TIMEOUT_RETRIES = 1`。

## 5. 新的东西放哪

| 你想加 | 放哪 | 附带要求 |
|---|---|---|
| 模板新字段 | `plan/template.rs` | **必须** `#[serde(default)]`；同步 agent-gui 的保存校验与面板展示 |
| 输出契约新 `kind` / 新字段 | `plan/output_schema.rs`（唯一定义处） | 同步 `OutputKind::ALL`、`uses_fields`、GUI 同名字段 |
| 占位符语义变化 | `plan/placeholder.rs` | **同步 GUI 镜像实现（见 §7-⑫）** |
| 执行期新行为 | `exec/executor/` 下**新建小模块** | 保持「总编排只在 `mod.rs`」（config/resolve/prior/tools 就是这么拆的；`spill` 因 executor 与 step 共用，提升到了 `exec/` 层） |
| 新的进度事件 | `exec/event.rs` | 同步 `run_service/state.rs::apply_event`（**穷尽 match，无 `_` 分支**）与快照字段 |
| 报告新指标 | `exec/report.rs` | 可选字段加 `#[serde(default)]` |
| 宿主新查询 / 命令 | `run_service/types.rs` + `service.rs` + `core.rs` | 启动走命令队列；查询 / 订阅 / 取消走 `RunStore` |
| 执行器提示词 | `exec/prompt.rs` 常量 | 不要引入 prompt 文件依赖 |
| 新测试桩 | `testing.rs` | 不加新的 public 测试模块 |

## 6. 怎么跑测试

```powershell
cargo test -p planned-agent --lib flexible::      # 本目录全部单测
cargo test -p planned-agent --test lib_api        # 只验 pub use 对外路径可达（不覆盖 flexible）
cargo test -p planned-agent-gui --bins            # 宿主侧（消费方）
```

用例分布（实测 `#[test]` + `#[tokio::test]` 共 95）：`exec/prompt.rs` 10、`exec/report.rs` 2、
`exec/executor/tests.rs` 21、`exec/step/tests.rs` 9、`plan/output_schema.rs` 6、`plan/params.rs` 7、
`plan/placeholder.rs` 10、`plan/template.rs` 5、`run_service/core.rs` 8、`run_service/state.rs` 6、
`run_service/store.rs` 11。

⚠️ **既有 baseline 失败，不是本目录的锅**：`cargo test -p planned-agent`（不带过滤）有
**3 个 `planner::coarse::llm_planner` 用例**失败，原因是运行时找不到 prompt `planning/coarse_plan`
（测试硬编码仓库根 `prompts/`，而真实 prompt 目录是 `crates/agent-gui/prompts`）。**不要顺手改**；
跑本目录时用 `--lib flexible::` 过滤。

## 7. 已知的坑（都踩过）

1. **`from_record` 会整体覆盖 `StepSnapshot`**（`run_service/types.rs:184`）→ 走 `StepFinished`
   时必须先 `mem::take` 接住 `track` 再覆盖（`state.rs:50-57`）；`RunFinished` 用报告重建全部步时
   要按 `index` 把轨迹接回（`state.rs:70-83`）。**否则一执行完 think box 就空。**
2. **`RunFinished` 覆盖步骤有边界**：`report.steps` 为空时**保留现有步**（别清空已展示进度）；
   `intent` 展开失败 / 被跳过的步执行器**不发** `StepFinished`，最终相位只在报告里。
3. **取消通道只在「真正采纳了该终态事件」时才清**（`run_service/core.rs:242-245`）：否则一条迟到
   终态事件会清掉**新一轮**的取消通道，让新轮「停止」失效。`run_id` 不匹配的事件一律丢弃（`state.rs:15`）。
4. **`stop` 直连 `RunStore`、同步生效**（`run_service/service.rs:60-62`）；因此 `RunLoop::start` 里
   刻意**先 `register_cancel` 再写起跑快照**（`core.rs:139-142`）—— 两步之间点停止会落空。
5. **`unsubscribe` 只保证不再 push**：通道里已缓冲的旧消息仍会被读到，宿主必须用
   `RunUpdate::session_id()` 过滤（`run_service/types.rs:356-366`）。
6. **`is_cancelled` 有两份**（`exec/executor/mod.rs:466` 与 `exec/step/llm.rs:12`，同名同语义）——
   改一处别忘另一处。
7. **spill 阈值是「含等号」的 `<=`**：等于阈值仍内联，多 1 字符才落盘（`exec/spill.rs:69-96`）。
   落盘 **≠ 省内存**：全文始终留在调用方手里，落盘只是「不把全文塞进上下文」；
   文件名用**序号**而非 `result_reference`（防目录穿越）。
   **两个消费者**：跨步产出（`executor`，`step-<i>.txt`，IO 失败 → 该步 `Failed`）与
   步内工具输出（`step`，`tool-s<步>-r<轮次>-<序号>.txt`，IO 失败 → 告警 + 回退内联，**不**判该步失败）。
8. **`OUTPUT_MAX_CHARS`(8000) 只截「记录侧」**（`exec/step/mod.rs:322-331`）：`StepRunResult.output`
   保持不截断，下游 `prior` 依赖它。
9. **依赖校验只警告不阻断**（`exec/executor/prior.rs:19`），运行期 `collect_prior` 兜底跳过未命中引用。
10. **`cache_dir` 相对进程 cwd**，执行器自建 `run-<毫秒>-<序号>` 子目录隔离每次执行；
    **会话段由宿主拼**（`exec/executor/config.rs:37-42`）。
11. **超时语义**：`llm_timeout` 是「**一次 `chat_completion` 调用**」的墙钟上限（含 ai-openai 内层重试），
    **不是**整步 / 整次超时；`llm_timeout_retries` **只重试超时**（其它失败在下层已重试，再叠加会倍数放大）
    —— `exec/executor/config.rs:47-57`。
    **空回答重发**（`llm_empty_retries`，默认 1）是**另一条独立的线**：管「**拿到了响应但没有内容**」
    （provider 空响应 / 推理预算耗尽；空回答是 HTTP 200，内层重试覆盖不到）。重发的是**同一条请求**
    （messages 未变 ⇒ 不重复执行工具，对比「整步重跑」会），**不占** `rounds`，故
    `call_usages.len() == rounds + llm_retries`；用尽后该步 `Failed`，理由带「已重发 N 次」
    （hardening 稿 A6，`exec/step/mod.rs`）。诊断：空回答的 WARN 带 `finish_reason`，用于区分
    「provider 空响应」与「被 `max_tokens` 截断」。
12. **GUI 有 `placeholder` 的镜像实现**：`crates/agent-gui/src/pages/plan/flexible/placeholder.rs`。
    两份 `validate` **签名不同**（本目录版 `(steps, inputs)` 只校验 `steps`；GUI 版
    `(steps, schema, inputs)` 还覆盖 `output_schema` 的 `goal`/`success`/`format`，**落库走 GUI 那份**）。
    改契约**必须同步两处**。
13. **本目录 `plan/placeholder.rs` 的 `collect_*` / `validate` 在 `planned-agent` 内没有生产调用点**
    （只有定义、`mod.rs:51` re-export 与测试；真正的校验发生在 GUI 保存路径）。看到「没人用」是正常的，**别删**。
14. **`plan/params.rs` 有个乱码注释、且文件行尾 CRLF/LF 混用** —— 编辑时用定向替换，
    **不要整文件重写**，否则 diff 会炸。
15. **环境段的位置是刻意的**：`step_system_prompt(Some(env))` 只在**尾部**追加固定内容
    （为了命中 provider 前缀缓存），`None` 时逐字返回基准串（`exec/prompt.rs:26-46`）；
    整次执行只算一次、每步复用。

## 8. 设计稿在哪（`docs/planned-agent/`）

| 文档 | 主题 | 状态 |
|---|---|---|
| `flexible-executor.md` | 执行器内核：模板 + 参数 → 逐步执行 → 报告 / 事件 | 🚧 内核已实现（宿主接线 / 落库另见下两行） |
| `flexible-run-service.md` | 宿主侧常驻执行服务、零依赖接缝 | ✅ 已实现 |
| `flexible-step-output-spill.md` | 大产出「落文件 + 句柄传递」取代硬截断 | ✅ 已按稿实施 |
| `flexible-step-tool-output-spill.md` | 单步内工具输出「落盘 + 句柄」（只做不内联；chat 侧同类问题待做） | ✅ 已按稿实施 |
| `flexible-output-step.md` | 新增「输出定义」步（`output_schema` 的来源） | ⚠️ 设计稿（未实施，落点在 GUI/prompts 侧） |
| `flexible-output-followups.md` | 面板展示 `output_schema` 与本次结果 | ✅ 已实施（GUI 侧） |
| `flexible-execution-audit.md` | 一次真实执行日志的只读诊断 | 📋 待逐条拍板 |
| `flexible-execution-hardening.md` | 加固设计（A/B/C/D 共 16 项） | 📋 待确认后才动代码 |
| `flexible-execution-improvements.md` | 少走弯路：per-step 指导字段 / 工具选择反哺 | 📋 设计待审 |

⚠️ 这些稿子里的**用例数、行号、文件路径会漂移**（例如 spill 稿写 88 例、run-service 稿写 55 例，
都已是历史值）。**以代码为准。**

## 9. 宿主侧对照（`crates/agent-gui`）

改本目录的对外契约前，先看这些消费点（符号名稳定，行号仅作快速定位）：

- 组装与驱动：`services/run_service.rs` 的 `start_run_service`（`new_run_service` + `spawn_forever(core.run())`）、
  `main.rs` 的 `use_hook(… start_run_service …)` + `use_context_provider(move || run_service)`。
- 订阅消费：`services/run_service.rs` 的 `use_run_service` / `use_run_subscription`；
  点执行 → `start_run_with_template`，点停止 → `RunService::stop`（调用点在 `pages/plan/left_panel/`）。
- 计划期类型 / 配置：`services/plans_flexible_service.rs`（`FlexiblePlanTemplate`）、
  `services/run_service.rs`（`ExecutorConfig`、`PlanRunParams`）、
  `pages/plan/left_panel/{left_panel,output_schema,params}.rs`、`pages/plan/flexible/placeholder.rs`。
