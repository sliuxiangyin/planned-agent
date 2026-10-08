# 协调器「看不见真实档位」：每轮注入 flexible_state（设计稿）

> 状态：**S1 + S2 已实施**（2026-10-08，见 §9 实施记录）；S2b 未做（已被 S2 覆盖）；Q5（既有会话 history 污染）未处理。
> 触发：2026-10-08 线上实测（`crates/agent-gui/logs/gui.log.2026-10-08`）。
> 相关：`flexible-coordinator-slimming.md`（入口判定与提示词精简）、`flexible-incremental-revise.md`（修订的落库授权）、`flexible-run-service.md`（执行期读模板）。

---

## 1. 现象（实测）

会话 `plan_id=f29e442b-…`、`session_id=fe5a6669-…`，`flexible_state` 实际内容：

```
current_step = "planned"
products     = { category:"Browser", steps:[#E1..#E5 未参数化], task_definition:{…} }
updated_at   = 2026-10-08T09:41:57.535Z
```

（注意：**没有** `inputs` / `output_schema`，`steps` 里是硬编码 URL 与账号密码 —— 这是「计划步刚定稿」的形态，见 §1.2。）

界面上用户连发两句，得到两个错误答复：

| 用户 | 协调器回复 | 事实 |
|---|---|---|
| 保存会话 | 当前会话已落库，无需再保存。 | 档位是 `planned`，没有"已落库" |
| 继续完成后续步骤 | 当前 `current_step` 已是 `saved`，没有后续步骤需要执行。 | 档位是 `planned`，后续步骤**存在且未做完** |

两句话都不在代码里（全仓 grep「无需再保存」「已落库」无命中，唯一一处是工具说明 `tool/flexible_state.rs:110` 的 `saved = flexible_save 已落库`）—— 它们是**协调器 LLM 自己组织的**。

### 1.1 铁证：那一轮一个工具都没调

`logs/gui.log.2026-10-08` 尾段（行 1987-1996）是当场 history 快照，其结尾三行：

```
[round]  [25] User: 保存会话
[round]  [26] Assistant: 当前会话已落库，无需再保存。
[round]  [27] User: 继续完成后续步骤
[round]  [28] Assistant: 当前 `current_step` 已是 `saved`，没有后续步骤需要执行。
[round] 无 tool_calls，break（本轮 LLM 未调用工具）
```

而整份 29 条 history 里，**`flexible_state` 只被调用过一次** —— 最早的 `[2]`，返回 `{"current_step":"none","loaded":false,"products":{}}`（那时是全新会话）。此后每一轮都再没回查过档位。

> 结论：`saved` 不是读出来的，是**编**出来的。

### 1.2 真实时间线（同一会话，从 history 快照还原）

| 轮次 | 谁写的 | 结果 |
|---|---|---|
| `[2]` | `flexible_state` | `current_step="none"`（唯一一次查询） |
| `[4]-[15]` | clarify → plan → parameterize → output → **save** | 档位 `saved`；模板（`plans_flexible_sessions.parameterized_task`）落库 |
| `[17]-[19]` | 用户「重新生成计划…只更新 category」→ `flexible_revise` | **`needs_restart`**：reason 写明「category 不在 task_definition / steps / inputs / output_schema 任一可见产物里」 |
| `[20]-[21]` | 协调器征求同意 → 用户「同意」 | — |
| `[22]-[23]` | `flexible_plan` 重跑 | 档位 **回退** `planned`；`CLEAR` 清掉 `inputs` / `output_schema`（`plan_callback.rs:38`）→ **就是 §1 里那份 products** |
| `[24]` | 协调器转述 | 「…**已保存的模板已同步更新为新版**」← **幻觉**：plan 步从不写模板列（`plan_callback.rs` 全无落库调用） |
| `[25]-[28]` | 用户追问，协调器连答 | 两句错话，**本轮未调用任何工具** |

`[24]` 那句错误结论一旦进了 history，就成了后续轮次的"既成事实"——`[26]`、`[28]` 都是顺着它往下推。**第一句编错，后面全错。**

---

## 2. 根因

### 2.1 直接原因：档位只能靠 LLM 主动查，本次它跳过了

prompt 要求「每条用户消息的第一件事」就是调 `flexible_state`（`prompts/flexible/flexible_global_system.toml:15`）。但这是**纪律**，不是**机制**：

- 协调器的 system prompt 在建会话时**渲染一次**就固化进 history 首条 —— `chat_service_factory.rs:46-55` 渲染、`:62` 以 `SystemPrompt::Rendered` 注入；`driver/prompt.rs:20-22` 遇到已有 system 直接 return。
- 每轮请求直接用 `state.history.snapshot()`（`driver/round/mod.rs:350`）组 `ChatCompletionRequest`（`:364-372`），**本轮没有任何可插桩的注入点**。
- 所以一旦模型跳过那次工具调用，**上下文里没有任何纠错信息**，它只能凭历史猜 —— 而历史里恰好有它自己编的 `saved`。

### 2.2 为什么 GUI 不能"渲染时就带上档位"

协调器的 system prompt 是**每次会话渲染一次**的：`chat_service_factory.rs:46-55`（`PromptContext::new().with_variable("session_id", …)` + `SystemPrompt::Rendered`）。若要带上每轮变化的档位，就得引入"每轮渲染"——而它此刻连 session_id 都要在 GUI 侧手工渲染，是因为 core 的 `Template` 分支用**空** `PromptContext` 渲染（`driver/prompt.rs:28-36`），带不了变量。**归根结底：core 的对话驱动没有 per-round 注入能力。**

### 2.3 诱发原因：入口表缺行 + 规格外措辞

入口判定表（`flexible_global_system.toml:18-24`）只有 5 行，**没有**「要求保存 / 继续完成后续步骤 / 恢复流程」这类"不含内容变更的推进"请求。这两句话落到表外，模型只能自由发挥；而它自认为"流程已完成"，自由发挥的结果就是"没什么可做的"。

---

## 3. 影响面

同一根因的三种显形（用户此前分别问过）：

1. **模板陈旧**：`plans_flexible_sessions.parameterized_task` 停在旧值（连 `category` 键都没有，那是 2026-10-08 提交 `c456a49` 才进 `build_payload`（`commit.rs:176-183`）的），执行器与左侧面板却照旧拿它当权威。
2. **说"已落库"**：档位其实已回退 `planned`。
3. **说"已是 saved"**：同一错误的第二次复述。

危害不止"说了句错话"：

- 错误结论留在 history 里，**持续污染该会话后续所有轮次**；
- 更坏的分支：误判入口 → 该跑的步不跑（本次），或反向误调 `flexible_clarify` 把 `steps` 清掉（`flexible_global_system.toml:30` 已专门为此写警告）。

---

## 4. 方案

### S1｜提示词层：补入口表 + 硬规则（止血，建议立即做）

改 `prompts/flexible/flexible_global_system.toml`：

1. **入口表补行**（按**通用意图**写，不写具体个案措辞）：已有 `steps` 且用户要求「落库 / 保存当前结果」「继续推进剩余步骤」「恢复/接着做」时，**先按本轮 `flexible_state` 的档位决定**该做什么 —— 落到 §6.1 的对应步骤，不得因"觉得已完成"而不做。
2. **硬规则**（新增一节）：
   - `current_step` 与产物状态**只能引自系统注入（或 `flexible_state` 的返回）**；**禁止**凭对话历史、凭自己上一轮的措辞陈述档位（明确写出：上一轮的结论可能已经过时）。
   - ~~每条用户消息的第一个动作必须是调用 `flexible_state`，即使你认为已经知道。~~ —— **已废弃**：S2 落地后不需要（状态由系统每轮注入，见 §9）。
   - 转述模板是否已更新时，**只转述 `summary` 里的措辞**，不得自行推断"已同步更新"。

> 纪律：本条规则只写机制与通用判据，**不得**把某次会话的具体措辞/业务格式写进通用 prompt。

**实证支撑**：协调器对规则的理解其实是**准确**的 —— 它能在回复里一字不差地复述 `:7` / `:132`（「协调器没有写状态的能力，状态由子 agent 完成回调自动登记」），却把档位说成 `saved`（实际 `planned`）。**失败的是事实层，不是规则层** —— 它手上没有事实，就现编一个，并用自己确实掌握的机制知识给这个假前提做背书（措辞技术性、逻辑自洽，最具误导性）。故 S1 这类"再加一条不许猜"的规则**边际收益有限**：它已经"知道"规则，缺的是事实。

局限：仍然是"纪律"，而本次事故正是纪律失效。所以 S1 单独不足以根治，但它便宜、当天可验证。

### S2｜机制层：宿主每轮注入档位（治本，推荐）

**核心思路**：把"必须查档位"从模型的自觉变成**上下文里的事实** —— 与本仓库早已确立的「系统直取、LLM 不转抄」同一原则（参考 `step_callback/before_inject.rs`，但它的注入目标是子 agent 的 `arguments`，`runner.rs:99-122` 触发；主协调器没有等价钩子）。

**设计要点**

| 项 | 取值 |
|---|---|
| 注入载体 | `ChatConfig` 新增可选字段 `per_round_context: Option<Arc<dyn PerRoundContext>>`（trait：`async fn context(&self) -> Option<String>`） |
| 注入时机 | 每轮组请求前，`driver/round/mod.rs:350` 的 `snapshot()` 之后 |
| 注入形式 | 作为**一条临时 system 消息**插入本轮请求的 messages，**不写 history**、不落库、不显示在 UI |
| 默认值 | `None` ⇒ 行为与现在完全一致（对 GUI 普通 chat、testkit 等使用方零影响） |

**GUI 侧实现**：在 `chat_service_factory.rs:57` 装配一个 provider，内部用 `PlansFlexibleService::load_state(plan_id, session_id)`（`services/plans_flexible_service.rs:92`）每轮现读，生成摘要。

**注入内容**（见 Q3）：

```
[本会话实时状态] current_step = planned（= 计划步已定稿，尚无参数化/输出定义产物）
已定稿产物：task_definition, steps, category
未定稿产物：inputs, output_schema
```

只给**档位 + 产物存在性**，不给产物全文 —— 协调器只判路由，正文由各 step 的 `INJECT_MAPPING` 自己注入（既有分工不改）。

**为什么不写 history**：不污染落库历史、不膨胀、每轮重新计算（档位可能在两轮之间被别的回调改掉）。

**备选形态（不推荐，记录原因）**

- *每轮重渲染 system 首条*：要改写已落库 history 的首条消息，副作用大且与 `push_front_system` 语义冲突（`driver/prompt.rs:20-22`）。
- *GUI 侧往 history 写一条消息*：污染历史与 UI 显示，且每条用户消息都带一条，越滚越多。
- *让 `flexible_state` "每轮自动执行一次"*：把通用工具特化成定时器，绕且难测。
- *强制模型每轮必须调用 `flexible_state`*（prompt 写"必须" / driver 没调就打回重试 / API `tool_choice`）：三者都不可取 ——
  prompt 已被本次实证否定（`:15` 本就写着"第一件事就是调用它"，模型照样跳过）；driver 打回重试要每轮多一次
  LLM round-trip（吃 `max_tool_rounds` 预算，见 `chat_service_factory.rs:75`），且需"重试 N 次就放行"的补丁
  （一放行又回到原点）；`ChatCompletionRequest`（`core/src/ai/types.rs:164-180`）**没有 `tool_choice` 字段**，
  强制调用得改 core + ai-openai 两层，且它只能表达"本次必须调某工具"，表达不了"每轮开始先调"。
  **更关键：强制调用会把污染从"偶尔一条"变成"每轮一条"**（tool 结果落库后全量重发），与本方案的目标相反。
  > 注：`flexible_state` **工具本身保留**（模型想查细节时仍可按需调用），与每轮注入不冲突 —— 注入提供的是"每轮必有的事实"，工具提供的是"按需的细节"。

**代价**：跨 crate 一处小改（`crates/planned-agent/src/chat/`）+ GUI 侧一个 provider + 测试。

#### 4.1 实现细化（落点已核实）

**(1) core｜`chat/service/config.rs`**：`ChatConfig` 加一个可选字段。trait 对象不支持 `derive(Debug)`，故包一层 newtype（`ChatConfig` 现在是 `#[derive(Debug, Clone)]`，见 `:21`）：

```rust
#[derive(Clone)]
pub struct PerRoundContext(Arc<dyn PerRoundContextSource>);
impl std::fmt::Debug for PerRoundContext { /* 常量字符串即可 */ }

#[async_trait]
pub trait PerRoundContextSource: Send + Sync {
    async fn render(&self) -> Option<String>;
}

// ChatConfig 内（Default 里给 None ⇒ 其它使用方零影响）：
pub per_round_context: Option<PerRoundContext>,
```

**(2) core｜`chat/driver/round/mod.rs:350`**：每轮组请求前插入，**不写 history**。

```rust
let mut messages = state.history.snapshot();
let ctx_source = state.config.lock().unwrap().per_round_context.clone();
if let Some(src) = ctx_source {
    if let Some(text) = src.render().await {
        messages.insert(1, system_message(text)); // 紧跟首条 system；落地时确认 Message 构造 helper
    }
}
```

（`state.config` 是 `Mutex<ChatConfig>`，`state/state.rs:46`；`ChatService` / driver 通过 `Arc<State<PM>>` 共享，`service/service.rs:36`。）

**(3) core｜`service/service.rs:272` 一带**：加 `set_per_round_context(...)`，与既有 `set_system_prompt` / `set_allowed_tools` 同款，便于构造后装配。

**(4) GUI｜新文件 `flexible/state_context.rs`**：实现 provider（持 `plan_id` + `Arc<PlansFlexibleService>`），`render()` 调 `load_state`（`services/plans_flexible_service.rs:92`）→ 纯函数 `render_state_summary(current_step, products)`。在 `chat_service_factory.rs:57` 装配。

**落点待确认的两处类型细节**：`history.snapshot()` 的返回类型、以及构造 system `Message` 的现成 helper。

**为什么不缓存**：同一次会话里档位会被 step 回调改掉（例如 plan 定稿把它打回 `planned`），缓存会立刻过期；本地 SQLite 读是微秒级，不值得冒这个险。

### S2b｜step 完成后把档位"回带"（可叠加；覆盖不到纯追问轮）

这是「入口 A / 入口 B / 单步调用完成后再读一次状态」的**机制化**版本：不让 LLM 自觉去读，
而是由**宿主在 step 回调里把写库后的真实档位附回给协调器**。

- **为什么不必改 core**：链的收场决策里有 `ResultDecision::Transform(new)`，它**直接改写对外结果**
  （`crates/planned-agent/src/chat/sub_agent/collect.rs:295`；`Accept` 则用 prelude 定稿的 `outer`，`:322`）。
  而各 step 的业务回调**恒为链末位**（`create_stepN_callback` 只挂一个业务回调 + prelude，见
  `plan/mod.rs:33`、`parameterize/mod.rs:35-38`），当前用 `Accept`（`commit.rs:95-101` 的 `hand_off`）—— 
  换成 `Transform` 即可。
- **数据现成**：`commit_state` 已返回写库后的 `(current_step, products)`（`commit.rs:57-90`），据此
  生成一行附加段即可。
- **落点**：`step_callback/commit.rs` 的 `hand_off`（+ 各 step 调用点），零 core 改动。
- **附带好处**：该文本作为 tool 结果进入 history，「最近一次真实档位」在后续轮次也看得见。
- ⚠️ **副作用**：tool 结果从「纯 JSON」变成「JSON + 附加段」。已确认安全的两处：prelude 的解析发生在
  写库**之前**（`prelude.rs:56`），协调器 prompt 也只看顶层 `status`（`flexible_global_system.toml:124`）；
  落地前**须再 grep 一遍**是否有对子 agent tool 结果做严格 `serde_json::from_str` 的消费者。

**边界（重要）**：它只在「本轮真的调用了 step」时生效；用户只发一句追问（本次事故的两轮正是如此）时
没有触发点 —— 所以 **S2b 不能替代 S2**，只能叠加。

### S3｜远期（可另立稿）：把档位判定权收回宿主

协调器只负责自然语言转述与征求同意，**入口由宿主按 `flexible_state` 决定**并直接调对应 step。这能彻底消灭"模型以为自己知道档位"这一类错误，但等于重构协调器（prompt 内分节路由 → 宿主状态机），成本最大，本文不展开。

---

## 5. 待拍板

| # | 问题 | 倾向 |
|---|---|---|
| Q1 | 只做 S1，还是 S1 + S2 都做？ | **都做**：S1 先上（当天可验），S2 随后 |
| Q2 | S2 的注入形态：临时 system 消息（不落库） vs 每轮重渲染 system 首条？ | 前者 |
| Q3 | 注入粒度：仅 `current_step` + 产物存在性 vs 附加产物摘要（steps 条数、inputs 名单）？ | 前者（+ 少量元信息）；细节留给 step 注入 |
| Q4 | 注入机制放 core（`ChatConfig` 新字段） vs GUI 侧旁路？ | core；旁路会污染 history |
| Q5 | 既有会话里那条错误结论（history 污染）怎么办？开新会话 / 提供"清历史" / 不管？ | 待用户定 |
| Q6 | 是否顺带把「保存 / 继续」的入口判定从「按表枚举」改成「按档位驱动」（S3 的最小切片）？ | 待用户定 |
| Q7 | 是否在 S1/S2 之外**叠加 S2b**（step 完成后回带档位）？ | 倾向做：便宜、让 tool 结果自带权威档位；但**不能替代 S2**（纯追问轮无触发点） |

---

## 6. 落地清单（若 Q1 拍"都做"）

**S1**
- `crates/agent-gui/prompts/flexible/flexible_global_system.toml`：§一 入口表补行 + 新增「档位只能引自本轮工具返回」的硬规则一节。

**S2**
- `crates/planned-agent/src/chat/service/config.rs`（`SystemPrompt` 定义在 `:9-18` 附近）：`ChatConfig` 新增 `per_round_context` 字段 + trait 定义。
- `crates/planned-agent/src/chat/driver/round/mod.rs:350` 附近：每轮插入临时 system 消息（`None` 时保持原样）。
- `crates/agent-gui/src/pages/plan/flexible/chat_service_factory.rs:57`：装配 `FlexibleStateContext{ plan_id, service }`。
- 新文件建议：`crates/agent-gui/src/pages/plan/flexible/state_context.rs`（provider 实现 + 纯函数 `render_state_summary(current_step, products) -> String`，纯函数便于单测）。

**S2b（可选，与 S2 叠加）**
- `crates/agent-gui/src/pages/plan/flexible/step_callback/commit.rs`：`hand_off` 改为接收写库后的
  `(current_step, products)` 并返回 `Transform(带档位的文本)`；六个 step 回调的调用点同步改
  （`clarify_callback.rs:75`、`plan_callback.rs:74`、`parameterize_callback.rs:70`、`output_callback.rs:70`、
  `save_callback.rs:113`、`revise_callback.rs:380` 一带，`hand_off(call)` 在各自文件末尾）。
- 纯函数：`render_state_tail(current_step, products) -> String`，单测锁文案。
- 前置检查：grep 是否存在对子 agent tool 结果做严格 JSON 解析的消费者。

**测试**
- 单测：`render_state_summary` 对 §1 那份 products 的输出（锁住档位文案与产物清单）。
- driver 侧：用假 `AiClient` 断言每轮请求的 messages 里含该临时 system 消息，且 **history 未被写入**。
- 回归：`cargo test -p planned-agent --lib`、`cargo test -p planned-agent-gui --bins`（注意 §5 已知 baseline 与 `--lib` 要求）。

---

## 7. 验证方式

1. **单元**：见 §6「测试」。
2. **端到端重放**（手动）：造一份「saved 的模板 + 档位被打回 planned」的会话，发「继续完成后续步骤」，断言协调器在**未调用任何工具**的情况下也**不会**说 `saved`，而是按 `planned` 继续（或至少如实转述"当前是 planned"）。
3. **日志断言**：每轮请求的首条/次条 system 里能看到 `current_step = …`（对照 `logs/gui.log.*` 的 `[round] [N] System:` 行）。
4. **反向验证 S1 是否必要**：只看 S2 生效后的行为，若仍出现"跳过工具调用 + 凭历史陈述"，说明 S1 的硬规则仍需保留。

---

## 8. 明确不做

- 不改 `flexible_state` 工具的对外契约（仍是只读、仍只收 `session_id`）。
- 不把 `products` 全文注入协调器上下文（避免把该由 step 注入的内容变成 LLM 二次转抄）。
- 不动 `plan_callback.rs` 的 `CLEAR` 语义（"上游重跑作废下游"是对的）——模板陈旧是 §3 的独立问题，另行处理。
- 不在本稿内处理"模板与 state 不一致的 UI 提示"（那是另一个缺口，见 §3 第 1 条）。

---

## 9. 实施记录（2026-10-08）

**已落地：S2（每轮注入）+ S1（提示词对齐）。**

| 落点 | 改动 |
|---|---|
| `crates/planned-agent/src/chat/service/config.rs` | 新增 `PerRoundContextSource`（async trait）、`PerRoundContext`（newtype + 手写 `Debug`）、`ChatConfig.per_round_context`（`Default = None`）+ 2 项单测 |
| `crates/planned-agent/src/chat/driver/round/mod.rs` | 每轮组请求前插入一条临时 system 消息（先 clone 来源再 `await`，锁不跨 `await`），不写 history；注入逻辑抽为 `inject_system_context` + 4 项单测 |
| `crates/planned-agent/src/chat/service/service.rs`、`service/mod.rs`、`chat/mod.rs` | `set_per_round_context` + 对外导出两个新类型 |
| `crates/agent-gui/src/pages/plan/flexible/state_context.rs` | 新增：`FlexibleStateContext`（现读 `flexible_state`）+ 纯函数 `render_state_summary` + `step_meaning` + 6 项单测 |
| `crates/agent-gui/src/pages/plan/flexible/chat_service_factory.rs` | 协调器装配 `per_round_context`（须在 `plan_id` / `session_id` 被 move 之前构造） |
| `crates/agent-gui/prompts/flexible/flexible_global_system.toml` | §一.1 改为「状态由系统注入」；入口表补「推进 / 收尾」行；新增「状态只认注入与工具返回」硬规则；§五.1 / §6.3 同步 |
| `crates/agent-gui/src/pages/plan/flexible/tool/flexible_state.rs` | 档位说明改由 `step_meaning` 拼装（单一来源）；「用途」改为按需查产物内容 |

**验证**：`cargo test -p planned-agent --lib` → 209 passed / 3 failed（3 项为 AGENTS.md §5 记录的既有 baseline）；`cargo test -p planned-agent-gui --bins` → 103 passed / 0 failed。

**未做 / 遗留**：

1. **S2b 未做** —— 被 S2 覆盖：S2 是「每轮」（每次 LLM 请求）render，step 调用之后的那一轮请求就会看到新档位，而 S2b 想补的正是这个场景。
2. **Q5（既有会话的 history 污染）未处理** —— 那条错误结论仍留在旧会话历史里，需用户决定开新会话 / 清历史 / 不管。
3. **未加「注入漏装」的硬约束** —— 当前装配点只有 `chat_service_factory` 一处，且是必填式写入（`Some(...)`），漏装在这里不可能发生，故暂不加断言/编译期约束；若日后新增协调器入口，需自行装配，可考虑在那时把 `per_round_context` 从 `Option` 改为必填参数。
