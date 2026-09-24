# flexible 流程新增「输出定义」步（`flexible_output`）

> 设计稿（待确认）。状态：**未实施**。
> 上游背景：flexible 流程现有四步 `flexible_clarify`（需求澄清）→ `flexible_plan`（计划）→ `flexible_parameterize`（参数化）→ `flexible_save`（保存），产物为 `{ task, inputs, steps }`，落库进 `plans_flexible_sessions.parameterized_task`。
> 相关：`docs/planned-agent/flexible-executor.md`（执行链路）。

---

## 1. 需求（用户原话整理）

1. **在 `crates/agent-gui/prompts/flexible/` 下新增一个「输出 step」**，并让 `parameterized_task` 增加「输出 schema」字段。
2. **运行到输出 step 时用 UI action 提示用户**：用「默认的执行成功结果」，还是「指定结果」（如生成的 csv、字符串（含 json 字符串）等）；**若指定结果经分析属未知，则允许为空**。
3. 若任务只是「计划执行完成 → 返回成功/失败」，或已有明确结果，**就在这个阶段把 schema 定义完**。
4. **后期**（等右侧 `PlansFlexibleService` 的执行做成持久化之后）：把执行结果与过程传入输出 step，在右侧 chat 里定义 schema。——**本期不做，只保证结构上留好口子。**

一句话：**把「结果契约」做成流程里的一个可由人拍板的一等步骤**，而不是让执行器事后猜。

---

## 2. 设计

### 2.1 流程位置与档位

五步，输出步插在「参数化」之后、「保存」之前（保存是落库动作，schema 必须在此之前定稿）：

```
需求澄清 → 计划 → 参数化 → 输出定义 → 保存
```

档位串新增一档：

```
none → task_defined → planned → parameterized → output_defined → saved
```

### 2.2 产物与落库形态

- **state 产物 key**：`output_schema`（进 `flexible_state.products`）。
- **模板 JSON**（落库的 `parameterized_task`）由 `{ task, inputs, steps }` 扩为：

```json
{
  "task": "...",
  "inputs": [...],
  "steps": [...],
  "output_schema": { ... }   // 可为 null
}
```

- 兼容性：`FlexiblePlanTemplate`（`crates/planned-agent/src/flexible/template.rs:11-20`）**没有 `deny_unknown_fields`**，加字段本身不会报错；但**新字段必须 `#[serde(default)]`**（`Option<T>` 也要显式 default），否则旧落库数据反序列化失败。全库唯一的反序列化点是 `template.rs:49-53` `from_json`，消费方是 `services/plans_flexible_service.rs:169-174` 与执行器 `flexible/executor.rs:67-69`。

### 2.3 `output_schema` 的形态（弱声明，允许未知）

承接「结果形态不能事前锁死」的结论，schema 只表达**意图与宽类型**，不锁字段集：

```json
{
  "kind": "success_only | text | json | csv | markdown | file",
  "description": "这次执行要交付什么（人话）",
  "detail": "可选的形态说明，如 `UTF-8 CSV，首行表头，列 title,price`",
  "required": ["title", "price"],
  "wanted": []
}
```

- `success_only`：任务只要「成功 / 失败」（需求 3 的第一种）。
- **「未知」= `null`**：整体 `null` 与 `{"kind":"unknown"}` 是同一件事，**只保留 `null` 一种表示**，避免两套写法。
- `required` 只放**能担保存在的**字段（缺了要报告）；`wanted` 是探索型「尽力去找」（找不到不算错）—— 对应「抓网页时不知道有没有 title」的场景：**没拿到 title 是合法结果，不是失败，更不许编**。
- 最低校验：非 null 时 `kind` 必须是上述枚举之一，`description` 非空。

### 2.4 交互（UI action）

输出步的子 agent 工具白名单含 `request_user_action`（范式同 `flexible_clarify`），提 **1 题**：

```json
{
  "questions": [{
    "header": "输出结果",
    "question": "这次任务执行完，要产出什么结果？",
    "options": [
      { "label": "只要成功/失败状态", "value": "success_only", "recommended": true },
      { "label": "指定结果形态",       "value": "specify" },
      { "label": "现在还定不了（留空）", "value": "unknown" }
    ],
    "allow_input": true
  }]
}
```

- `allow_input` 默认即 true（`crates/core/src/events/ui_action.rs:41-48`），前端 `chat_ui_actions_view` 已有**常驻行内输入框**（`component.rs:349-392`），「指定结果」这种自由文本输入**不需要新增任何 UI 组件**。
- 回传格式由 `build_choice`（`chat_ui_actions_view/component.rs:157-192`）决定：单选时**行内输入非空会覆盖所选 value**。于是子 agent 的判定规则天然成立：

| 回传值 | 判定 |
|---|---|
| `success_only` | 只关心成功/失败 |
| `unknown` | 定不了 → `output_schema = null` |
| **其它任意文本**（含选了 `specify` 再输入） | 当作「指定结果」的自由文本，交给 LLM 分析成结构化 schema |

- 挂起-恢复：子 agent 内 `request_user_action` → `EmitAndSuspend`（`chat/driver/mod.rs:104-112`）→ 前端 resume → 复用**首次挂起的 arguments** 重跑结果链（`chat/sub_agent/session.rs:24-25,78-79`）。因此**提示词必须写明**：「用户回答会出现在你的对话历史里；拿到回答后**不要再提问，直接输出最终 JSON**」。

### 2.5 定稿契约与状态清理

- 定稿 `status = "success"`（与参数化一致）；其它（`error` / 非 JSON）不登记任何产物。
- `NEXT_STEP = "output_defined"`，`PRODUCTS = ["output_schema"]`，`CLEAR = []`（其下游只有 save，save 不产 state 产物）。
- **上游定稿要作废它**：`clarify`（`clarify_callback.rs:38`）、`plan`（`plan_callback.rs:36`）、`parameterize`（`parameterize_callback.rs:34`）三处 `CLEAR` 各加 `"output_schema"` —— 需求/计划/参数变了，输出契约一并作废。

### 2.6 保存步的改动

`save_callback.rs` 的 `build_payload`（`:69-120`）加读 `output_schema` 并拼进 `json!`；`output_schema` 缺失或 `null` 一律落 `null`（**不报错** —— 用户可能跳过输出定义或选「定不了」），非 `null` 时必须是对象。

**不给 save 子 agent 加注入**：落库由回调直接从 `flexible_state.products` 读，save 子 agent 本身不需要该字段（它只校验三件套）—— 遵循「按需注入」原则（`flexible_global_system.toml` 的「其余业务字段都不要传」）。

### 2.7 与执行期结果的接口（需求 4，本期只留口子）

后期要把「执行结果与过程」喂给输出步，**现有机制已经够用、不需要改结构**：

- `step_callback/before_inject.rs` 的 `StateInjectCallback` 就是「按 `INJECT_MAPPING` 从会话 state 取字段 → 注入子 agent 参数」，映射可任意改名（测试见 `before_inject.rs:144-165`）。
- 所以后期只要：① 让执行侧把轨迹/结果写进会话 state（依赖 `PlansFlexibleService` 的执行持久化）；② 在 `output/mod.rs` 的 `INJECT_MAPPING` 里加一行 `("execution_trace", "execution_trace")` 之类即可，**提示词与回调都不用重构**。
- 本期输出步的注入只放它现在用得上的：`task_definition` + `steps` + `inputs`（供 LLM 判断「这次任务本来想交付什么」）。

---

## 3. 影响面清单（精确落点）

**新建**
- `crates/agent-gui/prompts/flexible/flexible_output.toml`
- `crates/agent-gui/src/pages/plan/flexible/step_callback/output/mod.rs`（含 `INJECT_MAPPING` + 单测）
- `crates/agent-gui/src/pages/plan/flexible/step_callback/output/output_callback.rs`（`AGENT`/`OK_STATUS`/`NEXT_STEP`/`PRODUCTS`/`CLEAR` + `#[async_trait]` 回调 + 单测）

**修改**
| 位置 | 改什么 |
|---|---|
| `prompts/flexible/flexible_global_system.toml` | `:9` 四步→五步；`:11-16` 步骤表；`:39-46` status→档位表；`:55-64` 直达依赖表；`:67` 档位串；`:104-108` 保存阶段小节前插入输出阶段 |
| `step_callback/mod.rs` `:66-75` | `pub(crate) mod output;` + `use output::{create_output_callback, create_output_inject};` |
| `page.rs` `:26-30` / 新块 / `:301-311` | imports；新增一处 `register_sub_agent`（范式 = `:208-248` parameterize 块，但 `allowed_tools: Some(vec!["request_user_action"])`）；`use_drop` 注销列表加 `"flexible_output"` |
| `chat_service_factory.rs` `:65-72` | 协调器白名单加 `"flexible_output"` |
| `tool/flexible_state.rs` `:53` `:99-108` | 工具描述里的档位序列 + `products` key 列表加 `output_schema` |
| `storage/entities/flexible_state.rs` `:5-6` `:23-25` | 档位串与 products 键注释 |
| `step_callback/clarify/clarify_callback.rs` `:38`、`plan/plan_callback.rs` `:36`、`parameterize/parameterize_callback.rs` `:34` | `CLEAR` 各加 `"output_schema"` |
| `step_callback/save/save_callback.rs` `:69-120` | `build_payload` 加 `output_schema`（缺省/null → null；非对象报错） |
| `step_callback/save/mod.rs` `INJECT_MAPPING` | **不改**：save 子 agent 不需要该字段，落库由回调直读 state |
| `page.rs` save 块输入 schema | **不改**（不注入，同上） |
| `crates/planned-agent/src/flexible/template.rs` `:11-20` | 加 `#[serde(default)] pub output_schema: Option<Value>` |
| `step_callback/mod.rs` `:107-128` | 断言「≥5 份 prompt」→「≥6 份」 |
| （可选）`left_panel/left_panel.rs` `:174-200` `:373-422` | 展示 `output_schema` 的 Bento 块 |

---

## 4. 测试

- `cargo test -p planned-agent-gui --bins`：新增 output 回调常量单测、`INJECT_MAPPING` 单测、`flexible_prompts_load_through_file_manager` 的份数断言。
- `cargo test -p planned-agent`：`template.rs` 加「旧 JSON（无 `output_schema`）仍可解析」与「`output_schema: null` 可解析」两个用例。

---

## 5. 已拍板（2026-09，用户确认）

1. **位置与档位名**：插在「参数化之后、保存之前」，档位名 **`output_defined`**。
2. **schema 粒度**：**完整版** —— `kind` + `description` + `detail` + `required` + `wanted`（一次把「能担保的字段」与「探索型字段」的语义分开）。
3. **默认与跳过**：UI action 默认推荐项 = `success_only`，且**允许整步跳过**。
   - 「跳过」= 协调器不调输出步、直接从 `parameterized` 调 `flexible_save`；此时 `output_schema = null`，`current_step` 停在 `parameterized`（**不引入 `output_defined` 档位**）。save 的前置检查只要求 `current_step` 至少 `parameterized`（`flexible_global_system.toml:61`），天然成立。
4. **命名**：工具名 **`flexible_output`** + state 产物/模板字段 **`output_schema`**（与 prompt-manager 的 TOML `[output_schema]` 段层级不同，不冲突）。

---

## 6. v2 修订（用户拍板，已实施）

### 6.1 `output_schema` 结构 v2

| 项 | v1 | v2 |
|---|---|---|
| `kind` | `success_only` / `text` / `markdown` / `json` / `csv` / `file` | **`bool`** / `text` / `markdown` / `json` / `csv` / `file` |
| 要什么 | `description` | **`goal`**（`bool` 之外必填） |
| 成功判据 | 挤在 `description` 里 | **`success`**（`bool` 必填，其余可选） |
| 形态细节 | `detail` | **`format`** |
| 字段清单 | `required` / `wanted`（无条件） | 同名字段，**仅 `json` / `csv` 生效** |

- 定义与校验的唯一处：`crates/planned-agent/src/flexible/output_schema.rs`（`OutputKind` + `OutputSchema::parse`），**保存 / 执行 / 展示三处共用**。
- **`goal` / `success` / `format` 里的参数值必须写 `${name}`**（与 `steps` 同一套规则）：`placeholder::collect_from_schema` 收集、`validate` 一并校验并**点名来源**（`steps` / `output_schema`）。在模板里写死具体路径 = 模板被一份参数值绑架。

### 6.2 执行期的「输出整理步」

模板步骤全部成功跑完后，执行器追加**一步**（`result_reference = #RESULT`）：

- 输入：交付步（= 最后一个有输出的模板步骤）的**完整输出** + 各步 200 字摘要（`output_summary` 的第一个生产消费点）+ 渲染后的契约文本；
- 提示词：`prompt::OUTPUT_RESOLVE_SYSTEM_PROMPT`（**不带工具**）；
- 产物：`PlanRunReport::result` → `RunSnapshot::result` → 左面板 RESULT 块；
- **不参与任务成败判定**：它失败只让结果为空，任务仍按模板步骤算成功；
- 契约缺失（`null`）：不追加整理步，结果退化为交付步输出原文；
- 契约非法：追加一条 `Failed` 的整理步（原因在它的 `error` 里），任务成败不变。

### 6.3 展示

左面板新增 **OUTPUT**（契约）与 **RESULT**（本次结果）两个 Bento 块；PIPELINE 每步多一个**默认收起**的产出折叠区（超长如实标注「已截断」）。数据通路与后续（落库 / 反哺）见 `docs/planned-agent/flexible-output-followups.md`。

