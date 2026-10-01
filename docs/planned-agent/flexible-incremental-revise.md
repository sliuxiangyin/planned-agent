# 灵活模式·增量修改（flexible_revise）

> 目标：把「用户改一句需求 → 整链重做 + 被追问一堆无关问题」变成
> 「只改本次实际涉及的那一个步骤对象，其余定稿产物逐字保留、不再追问」。

## 1. 问题：现状只有「整链重做」，没有「局部修改」

复现路径（两条都能稳定复现用户的两个抱怨）：

```
用户（已有计划 / 已保存）：把第 2 步改成按时间降序
  ↓ 协调器:78-91「非直达消息一律按需求澄清处理」
flexible_clarify  → clarify_callback.rs:38 定稿即清 steps / inputs / output_schema
  ↓
flexible_plan     → 整份 steps 重新生成（plan_callback.rs:36 再清 inputs / output_schema）
  ↓
flexible_parameterize
  ↓
flexible_output   → flexible_output.toml:64「每次执行你都先提问」→ 又弹输出格式问题
  ↓
flexible_save
```

两个抱怨的根因，各自定位得很清楚：

| 抱怨 | 根因 |
|---|---|
| 改需求时被问「输出格式」等无关问题 | `flexible_clarify` 定稿**无条件**清 `output_schema`（`clarify_callback.rs:38`），下游被作废就必然重问；且 `flexible_output.toml:64` 规定「每次执行都先提问」 |
| 只调一步，所有步骤都重写 | `flexible_plan` 的唯一输入是 `task`（`flexible_plan.toml:19`），**没有「上一版 steps」基线**，结构上不存在「只改一个 step 对象」的通路 |

一句话：现有五个 step 的语义都是「从上游产物**重新生成**」，没有一处是「在既有产物上打补丁」。

## 2. 目标 / 非目标

**目标**

- **A（不追问无关环节）**：已定稿的产物，默认**冻结** —— 用户没提就一个字都不动，也不为它再发起任何提问。
  尤其是 `output_schema`：用户没说「改输出 / 交付形态」，就**不调用 `flexible_output`、不问输出格式**。
- **B（只改一处）**：用户只调某一个步骤对象（四个字段
  `result_reference` / `intent` / `expected_output` / `dependencies`）时，
  **只有那一个对象被替换**；其余步骤对象的字段内容、顺序、`result_reference`、`dependencies` **物理不变**
  （不是「让 LLM 承诺保留」，而是「LLM 根本没有地方写它们」，见 §4.2）。
- **C（拿不准就回退，且先告知）**：判定「这次改动会牵连整体」时**不猜** —— 回退到既有的全量重做路径，
  并明确告诉用户「这次改动需要重做整份计划」。

**非目标（本稿不做）**

- 不改「重做/返回上游步骤」的语义：用户明确点名重做时，下游作废是对的，`clarify_callback.rs:38` /
  `plan_callback.rs:36` 的 `CLEAR` **保持原样**。
- 不做执行期的局部重跑调度（那是执行器的事，与本次诉求无关）。
- 不做会话多版本（改一版存一版）；已保存模板仍按「同一会话行覆盖」处理。

## 3. 核心设计：一次修订调用 = 影响面判定 + **结构化补丁**

关键判断：**判定影响面必然要一次 LLM 调用**（读「现状 + 用户这句话」），那就让这次调用的产物直接可用。
问题只剩「产物该长什么样」。两种形态对比：

| | ✗ 返回整份 `steps` 数组 | ✓ 只返回被调整的步骤对象（本稿采用） |
|---|---|---|
| LLM 顺手润色别的步骤 | 会，且必须靠事后的越界检测 + 还原兜 | **不可能**：它的输出 schema 里没有别的步骤对象可写 |
| 「其余逐字保留」的保证 | 靠纪律 + 校验纠偏 | 靠结构：系统只做「按 `result_reference` 定位 → 整块替换」，其它对象连碰都碰不到 |
| 删除步骤 | 能从数组里悄悄删掉 | 表达不出来 → 走 `needs_restart`（与既定「不允许增删」一致） |
| 实现 | 需要新旧 steps 逐对象比对 + 还原逻辑 | 只需一次定位替换（纯函数） |

于是流程是：

```
已有 current_step >= planned（含 saved）
  ↓ 用户消息 = 对现有任务的修改/调整/补充
flexible_state（只读，盘点现状）
  ↓
flexible_revise（唯一新增；输入=现状全量产物 + 本轮 user_message）
  ├─ status = revised         → 只带回被调整的步骤对象（可能 1..n 个）+ 需要同步的其它产物
  │                             系统定位替换 → 写回；档位不动；未涉及环节不再追问
  └─ status = needs_restart   → 协调器告知用户「这次改动牵连整体，需重做」，同意后走原 clarify 全量重做
```

落库侧继续复用现有设施：

- `build_patch`（`commit.rs:25-44`）：子 agent 输出里**没出现的 key → 跳过写入**，「没提就不动」由公共函数保证，
  不靠纪律。revise 的 `PRODUCTS = [task_definition, steps, inputs, output_schema]`、`CLEAR = []`。
- `commit_state(…, next_step = None, …)`：`None` = **保留原档位**（`plans_flexible_service.rs:148`），
  修订不推进 `current_step`。已有的 `Option<&str>` 参数直接可用，无需改公共设施。
- 唯一的**新增**：`steps` 这一项的 patch 值不是「LLM 直接给的数组」，而是
  「读旧 `steps` → 按 `result_reference` 把补丁对象整块替换进去」得到的新数组（§4.3，纯函数 + 单测）。

## 4. `flexible_revise` 契约

### 4.1 输入（系统注入，协调器只传两样）

| 来源 | 字段 | 说明 |
|---|---|---|
| 系统注入（`INJECT_MAPPING`） | `current_task_definition` / `current_steps` / `current_inputs` / `current_output_schema` | 现状全量产物，按 `host_session_id` 从 `flexible_state` 直取 |
| 协调器传入 | `user_message` | 本轮修改要求 |
| 协调器传入 | `host_session_id` | 回调定位会话 |

与 `flexible_clarify` 完全同构（只有 `user_message` 是「只能由协调器提供」的输入）。

**注入侧必须加 `current_` 前缀（不能同名注入）**：输出的 `steps` 是**补丁数组**，输入的 `steps` 是**现状全量**，
同名会让 LLM 把两者互抄。`INJECT_MAPPING` 本就支持改名（`(state 字段, 注入字段)`），照
`clarify` 把 `task_definition` 注入成 `previous_task_definition` 的既有做法即可：
`[("task_definition", "current_task_definition"), ("steps", "current_steps"), ("inputs", "current_inputs"), ("output_schema", "current_output_schema")]`。
（`flexible_revise.toml` 已按这组名字写定。）

### 4.2 输出契约

```json
{
  "status": "revised",
  "summary": "把 #E2 的排序改成按时间降序",
  "steps": [
    { "result_reference": "#E2", "intent": "…改后…", "expected_output": "…改后…", "dependencies": ["#E1"] }
  ]
}
```

- `status`：`revised`（定稿，登记补丁） / `needs_restart`（不登记任何产物，交协调器回退） / `error`（失败）。
- `summary`：一句话说明本次改了什么，协调器据此**转述给用户**（对外文本，不是原样回抄 JSON）。
- **`steps` 的语义 = 「待替换的步骤对象」数组，不是全量数组**（这是本稿与「整份数组」方案的唯一实质差别）：
  - 每个对象必须**四字段齐全**，且 `result_reference` **必须是旧 `steps` 里已存在的标识**
    （`#E2` 或用户说的「第 2 步」由子 agent 自行对应）；
  - 系统按 `result_reference` 在旧数组里定位并**整块替换**该对象 → 其它对象、顺序、条数物理不变；
  - 数组长度 1..n：只改一步就是长度 1；用户一次点名多处（「第 1 步和第 3 步都改」）就是多个；
  - **禁止**把没被要求调整的步骤也放进来（prompt 硬规则；见 §4.3 第 4 条的冗余处理）。
- 其它产物 key **可选，出现才写**：
  - `task_definition`：用户这次改的是任务描述本身时给出（**完整对象**，写全 `task`）；
  - `inputs`：**出现时必须是完整的参数表**（未改动的参数也要原样带上 —— 该产物是整块替换，不是逐项合并）。
    两种触发情形：① `steps` 里 `${name}` 集合变化；② 某个参数的 `default` / `description` 要改
    —— 后者因为 `steps` 里是占位符，**值变了步骤文字不变，所以通常只给 `inputs`、不给 `steps`**；
  - `output_schema`：**仅当用户明确要求变更交付形态时**才给出 —— 这是诉求 A 的硬规则（见 §5）。
- 失败：`{"status":"error","error_message":"…"}`。

### 4.3 定位替换 + 合法性校验（纯函数，放 `revise/` 目录内，配单测）

`apply_step_patches(old_steps, patches) -> Result<Vec<Step>, PatchReject>`：

1. `result_reference` 在旧数组里找不到 → 拒绝。语义上等于**新增步骤**，而「不允许增删」已拍板 → 整体转
   `needs_restart`（不是悄悄 append）。
2. 补丁对象四字段缺失 / 类型不对（`dependencies` 非字符串数组等）→ 拒绝 → 输出 `error`（如实上报，不猜）。
3. 同一个 `result_reference` 在补丁里出现两次 → 拒绝 → `error`。
4. 补丁对象与旧对象**深度相等**（等价复制、白写一遍）→ **静默忽略**（不替换、`warn!`）：冗余无害，
   不升级成流程中断。
5. 通过 → 返回替换后的新数组；其余对象引用自旧数组（逐字不变）。

**没有「越界还原」这一步** —— 结构上不存在越界。留给用户的可见性防线：`summary` 与
「本次改动：#E2」这类清单由协调器转述，若子 agent 多带了一个对象，用户能一眼看到并要求改回。

### 4.4 回调常量

| 常量 | 取值 | 说明 |
|---|---|---|
| `AGENT` | `flexible_revise` | 工具名 |
| `OK_STATUS` | `revised` | 定稿判定 |
| `PRODUCTS` | `[task_definition, steps, inputs, output_schema]` | 出现才写 |
| `CLEAR` | `[]` | **绝不无条件清下游** —— 这正是诉求 A 的落点 |
| `NEXT_STEP` | `None` | 档位不动（沿用当前 `current_step`） |

### 4.5 实现细节（写代码时别踩）

- **`build_patch` 的 warn 噪音**：它会对 `PRODUCTS` 里「定稿但未出现」的每个 key 都 `warn!`。
  而修订**只有 `steps` 是常态产物**（`inputs` / `output_schema` 多数时候本就不该出现），照搬会给每次修订刷三条误导性 warn。
  做法：`revise_callback.rs` 不复用通用 `build_patch` 的完整形态，改为本地组装补丁 ——
  `steps` 先经 §4.3 定位替换成新全量数组，其余三个 key「有就写、没有就跳过且**不 warn**」（它们是可选产物，缺失是正常态）。
- **`inputs` 的整块替换语义**：`merge_state` 是按 key 覆盖，所以子 agent 给的 `inputs` 必须是完整表（
  已在 `flexible_revise.toml` 写明）；本次没给 `inputs` 时**不得**写 `null`（`merge_state` 把 `null` 当删除 → 会清掉既有参数表）。
- **`steps` 的两段式**：① 从输出取补丁数组 → ② `apply_step_patches(current_steps, patches)` → ③ 把结果作为 `steps` 的补丁值写入。
  读旧 `steps` 与写新 `steps` 之间**必须同一次状态读取**（`load_state` 拿到的 `products` 直接用），否则并发修订会互相覆盖。

## 5. 冻结规则（写进提示词的「硬规则」）

1. **`output_schema` 默认冻结**：用户本轮没明确说「输出 / 交付形态 / 要什么结果」→ 修订**不得**输出
   `output_schema`，协调器**不得**调用 `flexible_output`、**不得**用 `request_user_action` 问输出格式。
2. **`steps` 里只放被要求调整的对象**：不得把其它步骤对象一起带回。
3. **不新增未定义占位符**：`steps` 里出现的 `${name}` 必须已存在于（本次给出的或旧的）`inputs` 中；
   需要新参数 → 同时输出 `inputs`。
4. **改动牵连整体就 `needs_restart`**：改动可能让其它步骤的 `intent` / `expected_output` 或整体交付形态失真时，
   输出 `needs_restart`，不要自行扩大改动面。
5. **只有指代不清才回问用户**：
   - 能唯一对应到某个 `result_reference` → **直接改**，不要为了确认再问一遍（不要制造新的骚扰）。
   - 候选 ≥2 且无法从上下文排除 → 用 `request_user_action` 问**一次**，并且必须**列出候选步骤对象**
     （形如 `#E2 "筛 ERROR 并按时间升序"` / `#E4 "把结果按时间排序"`，带上各自措辞，让用户能一眼认出）。
   - **「指代不清」≠「牵连整体」**：前者问用户，后者 `needs_restart`，两者不得混用。

## 6. 协调器路由改动（`flexible_global_system.toml`）

**落地形态：不新增文件、不新增 agent 层 —— 把这一份 prompt 重排成「一个入口判定 + 三个入口 + 一组通用规则」。**

理由：路由判断的输入（`flexible_state` 的返回 + 本轮 `user_message`）协调器本来就拿得到；它不产生产物、
不需要独立上下文，只有一个三分支输出。为它单开一层 agent 会带来三处硬代价：
① 协调器降为子 agent 后 `{{ session_id }}` 渲染不出来（核心 driver 的 `Template` 分支用空 `PromptContext`，
带变量的渲染只在 GUI 侧建主 ChatService 时发生），而它**必须**用这个值调 `flexible_state` 并传给每个 step；
② 挂起-恢复链会变成两层，而全仓唯一的挂起测试是单层（`tool_sub_agent_stream` 的
`sub_agent_awaiting_user_action_then_resume`）；
③ 中间的协调器是 LLM，`awaiting_user_action` 要经它之手冒泡，多一次透传可靠性风险。

重排后的结构（详见该 prompt 的「一、入口判定」）：

```
每条用户消息
 └─ 0. 先调 flexible_state（只读）盘 current_step + products
      ├─ 没有 steps（none / task_defined）        → 入口 A｜需求澄清 → 全流程
      └─ 已有 steps（planned 及以后，含 saved）
           ├ 点名「执行 / 重做 / 跳到」某一步     → 入口 B｜重做某一步 → 后续自动走完
           ├ 说「改 / 调整 / 补充 / 换成」某处    → 入口 C｜修订
           ├ 其它（闲聊 / 取消 / 提问）           → 入口 A（由 clarify 判 ignored / cancelled）
           └ 分不清 B 还是 C → 按 C 走（B 会清下游重跑，判错代价大）
```

**判据是「有没有 `steps`」而不是「用户说的是需求还是步骤」**：计划还没生成时，「改需求」本来就该在
澄清阶段做（走 B/C 会因缺前置而卡住）；计划已生成还走 clarify，就必然整链重做 —— 那正是要修的缺陷。
取消 / 闲聊不新增分支：它们在 `flexible_clarify` 里已完备（`cancelled` / `ignored`），且这两种 status
**不定稿、不写任何产物**，走 clarify 不会误清 `steps`。

入口 C 的路由（与本节最初的草案一致，仅补齐「首次 / 修订」的提问差异）：

- `revised`：产物已由系统登记、`current_step` 不变；转述 `summary` 给用户（**明确说出改了哪一处**）；
  **不进入计划 / 参数化 / 输出定义 / 保存任何一步**。若修订前 `current_step == saved` → 提示用户
  确认后再重存（Q2）。修订路径下 **`output_schema` 默认冻结**：不调 `flexible_output`、不问输出格式
  （Q7 拍板的「首次问、修订不问」就落在这里）。
- `needs_restart`：转述 `reason`、说明「这次改动需要重做整份计划」并**征求同意**；同意后走入口 A。
- `error`：转述 `error_message` 并停下。
- （revise 内部若因指代不清发了 `request_user_action`，那是子 agent 自己的一轮 awaiting → resume，
  协调器不参与，只等最终结果 —— 与 `clarify` / `output` 同构。）

「重做与状态」一节补一句：修订**不改 `current_step`**、也**不作废任何产物**。

**同时保留的既有行为**（重排只是搬家，不改语义）：入口 A 里澄清定稿后那次「补充/修改 or 确认继续」的
交互**保留**（Q6）—— 它是唯一必要的一次确认；`flexible_output` 的「必问」保留，但只在**首次创建**
时问，修订路径根本不会走到它。

### 6.1 参数修订的两条规则（追加）

| 用户想改 | 走哪条 | 为什么 |
|---|---|---|
| 已有参数的 `default` / `description` | **入口 C 修订**，只更新 `inputs` | `steps` 里是 `${name}` 占位符，值变了步骤文字不变 —— 改动面就是一张参数表 |
| **新增 / 删除参数**、改参数名、调整顺序 | `needs_restart` → 入口 A 全量 | 新增参数必然要往 `steps` 里插新的 `${name}`（否则是个死参数，`flexible_save` 的占位符校验也会拒），而「哪些值该参数化、同一原值共用哪个占位符」是参数化步的职责；revise 只盯着被点名的那一处，没有整份骨架的参数化视角 |

**系统侧兜底**：`validate_inputs_patch`（`revise_callback.rs`）校验新参数表的 `name` 序列与旧值**一字不差**
（含顺序），不一致即拒绝本次修订并如实上报。与 `apply_step_patches` 同一路子 —— 提示词是要求，回调是保证。

**`saved` 之后的模板同步（系统侧自动完成）**：左侧面板 `PARAMS`、以及执行时用的模板，读的都是**落库模板**
`plans_flexible_sessions.parameterized_task`（见 `left_panel/params.rs` 的注释），**不是** `flexible_state`
—— 所以 `saved` 状态下改了产物，必须**同步落库**才会生效。

这个同步放在 **`flexible_revise` 回调里自动做**（`revise_callback.rs` 的第 3 步）：`commit_state` 返回合并后的
`(current_step, products)`，若 `current_step == "saved"` 就调 `commit::persist_template`
（`build_payload` + `save_snapshot` + `TemplateNotifier::notify`）把模板整段刷新，并通知左侧面板重读。

**不靠协调器 LLM 再调一次 `flexible_save`** —— 早先的写法是「提示词里让协调器问『要不要重新保存』，用户答完
再调 `flexible_save`」，两个问题：① 依赖 LLM 跨轮记住「上一条消息其实是在确认重存」，记错就退化成整链重做；
② 用户答「暂不」或 LLM 漏调，系统就停在「`state` 有新值、模板是旧值」的不一致态，而且没人知道。

落库前 `build_payload` 照样把关（输出契约形态 + 占位符一致性）；**组装 / 校验失败时不写库**。另外占位符校验
被**提前**到写状态之前（`revise_callback` 里先跑 `placeholder::validate`）—— LLM 可能在改 `intent` 文本时顺手
插一个新 `${name}`，在写状态前挡住才不会留下残局。

**落库设施的归属**：`build_payload` / `persist_template` 放在 `step_callback/commit.rs`（**不**在 `save/` 里）——
`flexible_save` 用它首次落库、`flexible_revise` 用它同步模板，**两个 step 共用**（符合 `step_callback/mod.rs`
的分层规则）。

**一个已知的架构约束（为什么「新增参数」不能只重跑参数化）**：`flexible_parameterize` 的入参契约要求
`steps` 是**未占位**骨架，而 state 里的 `steps` 已被它自己覆盖成占位后版本（`parameterize/mod.rs` 注入
`("steps","steps")`）。所以新增参数这条路只能从 `clarify` 或 `plan` 起步（前者最彻底、后者能拿到未占位
`steps`），**不能**从 `parameterize` 起步 —— 那会踩进没有验证过的区域。

## 7. 改动清单

| # | 文件 | 改动 |
|---|---|---|
| 1 | `crates/agent-gui/prompts/flexible/flexible_revise.toml` | **新增**：角色 / 输入 / 输出契约（补丁数组语义）/ §5 硬规则 / 示例 |
| 2 | `.../flexible/step_callback/revise/mod.rs` | **新增**：组装点 + `INJECT_MAPPING`（四产物）+ 映射单测 |
| 3 | `.../flexible/step_callback/revise/revise_callback.rs` | **新增**：常量 + §4.3 定位替换纯函数 + 定稿登记 + 单测 |
| 4 | `.../flexible/step_callback/mod.rs` | 模块声明 + `pub(crate) use`；prompt 加载用例的注释里份数 6 → 7（断言 `>= 5` 可维持，建议收紧为 `>= 7` 以便新增 prompt 漏挂时立刻报错） |
| 5 | `.../flexible/page.rs` | `register_sub_agent("flexible_revise", …)`（含 `user_message` schema、`hidden_args`、**`allowed_tools = [request_user_action]`** —— 指代不清时回问用）+ `use_drop` 注销列表 |
| 6 | `.../flexible/chat_service_factory.rs` | 协调器 `allowed_tools` 白名单加 `flexible_revise` |
| 7 | `crates/agent-gui/prompts/flexible/flexible_global_system.toml` | §6 的路由段 |
| 8 | `docs/planned-agent/flexible-incremental-revise.md` | 本稿 |

**不动**：`clarify_callback.rs` / `plan_callback.rs` 的 `CLEAR`、`flexible_output.toml` 的「必问」、
`plans_flexible_service.rs`（无存储/迁移改动）。新增 step 的同步点与既有五个 step 完全一致
（回调 + 注册 + 白名单 + 注销 + 模块声明 + 份数断言）。

## 8. 测试计划

- **单测**：`INJECT_MAPPING` / 常量取值；§4.3 的五个分支（定位替换成功且其余对象逐字不变 /
  未知 `result_reference` 拒绝 / 字段残缺拒绝 / 重复 target 拒绝 / 深度相等静默忽略）。
- **现有基线**：`cargo test -p planned-agent-gui --bins`（提示词加载用例会覆盖新 prompt 可解析）；
  `cargo test -p planned-agent-gui --lib`。
- **手工端到端（场景清单）**：
  1. 已有 `planned`：说「把第 2 步改成按时间降序」→ 只有 #E2 变，其余逐字同旧，**无任何提问**。
  2. 已有 `parameterized`：说「改成 csv 输出」→ 只有 `output_schema` 变，`steps` / `inputs` 不动，
     不调 `flexible_output`。
  3. 已有 `saved`：说「第 3 步的目录换成 D:\out」→ 只有 #E3 变（涉及参数时连 `inputs`），提示重存。
  4. 已有 `planned`：说「改成先发邮件再归档」→ `needs_restart` → 告知需重做 → 同意后走原全量链。
  5. 已有 `planned`：说「加一步 / 删掉第 2 步」→ `needs_restart`（不允许增删）。
  6. 已有 `planned`：说「重新做一份完全不同的计划」→ 不被误判为修订，走 `flexible_clarify`。
  7. 已有 `planned`（两步都涉及排序）：说「把排序那步改一下」→ revise 用 `request_user_action` 列出候选 →
     用户答「#E2」→ 同轮继续改完；其余步骤逐字不变。

## 9. 风险与代价

- **指代歧义**：「改一下排序那步」可能对应不上唯一的 `result_reference` → 已定策略：revise 用
  `request_user_action` 列候选问一次（§5 硬规则 5），代价是给 revise 放行一个额外工具。
- **职责轻微重叠**：`flexible_revise` 要在「含 `${name}` 占位符的 steps」上做编辑，因此
  `flexible_revise.toml` 必须复述 `flexible_parameterize.toml` 的占位符纪律。这是为「不动现有五个 step」
  付的代价；缓解手段是 §5 硬规则 3。
- **多一次 LLM 调用 / 一次 `flexible_state` 读**：每条修改消息成本上升，但换掉的是整链四次调用。
- **修订后模板不自动生效**：`plans_flexible_sessions` 只在 `flexible_save` 时写，修订不碰模板行
  （左侧面板无需感知）。`saved` 场景按 Q2 提示用户重存。

## 10. 已拍板 / 待拍板

| # | 问题 | 结论 |
|---|---|---|
| Q1 | 落地形态 | **已定**：新增第六个 step `flexible_revise` |
| Q2 | `saved` 状态下修订完 | **已定（改）**：**系统侧自动同步** —— revise 回调在 `current_step == "saved"` 时直接把改动落库（`commit::persist_template`）并通知左侧面板。不再问用户、也不靠协调器再调一次 `flexible_save`（理由见 §6.1） |
| Q3 | 越界（改了 `changed` 之外的步骤） | **已消解**：改为「只返回被调整的步骤对象」后，结构上不可能越界，无需事后检测与还原 |
| Q4 | 修订允许增删步骤吗 | **已定**：不允许；增删一律 `needs_restart` 走全量重做 |
| Q5 | 用户指代不清（「改一下排序那步」对应不上唯一步骤）时 | **已定**：**由 revise 自己问** —— 放行 `request_user_action`。revise 手里有全量 `steps`，提问能带候选措辞；用户答完在同一轮 resume 继续；与 `clarify` / `output` 同构。协调器保持「只做路由」，不参与交互 |
| Q6 | 首次创建时，澄清定稿后的那次「补充/修改 or 确认继续」 | **已定**：**保留** —— 它是唯一必要的一次确认，问的是需求本身而不是无关问题 |
| Q7 | 输出定义步还要不要问用户 | **已定**：**首次创建问一次；修订路径不问**（修订不经过 `flexible_output`，`output_schema` 默认冻结） |
| Q8 | 「路由」这一层放哪里 | **已定**：**协调器 prompt 内分节**（不新增文件、不新增 agent 层）。另两档的代价见 §6 开头 |
| Q9 | 改**已有参数**的默认值 / 说明 | **已定**：属入口 C 修订，**只更新 `inputs`**（`steps` 里的 `${name}` 不变）；`current_step == saved` 时用**同轮** `request_user_action` 问是否重存 |
| Q10 | **新增 / 删除参数**、改参数名 | **已定**：走 `needs_restart` → 入口 A 全量（从 `clarify` 起，**不从 `parameterize` 重跑**，理由见 §6.1 末段）；系统侧由 `validate_inputs_patch` 兜底拒绝 |

## 11. 分期

**一期（已完成）** —— 落地清单：

| # | 文件 | 状态 |
|---|---|---|
| 1 | `prompts/flexible/flexible_revise.toml` | ✅ 新增（角色 / 输入 / 输出契约 / 硬规则） |
| 2 | `.../step_callback/revise/mod.rs` | ✅ 新增（组装点 + `INJECT_MAPPING` + 映射单测） |
| 3 | `.../step_callback/revise/revise_callback.rs` | ✅ 新增（`apply_step_patches` / `validate_inputs_patch` + 定稿登记 + `saved` 时同步落库 + 19 个单测） |
| 4 | `.../step_callback/commit.rs` | ✅ `build_payload` / `persist_template` 提到公共层（**save 与 revise 共用**）+ 6 个 payload 单测 |
| 5 | `.../step_callback/save/save_callback.rs` | ✅ 改用 `commit::persist_template`（组装与落库设施移出） |
| 6 | `.../step_callback/mod.rs` | ✅ 模块声明 + 重导出；prompt 份数断言收紧为 `>= 7` |
| 7 | `.../flexible/page.rs` | ✅ `register_sub_agent("flexible_revise", …)`（`allowed_tools = [request_user_action]`）+ 注销列表 + 传 `template_notifier()` |
| 8 | `.../flexible/chat_service_factory.rs` | ✅ 协调器白名单加 `flexible_revise` |
| 9 | `prompts/flexible/flexible_global_system.toml` | ✅ 重排为「入口判定 + 三入口 + 通用规则」 |

**验证**：`cargo test -p planned-agent-gui --bins` → **85 passed / 0 failed**
（含 19 个 revise 单测 + 6 个 `build_payload` 单测 + `flexible_prompts_load_through_file_manager`：7 份 prompt 均可被运行期加载器解析）。

**未做**：没有跑过真实 LLM 的端到端 —— §8 的 7 个手工场景仍需人工过一遍。

- **二期（按需）**：`steps` 占位符与 `inputs` 的一致性校验（在合并视图上做，违反时报 `error` 不落库）；
  修订历史（把每次 `summary` 记进会话供用户回看）。
