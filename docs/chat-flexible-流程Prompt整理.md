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
        (step1 定稿)   (step2 成功)  (step3 定稿)      (step4 定稿)       (step5 落库)
```

| 档位 | 该档位允许存在的产物（`products` 的不变量） |
|---|---|
| `task_defined` | `task_definition`、`output_format` |
| `executed` | `execution_trace`、`compressed_context` |
| `fields_selected` | `field_selection_result` |
| `params_confirmed` | `parameter_confirmation_result` |
| `templated` | 无（模板由 `flexible_save_template` 写入 `plans_flexible`，不入 `products`） |

规则：

- **读-改-写合并**：`save` 未传的 key 保留、传 `null` 的 key 清除、`current_step` 未传则保留原值。
- **只增不减**：`current_step` 仅在明确「重做 / 返回」上游时回退；回退时必须把下游产物一并在同一次 `save` 里传 `null` 清除。
- 失败不推进：任何 step 返回非定稿 status 时，不写产物、不动 `current_step`。

出处：`crates/agent-gui/src/pages/plan/flexible/tool/flexible_state.rs:127`、`crates/agent-gui/prompts/flexible/flexible_global_system.toml`（「产物一致性原则」一节）。

---

## 二、协调器（`flexible_global_system.toml`）

| 项 | 内容 |
|---|---|
| 模板变量 | `session_id`（必需），注入到「会话上下文」段 |
| 注册位置 | `crates/agent-gui/src/pages/plan/flexible/chat_service_factory.rs:51` |
| 代码白名单 | `flexible_step1~5`、`flexible_state`、`request_user_action` |
| 职责 | 纯状态机调度：路由到 step、读写 `flexible_state`、发 `request_user_action`、调 `flexible_save_template` 落库；**不碰业务工具、不做需求澄清** |
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

- **落 state**：**协调器**在用户确认后 `save(current_step="task_defined", products={task_definition, output_format})`。step1 **无回调**。
- **强约束**：`output_format` 必须由用户明确确认，禁止默认值 / 推断；缺基线且用户未指定时必须用单选 `request_user_action` 询问。

### 3.2 `flexible_step2` — 任务执行

- **职责**：按 `task_definition` 串行调用业务工具完成任务，逐步记录 `execution_trace`；高风险操作（删除 / 发送 / 提交 / 覆盖）先 `request_user_action` 确认。
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
  "execution_trace": [ { "tool": "工具名", "input": { "参数名": "参数值" }, "output_summary": "输出摘要" } ],
  "compressed_context": "≤50 字：是否成功 / 用了什么工具 / 得到什么结果 / 是否有高风险操作被拒" }

{ "status": "error",
  "error_message": "失败工具名 + 具体参数 + 失败原因",
  "execution_trace": [] }
```

- **落 state**：由 **`FlexibleStep2Callback`** 自动完成——读 `host_session_id` 定位会话，解析 `status == "success"` 后写入 `execution_trace` / `compressed_context`，`current_step = "executed"`，并**把 `field_selection_result`、`parameter_confirmation_result` 置 `null`**（重跑 step2 ⇒ step3/step4 定稿作废）。`status == "error"` 或非 JSON 时**不登记**。
  出处：`crates/agent-gui/src/pages/plan/flexible/step_callback/step2_callback.rs:61-89`。

### 3.3 `flexible_step3` — 字段选择

- **职责**：从 step2 执行结果里识别可用字段（结构化 / 非结构化 / 树形→点号路径），通过一次多选交互让用户勾选最终输出字段。**不得调用业务工具**。
- **允许工具**：`request_user_action`。
- **入参 schema**（`page.rs:276-293`）

| 参数 | 类型 | 必需 | 说明 |
|---|---|---|---|
| `execution_trace_summary` | string | ✅ | 来自 step2 的 `compressed_context` |
| `output_format` | string | | 来自 step1（**schema 未标必需**，但 prompt / 协调器都按必需处理，见 §六-4） |
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
- **落 state**：**协调器**手动 `save(current_step="fields_selected", products={field_selection_result})`。step3 **无回调**。

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
- **落 state**：**协调器**手动 `save(current_step="params_confirmed", products={parameter_confirmation_result})`。step4 **无回调**。

### 3.5 `flexible_step5` — 模板序列化

- **职责**：把 4 份上游产物编译成**混合模板**——`steps`（硬编码、零推理可跑）+ `execution_plan`（1:1 对应的智能修复说明书）。无用户交互。
- **允许工具**：`request_user_action`（实际不使用）；**无回调**，改由协调器调 `flexible_save_template` 落库。
- **入参 schema**（`page.rs:350-371`）

| 参数 | 类型 | 必需 | 说明 |
|---|---|---|---|
| `task_definition` | object | ✅ | 来自 step1 |
| `execution_trace` | array | ✅ | 来自 step2 |
| `field_selection_result` | **string** | ✅ | 注释写「纯文本输出」，但 step3 实际返回 **object**（见 §六-3） |
| `parameter_confirmation_result` | object | ✅ | 来自 step4 |

（**无** `host_session_id`——step5 无回调，不需要会话归属控制字段）

- **出参**

```json
{ "status": "success",
  "input_schema": { "month": { "type": "string", "required": true, "description": "月份，格式 YYYY-MM", "example": "2026-09" } },
  "output": { "format": "CSV", "fields": ["order_id", "amount"] },
  "steps": [ { "id": "step_1", "tool": "query_tool", "params": { "query": "...硬编码..." } } ],
  "execution_plan": [ { "step_id": "step_1",
                        "intent": "获取指定月份的订单数据",
                        "expected_schema": { "order_id": "string", "amount": "number" },
                        "dynamic_hints": { "param_mapping": "...", "field_adaptation": "...", "fallback_tool": "...", "stable": true } } ],
  "metadata": { "created_at": "2026-09-01T12:00:00Z", "version": 1 } }

{ "status": "error", "message": "缺少必要输入数据" }
```

- 约束：
  - 成功时顶层固定为 `status`、`input_schema`、`output`、`steps`、`execution_plan`、`metadata` 六项。
  - `steps` 与 `execution_plan` **等长且 `steps[i].id == execution_plan[i].step_id`**；`params` 为硬编码值，只有「上游输出被下游引用」时用 `{{step_1.output}}` 占位。
  - 字段必须来自输入，禁止臆造工具名 / 字段名 / 参数值（prompt 内的示例已标注为虚构）。
  - `input_schema` 为空时写成 `{}`。
- **落 state**：由 `flexible_save_template` 写入 `plans_flexible`（见 §四）；成功后协调器 `save(current_step="templated")`，**不写 `products`**。

---

## 四、两个旁路工具

### 4.1 `flexible_state`（协调器专用）

定义：`crates/agent-gui/src/pages/plan/flexible/tool/flexible_state.rs:127-177`

| 入参 | 必需 | 说明 |
|---|---|---|
| `session_id` | ✅ | 原样照抄 system prompt 中的本会话 ID |
| `action` | ✅ | `load` / `save` |
| `current_step` | (save) | 推进到的档位 |
| `products` | (save) | 要写入的产物对象；值传 `null` 表示清除该 key |

- **出参**：`load` → `{loaded, current_step, products}`（无记录时 `loaded=false`、`current_step="none"`、`products={}`）；`save` → `{saved, current_step, products}`。
- **合法 `products` key**：`task_definition`、`output_format`、`execution_trace`、`compressed_context`、`field_selection_result`、`parameter_confirmation_result`。

### 4.2 `flexible_save_template`

定义：`crates/agent-gui/src/pages/plan/flexible/tool/flexible_save_template.rs:133-161`

| 入参 | 必需 | 说明 |
|---|---|---|
| `session_id` | ✅ | 原样照抄 |
| `template` | ✅ | step5 返回的完整模板 JSON 对象（含五个顶层字段） |

- **出参**：成功 `{saved: true, id, plan_id, version}`；失败返回 error。
- **校验**（`flexible_save_template.rs:41-76`）：`template` 必须存在且为对象；`status == "error"` 直接拒绝；`steps` / `execution_plan` 必须存在、均为数组、长度相同且 `step_id` 一一对应。
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
        └─► 模板 6 字段 ──► flexible_save_template ──► plans_flexible ──► save(current_step="templated")
```

协调器传参要点（`flexible_global_system.toml`）：

- 含 `session_id` 的工具（`flexible_state`、`flexible_save_template`）必须原样照抄。
- 调 `flexible_step2/3/4` 时必须一并传 `host_session_id`（同样原样照抄）。
- step3 的 `execution_trace_summary` 取 step2 的 `compressed_context`；step4 的 `execution_trace` 取 step2 的完整轨迹。

---

## 六、发现的代码 ↔ prompt 不一致（待逐条确认）

| # | 位置 | 问题 | 影响 |
|---|---|---|---|
| 1 | `crates/agent-gui/src/pages/plan/flexible/chat_service_factory.rs:72-86` | 协调器 `allowed_tools` **没有 `flexible_save_template`**，但 `flexible_global_system.toml` 第 9 步要求调用它落库 | **P0**：模板无法落库（工具不在白名单，调用会被拒） |
| 2 | `crates/agent-gui/src/pages/plan/flexible/chat_service_factory.rs:87` | `max_tool_rounds: 2`、以及上一条 `builtin_read_documentation` 条目是**测试残留**（注释自己写明「测试完请还原」） | 协调器 2 轮即触顶，正常 step1~step5 调度跑不完 |
| 3 | `crates/agent-gui/src/pages/plan/flexible/page.rs:361-364` | step5 入参 `field_selection_result` 声明为 `string`（描述「纯文本输出」），而 step3 实际输出 **object**、step4 也声明 object；协调器 §6 / §8 文字同样写「纯文本输出」 | 类型契约自相矛盾，易导致传参形态不一致 |
| 4 | `crates/agent-gui/src/pages/plan/flexible/page.rs:283-292` | step3 的 `output_format` 未列入 `required`，但 prompt 与协调器都按必需传 | 契约松紧不一致（当前不致命，但易被误省略） |
| 5 | `crates/agent-gui/src/pages/plan/flexible/page.rs:246-249` | step2 的 `runtime_context` 已声明但协调器**从不传**；重试 / `back_to_execute` 也没回传 `compressed_context` | 死参数：声明与实现不一致 |
| 6 | `step_callback/step2_callback.rs:74-89` vs `flexible_global_system.toml`（任务执行阶段） | step2 产物由 callback 自动写 + 协调器又手动 `save`，两处逻辑重复（含清下游产物） | 双写；当前幂等无害，但职责重叠、易漂移 |
| 7 | `flexible_global_system.toml`（「状态」一节） | 文案写「整个流程分为四个阶段」，但实际列了 5 个 step、6 个状态档位 | 纯文档误差 |

补充说明（非缺陷，仅记录）：

- step2/3/4 带 `host_session_id`（`hidden_args`），step1/step5 不带——因为只有 step2 挂了回调（`create_step2_callback`，`page.rs:268`），step5 改由 `flexible_save_template` 落库（`page.rs:379`）。
- step3 / step4 目前**无回调**，状态登记完全靠协调器手动 `save`（与 `flexible_save_template 白名单` 同属待办方向：把 step3/4 登记也下沉为回调）。
- 模板下拉的筛选是 `name.starts_with("chat/") || name.starts_with("flexible/")`（`page.rs:409`），所以往 `prompts/` 下的 `chat` / `flexible` 目录放 `.md`/`.txt` 都会被 prompt-manager 注册并出现在下拉里。
