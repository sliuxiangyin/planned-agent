# flexible 流程 Prompt 整理

> 整理对象：`crates/agent-gui/prompts/flexible/*.toml`（协调器 + step1~step5）。
> 除 prompt 文本外，**均以代码里的真实工具 schema / 回调 / 白名单为准**核对过，出处见每节末尾的 `file:line`。
> 本文档是说明文档，位于仓库根 `docs/`，**不会被 prompt-manager 加载**（它只扫描 `prompts/` 目录）。
>
> 相关设计文档：`docs/chat-flexible-多会话保活设计.md`、`docs/chat-flexible-回调会话归属设计.md`、
> `docs/chat-flexible-流程审查与优化建议.md`。

---

## 一、状态机总览

流程中间状态持久化在 `flexible_state` 表，按会话（`plan_id + session_id`）一行。`current_step` 顺序推进：

```
none → task_defined → executed → fields_selected → params_confirmed → templated
        (step1 定稿)   (step2 成功)  (step3 定稿)      (step4 定稿)       (step5 回调推进)
```

| 档位 | 该档位允许存在的产物（`products` 的不变量） |
|---|---|
| `task_defined` | `task_definition`、`output_format` |
| `executed` | `execution_trace`、`compressed_context` |
| `fields_selected` | `field_selection_result` |
| `params_confirmed` | `parameter_confirmation_result` |
| `templated` | `template_payload`（整段模板副本，供 `flexible_save_template` 直接读取落库） |

规则（现由各 step 的完成回调各自执行，见 `step_callback/stepN/mod.rs`）：

- **读-改-写合并**：`merge_state` 未传的 key 保留、传 `null` 的 key 清除、`current_step` 未传则保留原值。
- **只增不减**：`current_step` 仅在明确「重做 / 返回」上游时回退；该 step 一旦定稿，回调会在同一次写入里把其下游产物传 `null` 清除。
- 失败不推进：任何 step 返回非定稿 status 时，不写产物、不动 `current_step`。
- **协调器不写状态**：`flexible_state` 工具已改为只读，状态只由回调登记。

出处：`crates/agent-gui/src/pages/plan/flexible/step_callback/stepN/mod.rs`、`tool/flexible_state.rs`、`prompts/flexible/flexible_global_system.toml`。

---

## 二、协调器（`flexible_global_system.toml`）

| 项 | 内容 |
|---|---|
| 模板变量 | `session_id`（必需），注入到「会话上下文」段 |
| 注册位置 | `crates/agent-gui/src/pages/plan/flexible/chat_service_factory.rs:51` |
| 代码白名单 | `flexible_step1~5`、`flexible_state`（只读）、`flexible_save_template`、`request_user_action` |
| 职责 | 纯状态机调度：路由到 step、只读 `flexible_state`、发 `request_user_action`、调 `flexible_save_template` 落库；**不碰业务工具、不做需求澄清、不写状态** |
| 输出契约 | 只读每个 step 返回 JSON 的顶层 `status`；**不解析正文、不向用户回显 JSON**（用户看到的是协调器转述的自然语言） |

step → `status` → 定稿档位：

| step | `status` 取值 | 定稿档位 |
|---|---|---|
| `flexible_step1` | `task_defined` / `ignored` / `cancelled` | `task_defined` |
| `flexible_step2` | `success` / `error` | `executed` |
| `flexible_step3` | `fields_selected` / `empty_result` / `back_to_execute` / `cancelled` | `fields_selected` |
| `flexible_step4` | `params_confirmed` / `back_to_step3` / `cancelled` | `params_confirmed` |
| `flexible_step5` | `success` / `error` | `templated` |

另有两条优先级规则：

1. **用户指定步骤直达（最高优先级）**：用户点名「执行 / 直接做 / 跳到 / 重新执行」某一步时，先 `flexible_state.load` 盘点前置产物；齐备则直调该 step，缺失则回报缺什么并停下（不擅自补跑）。
2. **产物一致性原则**：重做上游 step（含 `back_to_execute` / `back_to_step3`）时，同一次 `save` 里把该 step 下游产物一并传 `null`。

---

## 三、五个 step 的职责 / 入参 / 出参

### 3.1 `flexible_step1` — 需求澄清

- **职责**：把自然语言需求澄清为「一句话任务 + 参数表 + 输出格式」；只追问影响任务定义的关键信息；最多 3 次交互；**不执行任务**。
- **允许工具**：`request_user_action`（仅此一个）；depth 1 / max_depth 2。
- **入参 schema**（`page.rs:202-223`）

| 参数 | 类型 | 必需 | 说明 |
|---|---|---|---|
| `user_message` | string | ✅ | 用户本轮输入 |
| `conversation_summary` | string | | 历史对话摘要（**仅背景**，不作任务基线） |
| `previous_task_definition` | object | | 任务基线 `{task, params}`；优先级：`flexible_state` 已定稿 > 上一次 step1 返回 |
| `previous_output_format` | string | | 与基线配套的输出格式 |

- **出参**（纯 JSON，顶层 `status`）

```json
{ "status": "task_defined",
  "task_definition": { "task": "一句话任务描述", "params": { "任务类型": "统计", "数据源": "MySQL" } },
  "output_format": "CSV" }
{ "status": "ignored" }      // 与当前任务无关
{ "status": "cancelled" }    // 用户取消
```

- **落 state**：由 **`flexible_step1` 完成回调**自动登记——`status:"task_defined"` ⇒ 写 `task_definition` / `output_format`、清 step2~4 全部下游产物、推进 `task_defined`（`step_callback/step1/mod.rs`）；协调器不再写状态。
- **强约束**：`output_format` 必须由用户明确确认，禁止默认值 / 推断；缺基线且用户未指定时必须用单选 `request_user_action` 询问。

### 3.2 `flexible_step2` — 任务执行

- **职责**：按 `task_definition` 串行调用业务工具完成任务；高风险操作（删除 / 发送 / 提交 / 覆盖）先 `request_user_action` 确认。**执行轨迹由系统自动记录**（见下「落 state」），模型不再自述轨迹。
- **允许工具**：`all`（只暴露业务工具，剔除 Utility / SubAgent 类，避免误碰协调层工具）。
- **入参 schema**（`page.rs:239-256`）

| 参数 | 类型 | 必需 | 说明 |
|---|---|---|---|
| `task_definition` | object | ✅ | 来自 step1 |
| `runtime_context` | string | | 上一轮执行的 `compressed_context`；首次为空（**声明了但协调器从不传**，见 §六-5） |
| `host_session_id` | string | ✅ | 宿主控制字段，列入 `hidden_args`，不进子 agent 任务文本；供回调定位会话 |

- **出参**

```json
{ "status": "success",
  "compressed_context": "≤50 字：是否成功 / 用了什么工具 / 得到什么结果 / 是否有高风险操作被拒" }

{ "status": "error",
  "error_message": "失败工具名 + 具体参数 + 失败原因" }
```

- **落 state**：由 `flexible_step2` 的**结果链**自动登记，三环顺序不可颠倒：
  1. **前置分析**（`step_callback/prelude.rs` 的 `FlexibleStepPrelude`，所有 step 默认共用）：解析输出、定位 `host_session_id`、判定 `status == "success"`；解析失败 ⇒ 要求重新输出（最多 2 次，且要求**不得重跑工具**），非定稿 / 缺会话 ⇒ **链终止，一个产物都不登记**。
  2. **轨迹提取**（`step_callback/step2/execution_trace.rs`）：从**子 agent 会话历史**导出真实工具调用（`planned-agent/src/chat/trace.rs` 的 `export_tool_trace`），经 `step2/tool_trace.rs` 过滤 `request_user_action`、截断超长输出后写入 `execution_trace`（每条含 `name` / `arguments` / `output` / `outcome`）。模型自述的轨迹一律不采信；本轮一个工具都没调用就落**空数组**。
  3. **定稿登记**（`step_callback/step2/mod.rs`）：写 `compressed_context`，`current_step = "executed"`，并把 `field_selection_result`、`parameter_confirmation_result` 清掉（重跑 step2 ⇒ step3/step4 定稿作废）。

  第 2 环排在第 3 环**之前**：轨迹落库失败即 `Abort`，那时状态尚未推进（重跑 step2 即自愈）；反之会留下「已 `executed` 却没有轨迹」的状态。

### 3.3 `flexible_step3` — 字段选择

- **职责**：从 step2 执行结果里识别可用字段（结构化 / 非结构化 / 树形→点号路径），通过一次多选交互让用户勾选最终输出字段。**不得调用业务工具**。
- **允许工具**：`request_user_action`。
- **入参 schema**（`page.rs:276-293`）

| 参数 | 类型 | 必需 | 说明 |
|---|---|---|---|
| `execution_trace_summary` | string | ✅ | 来自 step2 的 `compressed_context` |
| `output_format` | string | ✅ | 来自 step1（已列入 `required`） |
| `host_session_id` | string | ✅ | 隐藏控制字段 |

- **出参**

```json
{ "status": "fields_selected",
  "field_selection_result": {
    "output_format": "CSV",
    "available_fields": [ { "name": "title", "description": "标题", "example": "示例值" } ],
    "selected_fields": ["title", "date"] } }

{ "status": "empty_result",      "message": "无数据可提取 / 无法自动提取字段（建议手动指定字段或重新执行）" }
{ "status": "back_to_execute",   "message": "可选补充原因" }
{ "status": "cancelled",         "message": "可选取消原因" }
```

- 规则：`selected_fields` 顺序即输出顺序；嵌套数据用点号路径作 `name`；字段必须来自真实执行结果。
- **落 state**：由 **`flexible_step3` 完成回调**自动登记——`status:"fields_selected"` ⇒ 写 `field_selection_result`、清 `parameter_confirmation_result`、推进 `fields_selected`（`step_callback/step3/mod.rs`）；协调器不再写状态。

### 3.4 `flexible_step4` — 参数确认

- **职责**：扫描 `execution_trace[].input` 收集具体参数值，识别「未来每次运行可能不同、值得参数化」的候选，让用户多选成为模板输入变量，产出模板输入定义。
- **允许工具**：`request_user_action`。
- **入参 schema**（`page.rs:311-332`，四项**全部必需**）

| 参数 | 类型 | 说明 |
|---|---|---|
| `execution_trace` | array | 来自 step2 的完整轨迹 |
| `output_format` | string | 来自 step1 |
| `field_selection_result` | object | 来自 step3（`output_format` / `available_fields` / `selected_fields`） |
| `host_session_id` | string | 隐藏控制字段 |

- **出参**

```json
{ "status": "params_confirmed",
  "parameter_confirmation_result": {
    "output_format": "CSV",
    "candidates": [ { "name": "date", "example": "2025-03-01", "description": "查询日期" } ],
    "selected_params": ["date"],
    "template_input": {
      "input_schema": [ { "name": "date", "type": "date", "required": true, "description": "查询日期", "example": "2025-03-01" } ],
      "output_type": "CSV",
      "output_fields": ["title", "date"],
      "tool_sequence": ["query_tool", "export_tool"] } } }

{ "status": "back_to_step3", "message": "可选补充原因" }
{ "status": "cancelled",     "message": "可选取消原因" }
```

- 约束：`input_schema[].type` 取 string / number / boolean / date / array；**`output_fields` 必须与 step3 的 `selected_fields` 一致**；候选必须来自真实轨迹。
- **落 state**：由 **`flexible_step4` 完成回调**自动登记——`status:"params_confirmed"` ⇒ 写 `parameter_confirmation_result`、推进 `params_confirmed`（`step_callback/step4/mod.rs`）；协调器不再写状态。

### 3.5 `flexible_step5` — 模板序列化

- **职责**：把 4 份上游产物编译成**混合模板**——`steps`（硬编码、零推理可跑）+ `execution_plan`（1:1 对应的智能修复说明书）。无用户交互。
- **允许工具**：`request_user_action`（实际不使用）；**有完成回调**（`step_callback/step5/mod.rs`）：推进 `templated` + 登记模板副本；落库仍由协调器调 `flexible_save_template` 执行。
- **入参 schema**（`page.rs:350-371`）

| 参数 | 类型 | 必需 | 说明 |
|---|---|---|---|
| `task_definition` | object | ✅ | 来自 step1 |
| `execution_trace` | array | ✅ | 来自 step2 |
| `field_selection_result` | object | ✅ | 来自 step3 返回 JSON 的 `field_selection_result` 对象（含 `output_format` / `available_fields` / `selected_fields`） |
| `parameter_confirmation_result` | object | ✅ | 来自 step4 |

（另有 `host_session_id`（`hidden_args`，必需）：step5 有完成回调，需据此判定状态归属）

- **出参**

```json
{ "status": "success",
  "input_schema": { "month": { "type": "string", "required": true, "description": "月份，格式 YYYY-MM", "example": "2026-09" } },
  "output": { "format": "CSV", "fields": ["order_id", "amount"] },
  "steps": [ { "id": "step_1", "tool": "query_tool", "params": { "query": "... WHERE month = '{{input.month}}' ..." } } ],
  "execution_plan": [ { "step_id": "step_1",
                        "intent": "获取指定月份的订单数据",
                        "expected_schema": { "order_id": "string", "amount": "number" },
                        "dynamic_hints": { "param_mapping": "...", "field_adaptation": "...", "fallback_tool": "...", "stable": true } } ],
  "metadata": { "created_at": "2026-09-01T12:00:00Z", "version": 1 } }

{ "status": "error", "message": "缺少必要输入数据" }
```

- 约束：
  - 成功时顶层固定为 `status`、`input_schema`、`output`、`steps`、`execution_plan`、`metadata` 六项。
  - `steps` 与 `execution_plan` **等长且 `steps[i].id == execution_plan[i].step_id`**；`params` 里**只允许两类占位符**：① 来自 `input_schema` 的输入参数写成 `{{input.<参数名>}}`（**每一次出现都要替换**，含嵌在长字符串内部的部分）；② 上游输出写成 `{{step_N.output}}`。其余值（工具固定选项、命令名、时间格式串）一律硬编码。
  - 字段必须来自输入，禁止臆造工具名 / 字段名 / 参数值（prompt 内的示例已标注为虚构）。
  - `input_schema` 为空时写成 `{}`。
- **落 state**：**状态推进 + 模板副本**由 `flexible_step5` 完成回调负责——`status:"success"` ⇒ 把 `current_step` 置为 `templated`，并把**整段输出**登记为产物 `template_payload`（`step_callback/step5/mod.rs`）。**落库**（写 `plans_flexible_sessions`）仍由 `flexible_save_template` 执行，但它改为**从 `flexible_state` 读这份副本**，不再由协调器转抄模板 JSON（见 §六-14）。回调先于协调器调 `flexible_save_template` 触发，故 `current_step` 会先到 `templated`。

---

## 四、两个旁路工具

### 4.1 `flexible_state`（协调器专用 · 只读）

定义：`crates/agent-gui/src/pages/plan/flexible/tool/flexible_state.rs`

| 入参 | 必需 | 说明 |
|---|---|---|
| `session_id` | ✅ | 原样照抄 system prompt 中的本会话 ID |

- **出参**：`{loaded, current_step, products}`（无记录时 `loaded=false`、`current_step="none"`、`products={}`）。
- **只读**：状态（`current_step` + `products`）由各 step 的完成回调登记，工具不再提供 `save`。`products` 的 key：`task_definition`、`output_format`、`execution_trace`、`compressed_context`、`field_selection_result`、`parameter_confirmation_result`、`template_payload`。

### 4.2 `flexible_save_template`

定义：`crates/agent-gui/src/pages/plan/flexible/tool/flexible_save_template.rs`

| 入参 | 必需 | 说明 |
|---|---|---|
| `session_id` | ✅ | 原样照抄 |

（**只有** `session_id`。模板数据**不来自参数**——工具自行读 `flexible_state` 的 `products.template_payload`；原 `template` 参数已移除，原因见 §六-14。）

- **出参**：成功 `{saved: true, id, plan_id, version}`；失败返回 error。
- **数据来源**：`flexible_state` 里 step5 回调登记的 `template_payload`；无该产物（step5 未定稿 / 重跑中）时返回 error 提示重跑 step5。
- **校验**：`steps` / `execution_plan` 必须存在、均为数组、长度相同且 `step_id` 一一对应；`status == "error"` 直接拒绝。
- 语义：同一会话反复产出会覆盖同一版本行，新会话首次产出新增一条。

---

## 五、参数流转

```
用户输入
  │
  ├─► step1 ──► task_definition(object) + output_format(string)
  │                 │
  ├─► step2 ◄───────┘  (+ host_session_id)
  │     └─► execution_trace(array) + compressed_context(string)     [callback 自动落 executed]
  │
  ├─► step3 ◄── compressed_context + output_format  (+ host_session_id)
  │     └─► field_selection_result(object)
  │
  ├─► step4 ◄── execution_trace + output_format + field_selection_result  (+ host_session_id)
  │     └─► parameter_confirmation_result(object)
  │
  └─► step5 ◄── task_definition + execution_trace + field_selection_result + parameter_confirmation_result
        └─► 模板 6 字段 ──► (回调) products.template_payload ──► flexible_save_template ──► plans_flexible_sessions
                            （`templated` 由 step5 回调即刻推进；落库工具读 state 里的副本，不经协调器转抄）
```

协调器传参要点（`flexible_global_system.toml`）：

- 含 `session_id` 的工具（`flexible_state`、`flexible_save_template`）必须原样照抄。
- 调任一 step 工具（`flexible_step1` ~ `flexible_step5`）时必须一并传 `host_session_id`（同样原样照抄）。
- step3 的 `execution_trace_summary` 取 step2 的 `compressed_context`；step4 的 `execution_trace` 取 step2 的完整轨迹。
- step2 的 `runtime_context` 取上一轮 step2 的 `compressed_context`（首次没有则不传；重试 / `back_to_execute` 重跑时必传）。
- 需求澄清前，用只读的 `flexible_state` 取 `products.task_definition` / `products.output_format` 作为**任务基线**（唯一来源）。

---

## 六、代码 ↔ prompt 一致性复核

本章原先列出的 7 项不一致，加上复核时新增的 5 项，**已全部处理**（改动记录见
`docs/chat-flexible-回调会话归属设计.md` §13 阶段 4 / 阶段 5）：

| # | 问题 | 处理结果 |
|---|---|---|
| 1 | 协调器 `allowed_tools` 缺 `flexible_save_template`（P0：模板无法落库） | 已加入白名单 |
| 2 | `max_tool_rounds: 2` + `builtin_read_documentation` 测试残留（P0） | 已还原：删除 `max_tool_rounds` 即回默认 10；残留条目保持注释态 |
| 3 | step5 入参 `field_selection_result` 声明 `string`、实为 object；prompt 写「纯文本输出」 | schema 已改 `object`，prompt 文案统一为「对象」 |
| 4 | step3 入参 `output_format` 未列 `required` | 已加入 `required` |
| 5 | step2 入参 `runtime_context` 声明却从不传 | 已补传（重试 / `back_to_execute` / 已有上一轮产物时带 `compressed_context`） |
| 6 | step2 双写（回调 + 协调器 `save`） | 已消除：`flexible_state` 改为**只读**，prompt 不再写状态 |
| 7 | prompt 写「整个流程分为四个阶段」而实际 5 步 6 档位 | 已改为「五个步骤」表 |
| 8 | 档位表把 `task_defined` 注为「step1 已确认」 | 已改为「step1 已澄清」（回调在用户点确认前即登记） |
| 9 | `flexible_state` / `flexible_save_template` 用 `session_id`，step 子 agent 用 `host_session_id` | 保留不动（两个 custom tool 各自独立，不撞名） |
| 10 | step5 回调早于 `flexible_save_template` 触发，`templated` 会先落地 | 已在 prompt 与设计文档注明顺序，属既定取舍 |
| 11 | prompt 把 `flexible_state` 描述为「流程状态的读写登记」 | 已改为只读 |
| 12 | step2 prompt 的「运行上下文」与 schema `runtime_context` 对应不明 | 已在 prompt 写清语义（上一轮运行记录） |
| 13 | **运行期实测**：`flexible_step1` 把 JSON 包进 markdown 代码块（且前面有两个空行），回调严格解析失败 → `flexible_state` 未登记 | 已修：回调改为逐级宽松解析（严格 → 剥围栏 → 取首个平衡对象）+ 失败时 `Retry` 让子 agent 重出；`flexible_step1.toml` 补「不要代码块、不要前后空行」 |
| 14 | **运行期实测**：step5 原始输出 `execution_plan[1].expected_schema` 是合法的 `null`，但协调器把 step5 输出「转抄」成 `flexible_save_template` 的 `template` 参数时写成 `""`（还把 `step_1` 的 `"array"` 改成 `{"type":"array"}`）→ 落库数据不合契约 | 已修（**模板不再经 LLM 手**）：step5 的定稿登记回调把整段输出登记为 `products.template_payload`；`flexible_save_template` 移除 `template` 参数（schema 仅 `session_id`），改为从 state 读副本 |
| 15 | 观察：step3 返回 `empty_result` 且用户选「继续推进」时，`flexible_state` 里没有 `field_selection_result` | **非缺陷**，属契约行为：回调对非定稿 status 不登记；后续 step4 的 `field_selection_result` 由协调器从 step3 返回内容构造并直传，不依赖 state |
| 16 | **运行期实测**：step5 把 `builtin_execute_command` 的**数组**参数 `args` 写成对象 `{"item": "..."}`（应为 `["..."]`）；子 agent 原始输出（`seq=25`）即如此，与落库无关 | 已修（**prompt 缺数组示例**）：`flexible_step5.toml` 的 `steps` 规则补「`params` 每项类型必须与工具真实输入一致、严禁改变容器类型」约束，示例补数组参数 `"options": ["--utf8", "--no-header"]` |
| 17 | **设计缺口**：`steps` 里来自 `input_schema` 的值被硬编码（`path`、命令串里的路径等），模板绑定录制时的具体值；执行器只能依赖 `dynamic_hints.param_mapping` 的文字描述让 LLM 替换，`steps` 就不是「零推理可跑」 | 已改（**输入注入点前移到 `steps`**）：规则改为「来自 `input_schema` 的值必须写成 `{{input.<参数名>}}`，**每一次出现都要替换**（含嵌在字符串内部）」；示例补 `'{{input.month}}'` / `orders_{{input.month}}.csv`；`param_mapping` 语义降级为「人类可读说明 + 兜底」 |

**验证**：`cargo check -p planned-agent-gui` 0 error；6 个 prompt 经 `FilePromptManager` 实测全部可加载、协调器模板 `{{ session_id }}` 渲染正常（校验用一次性脚本，跑完已删）。

**尚未做**：

- **运行时验收**：并发多会话归属、step3/step4 挂起-恢复后的回调归属，需启动应用手测。
- `max_tool_rounds` 触顶复现脚本（`chat/max_rounds_demo.toml`、`chat/sub_agent_max_rounds_driver.toml`）在还原后需临时改回才可复现。

其它记录：

- step1~step5 **全部**带 `host_session_id`（`hidden_args`，协调器按 schema 传参）——五步都挂了完成回调；step5 的回调推进档位并登记 `template_payload` 副本，落库仍由 `flexible_save_template` 负责（工具读 state 里的副本，不经协调器转抄）。
- **`flexible_step5.toml` 的两条编辑陷阱**（都在本轮踩到、且都被一次性校验测试当场抓住，否则会直接坏在运行期）：
  1. TOML 的 `"""` 多行字符串里 `\` **是转义符** —— 写 Windows 路径 `C:\data\in.txt` 会让 `\d` / `\i` 成为非法转义，整个文件 TOML 解析失败（prompt 加载不出来）。示例里请用 `C:/data/in.txt`。
  2. 该 prompt 经 **tera 渲染**，`{{...}}` / `{%...%}` **必须**包在 `{% raw %}...{% endraw %}` 内；否则渲染时会报 `Variable \`input.month\` not found in context` 直接失败。注意包裹范围：示例块外（如块上方的说明行）**同样要包**。
- 模板下拉的筛选是 `name.starts_with("chat/") || name.starts_with("flexible/")`，所以往 `prompts/` 下的 `chat` / `flexible` 目录放 `.md`/`.txt` 都会被 prompt-manager 注册并出现在下拉里。
