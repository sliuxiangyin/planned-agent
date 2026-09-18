# 灵活模式 · step 回调会话归属设计（让回调动态拿到会话标识）

> 状态：**阶段 1–6 已实施**（step1~step5 全量回调 + 状态登记收敛为「回调唯一写入」+ step5 模板副本消除协调器转抄），编译与单测通过；一次完整流程已端到端跑通（模板成功落库），并发多会话 / 挂起-恢复的运行时验收仍待做（见 §13、§15）
> 关联：`docs/chat-flexible-多会话保活设计.md`（§3 D 是本方案的前身；本方案是它的**延伸**，不推翻其「父 prompt 注入 + 参数传参」机制）
> 目标读者：`crates/agent-gui/src/pages/plan/flexible/` 与 `crates/planned-agent/src/chat/sub_agent/` 维护者
> 前置阅读：`docs/chat-flexible-多会话保活设计.md` §2（保活模型）、§3 D（会话隔离机制与「为何不用 task-local」）

## 1. 问题

把「step 子 agent 完成后的状态落库」从协调器 LLM 手里搬到**子 agent 完成回调**（`SubAgentResultCallback::on_result`）里做，前提是回调必须知道「这次调用属于哪个会话」。当前它拿不到：

- `on_result(&self, agent_name: &str, result: &ToolResult)`（`crates/planned-agent/src/chat/sub_agent/callback.rs:44`）只有 agent 名与结果文本，**没有会话身份**。
- 回调实例是 **plan 级共享单例**：注册只在壳里 `use_plan_agent_registrations` 跑一次（`crates/agent-gui/src/pages/plan/flexible/page.rs:105-354`），`create_step2_callback()` 每次调用只 `new` 一个 `Arc`（`step2_callback.rs:50-52`），`FlexibleStep2Callback` 是无字段 unit struct（`step2_callback.rs:18`）。同一 plan 下所有会话调用 `flexible_step2` 都命中同一个 runner / 同一个 callback。
- 而**多会话是并存保活的**：`FlexibleSessionHost` 切走不卸载（`docs/chat-flexible-多会话保活设计.md` §2、`flexible/session_host.rs`），后台会话的 driver 仍在跑。

因此「回调写状态」当前无解，必须先解决会话来源。

## 2. 结论（采用方案）

**沿用既有决策 D 的机制（父 prompt 注入 + 参数传参），并把「本次子 agent 调用的参数」从 runner 透传到回调。**

一句话链路：

```
GUI 注入父 system prompt（已有）
  → 父 LLM 照抄会话 id 进子 agent 调用参数
    → arguments 沿 runner → collect → callback 透传（本方案的唯一核心改动）
      → 回调（Rust 代码）据此定位会话并 save_state
```

要点：

1. ✅ **不读** `SessionManager` 全局 watch 槽（并发会串会话，见 §4）。
2. ✅ **不用** task-local（`docs/chat-flexible-多会话保活设计.md:106` 已否决；本方案不推翻）。
3. ✅ **不碰** `chat/driver/round/handlers.rs` 的父执行内核（那才是文档所说的「动执行内核」）。
4. ⚠️ **新字段必须另起名 `host_session_id`**，不能叫 `session_id`（该名已被「子 agent 挂起-恢复」占用，见 §5）。
5. ✅ `plan_id` **不需要动态**：它是 plan 级常量，注册时注入 callback 即可。

## 3. 现状与缺口（调用链）

```
父协调器 driver（该会话自己的 ChatService）
  └─ tool_call: flexible_step2(task_definition, <新增>host_session_id)
      └─ SubAgentToolExecutor::execute(arguments)              tool-manager/src/sub_agent/executor.rs:187   ✅ 有 arguments
          └─ SubAgentRunner::start(arguments, stream)          planned-agent/src/chat/sub_agent/runner.rs:63 ✅ 有
              └─ collect_until_outcome(...)                    runner.rs:123-131                        ❌ arguments 到此断掉
                  └─ cb.on_result(&stream.tool_name(), &probe) collect.rs:97                            ❌ 只剩 agent_name
```

| 环节 | 是否已有会话身份 | 证据 |
|---|---|---|
| 父 ChatService | 有（`ChatMessageStore { plan_id, session_id }`） | `flexible/chat_flexible_message_storage.rs:15-29`、`chat_service_factory.rs:44` |
| 父 system prompt | 有（`{{ session_id }}` 渲染注入） | `chat_service_factory.rs:46-55` |
| 子 agent 调用参数 `arguments` | 可携带（字段需新增） | `runner.rs:63`、`executor.rs:187` |
| `collect_until_outcome` → `on_result` | **无** | `collect.rs:28-35`、`collect.rs:97` |

**唯一缺口**就是 `runner.rs:123` → `collect.rs:97`：参数没有继续往下传。

## 4. 候选方案取舍

| 候选 | 结论 | 原因 |
|---|---|---|
| 读 `SessionManager` 全局 watch 槽 | ❌ | 槽是 **per-plan 单值**（`shared/session.rs:25-33`）。会话 A 的 step2 在后台跑、用户切到 B 后槽值=B，A 的回调会写 B 的库 —— 正是保活设计 §3 D 记录过的「串会话」问题 |
| per-session 注册 callback 实例 | ❌ | 与决策 B（注册上移到壳、一次性、去重）冲突：同名覆盖会让会话互相踩（保活设计 §3 B、§6） |
| task-local 沿 async 链传播 | ❌ | 已被否决（保活设计 `:106`，理由是「为传一个 id 动执行内核，成本大于收益」） |
| **参数传参 + 调用链透传（本方案）** | ✅ | 通道现成（`arguments` 已在 `start()`），改动收敛在子 agent 链路，天然 per-call 正确 |

## 5. 字段命名决策（本方案最易踩的坑）

仓库里已有**两种**叫 `session_id` 的东西，加上本方案要引入的会话 id，共**三种**语义：

| 名字 | 语义 | 位置 | 可复用？ |
|---|---|---|---|
| `host_session_id`（**本方案新增**） | **宿主会话**（当前宿主 = GUI，即用户创建的会话）= 落库归属（`plans_flexible_sessions` / `flexible_state` 的主键成分） | 本方案引入 | — |
| `session_id`（子 agent schema 字段） | **子 agent 挂起-恢复**的子会话 ID | 自动注入：`tool-manager/src/core/registry.rs:311-330`；读写：`sub_agent/executor.rs:191-205`、`registry.rs:754` | ❌ **禁用** |
| `run_id` | 挂起-恢复的路由（= 父调用的 tool_call_id） | `planned-agent/src/chat/service/config.rs:64-73` | ❌ 不混用 |

**为什么 `session_id` 绝对不能用**（`executor.rs:191-208`）：

```rust
let resume_sid = arguments.get("session_id").and_then(|v| v.as_str());
match &resume_sid {
    Some(sid) => { let mut session = self.store.take(sid)?; ... }  // 有值 → 走 resume
    None => { self.runner.start(arguments, ...).await }            // 无值 → 新跑一次
}
```

若父 LLM 把 GUI 会话 id 填进 `session_id`，那么 step3 / step4（**它们必然挂起**：要问用户选字段、选参数）恢复时，父 agent 回传的值会被当成挂起会话 ID → `store.take(sid)` 取不到 → **恢复直接坏掉**。同名双语义必然冲突。

> 补充：`flexible_state` / `flexible_save_template` 现在能安全使用 `session_id` 作为自定义工具参数，是因为它们是 **custom tool**（走 `ToolExecutor`），**不经过** `inject_session_fields`（该注入只对 `register_sub_agent` 生效）。子 agent 侧没有这个豁免。
>
> **命名取 `host_session_id`（宿主会话 id）而非 `flexible_*` 前缀名、也不用 `chat_session_id`**：
> - **不绑实现**：这个字段的属主是**宿主应用**（当前是 GUI，将来可能是 CLI / 服务端），与 flexible 流程无关。用 `flexible_` 前缀等于把宿主身份绑死在具体流程上，其它宿主复用时被迫改名。
> - **不歧义**：`chat_session_id` 易被读成「子 agent 自己的 chat 会话」（子 agent 也是 `ChatService`，也有 `ChatSubAgentSession`）；`host_` 前缀明确表示「宿主才拥有、核心库不认识的那个会话」。
> - **前缀划分清晰**：`flexible_*` 保留给流程工具（`flexible_state` / `flexible_save_template` / `flexible_step*`），宿主身份不属于其中。
>
> **相关（可选统一，未定）**：`flexible_state` / `flexible_save_template` 的参数目前仍叫 `session_id`（它们是 custom tool，不走 `inject_session_fields`，不撞名，故可用）。若将来希望「全仓只有一个名字指代宿主会话」，需一并改这两个工具的 schema + prompt + executor 到 `host_session_id`。

## 6. 目标数据流

```
新建会话（GUI）
  ├─ new_chat_service(plan_id, session_id)                     chat_service_factory.rs:35
  │   ├─ ChatMessageStore::new(plan_id, session_id, repo)      :44
  │   └─ render flexible_global_system + with_variable(session_id)  :50-55
  │       → ChatConfig.system_prompt = Rendered(...)           :62
  └─ use_plan_agent_registrations(plan_id)                     page.rs:105
      └─ create_step2_callback(plan_id, plans_flexible_service)  page.rs:253（新增入参）

会话运行期
  父 LLM 调用 flexible_step2({ task_definition, host_session_id: "<照抄>" })
    └─ runner.start(arguments)                                 runner.rs:63
        └─ collect_until_outcome(..., arguments)               collect.rs:28（新增入参）
            └─ SubAgentCallContext { agent_name, tool_call_id, arguments }
                └─ FlexibleStep2Callback::on_result(ctx, result)   step2_callback.rs:22
                    ├─ let sid = ctx.arguments["host_session_id"]
                    └─ service.load_state(plan_id, sid) / save_state(...)
```

## 7. 改动清单

### 7.1 核心库 `crates/planned-agent`（4 处，均为「透传」性质）

**① `chat/sub_agent/callback.rs`** —— 新增调用上下文，扩签名

```rust
/// 本次子 agent 调用的上下文（**通用**，不掺入 GUI 领域概念）。
#[derive(Clone)]
pub struct SubAgentCallContext {
    pub agent_name: String,   // 子 agent 工具名
    pub tool_call_id: String, // = invocation_id / run_id
    pub arguments: Value,     // 父 LLM 传入的原始参数（GUI 从中取 host_session_id）
}

pub trait SubAgentResultCallback: Send + Sync {
    async fn on_result(&self, ctx: &SubAgentCallContext, result: &ToolResult) -> ResultDecision;
}
```

影响面已核实**极小**：实现只有 `agent-gui/.../step2_callback.rs:21` 一处；调用点只有 `collect.rs:97` 一处。

**② `chat/sub_agent/collect.rs`** —— 增参 + 构造 ctx

- `collect_until_outcome(..., arguments: Value)`；在 `collect.rs:97` 处用 `stream.tool_name()` / `stream.invocation_id()` / `arguments` 拼 `SubAgentCallContext`。
- `Suspended` 分支构造 `ChatSubAgentSession`（`collect.rs:143-152`）时把 `arguments` 一并存入（见 §8）。

**③ `chat/sub_agent/session.rs`** —— `ChatSubAgentSession` 增字段 `arguments: Value`，`resume()`（`session.rs:71-78`）回传给 `collect_until_outcome`。

**④ `chat/sub_agent/runner.rs`** —— `start()`（`runner.rs:63`）把 `arguments` 传下去；并在 `runner.rs:87` 拼 task 文本前**剔除控制字段**（`host_session_id`），避免父会话 id 泄漏进子 agent 的上下文。

### 7.2 GUI `crates/agent-gui/src/pages/plan/flexible`

**⑤ `step2_callback.rs`** —— 从 unit struct 变带状态

```rust
pub struct FlexibleStep2Callback {
    plan_id: String,
    service: Arc<PlansFlexibleService>, // 与 FlexibleStateExecutor 同源
}

pub fn create_step2_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> Option<Arc<dyn SubAgentResultCallback>>;
```

`on_result` 内：`ctx.arguments["host_session_id"]` → `service.load_state()` → 判定定稿 → `service.save_state()`。**callback 从此能自己读写状态**（这是后续「回调下沉保存」的落点）。

**⑥ `page.rs:253`** —— 构造点改为 `create_step2_callback(plan_id.clone(), plans_flexible_service.clone())`（`plans_flexible_service` 在 `page.rs:114-117` 已存在，直接 clone）。

**⑦ step2 / step3 / step4 的 `input_schema`** 各加 `host_session_id`（`required`）：

```json
"host_session_id": {
  "type": "string",
  "description": "本会话 ID，原样照抄 system prompt「会话上下文」中给出的值，不得改写"
}
```

（step1 / step5 现已一并纳入回调：两者都补了 `host_session_id` + `hidden_args`，见 §13 实施记录。）

**⑧ 协调器 prompt**（`prompts/flexible/flexible_global_system.toml`）说明：调用 `flexible_step2` / `flexible_step3` / `flexible_step4` 时必须带上 `host_session_id`，值照抄「会话上下文」段。

> ⚠️ **不要**在 step 子 agent 的 prompt 里提 `host_session_id`：它已列入 `hidden_args`，**不会**出现在子 agent 收到的 task 文本里（子 agent 也不需要它）。传参义务只在父协调器侧。

### 7.3 配套契约收紧（回调要判定定稿，必须先修）

回调需要在**代码**里判定「本次是定稿还是非定稿」。现状：

| step | 定稿信号 | 非定稿信号 | 机器可判定？ |
|---|---|---|---|
| step2 | JSON `status:"success"` | JSON `status:"error"` | ✅ 达标（`flexible_step2.toml:52-70`） |
| step3 | 首行 `# 输出确认` | `back_to_execute` / `empty_result` / `cancelled` | ⚠️ `flexible_step3.toml:100-104` 只说「返回 X 状态」，**无精确首行模板** |
| step4 | 首行 `# 参数确认` | `back_to_step3：...` / `cancelled：...` | ✅ 接近达标（`flexible_step4.toml:65-74`） |

→ ~~必须先收紧 step3 的非定稿输出为严格首行标记~~ **（已过时）**：step1~step5 现已统一为「纯 JSON + 顶层 `status`」契约，回调直接用 `status` 判定定稿（见 §13 实施记录），不再依赖首行文本标记。（判定原则不变：宁可「不写」也不错写。）

## 8. 为什么 resume 路径必须一起改

若不把 `arguments` 存进 `ChatSubAgentSession`：step3 / step4 在用户交互时挂起 → 用户作答 → `resume()`（`session.rs:47-80`）重入 `collect_until_outcome` → 回调触发时**没有 arguments** → 拿不到 `host_session_id` → 静默不写状态。

而 step3 / step4 **几乎必然挂起**（都要问用户选字段 / 选参数），所以这不是边角情况，是**主路径**。

## 9. 风险与缓解

| 风险 | 评估 / 缓解 |
|---|---|
| `host_session_id` 仍由父 LLM 照抄，可能抄错 | 与既有决策 D **同等风险**（工具侧现状已如此），不因本方案变差。缓解：schema `required` + 描述强约束 + 回调/executor 校验非空，非法即回可读错误让父 agent 重试 |
| 想彻底摆脱 LLM 中转 | 只有一条路：让父执行内核把「父 State 的会话身份」显式传给子调用（改 `chat/driver/round/handlers.rs`）。这正是保活设计 §3 D 否决过的「动执行内核」，**成本更高**，列为 S2 备选，暂不做 |
| 回调内 `save_state` 失败 | 回调是 async 且被 await（`callback.rs:28-32`），需定策略：有限重试 → 仍失败则用 `Transform` 在 content 尾部追加机器可读告警（如 `state_saved:false`），协调器 prompt 据此**阻断后续步骤** |
| `on_result` 签名变更的影响面 | 已核实：1 个实现 + 1 个调用点（§7.1 ①） |
| 父会话 id 泄漏进子 agent 上下文 | `runner.rs:87` 拼 task 前剔除 `host_session_id`（§7.1 ④） |

## 10. 验收

1. **并发归属**：会话 A 跑 step2 时切到 B，并在 B 触发 step2 → 断言两次 `save_state` 各写自己的 `(plan_id, session_id)` 行。这是保活设计文档阶段 4 遗留的「并发多会话下归属正确」待验证项。
2. **挂起-恢复归属**：step3 挂起 → 用户作答 → resume 后回调仍能拿到**正确**的 `host_session_id`。
3. `cargo check -p planned-agent-gui -p planned-agent -p planned-agent-tool-manager` 全绿。

## 11. 实施顺序与 TODO 勾选

> 每完成一项勾选；阶段末做一次 `cargo check`。

### 阶段 1 · 核心库透传（不改变现有行为）

- [x] `callback.rs`：新增 `SubAgentCallContext` + 扩 `on_result` 签名
- [x] `collect.rs`：`collect_until_outcome` 增参 + 构造 ctx（含 `Suspended` 分支存 arguments）
- [x] `session.rs`：`ChatSubAgentSession` 增 `arguments` 字段并在 `resume()` 回传
- [x] `runner.rs`：把 arguments 传下去 + 按新增的 `ChatConfig.hidden_args` 过滤 task 文本（默认空，行为不变）
- [x] 回调侧读 `host_session_id` 并记日志（读状态验证归属，见 `step2_callback.rs`）
- [x] `cargo check -p planned-agent`

### 阶段 2 · GUI 接线

- [x] `step2_callback.rs`：改带状态（`plan_id` + `service`），从 `ctx.arguments` 取会话 id
- [x] `page.rs:253`：`create_step2_callback(plan_id, plans_flexible_service)`
- [x] step2 / step3 / step4 `input_schema` 加 `host_session_id`（required）
- [x] 协调器 prompt（`flexible_global_system.toml`）加 `host_session_id` 传参说明（**不在** step 子 agent prompt 里提）
- [x] `cargo check -p planned-agent-gui`

### 阶段 3 · 契约收紧与回调下沉

- [x] `flexible_step3.toml`：非定稿输出改严格首行标记
- [x] step2 回调内判定定稿 → `PlansFlexibleService::merge_state` 登记 `executed`（含清下游）
- [x] 扩到 step1 / step3 / step4 / step5（step1 / step5 一并纳入；默认走通用实现）
- [ ] 跑 §10 验收 1、2

### 收尾

- [x] `cargo check` 三 crate 全绿（`planned-agent` / `planned-agent-gui` / `planned-agent-tool-manager`）
- [x] 已更新 `docs/chat-flexible-多会话保活设计.md` §3 D，加指向本文件的回链

## 12. 相关发现（不在本方案范围，但同属灵活模式流程）

评审期间发现的两处**现存代码与文档不一致**，记录备查：

1. **`flexible_save_template` 未进协调器白名单**（疑似回归）。
   - `page.rs:128-137` 已把该工具注册进 registry；
   - 但协调器的 `allowed_tools`（`chat_service_factory.rs:72-86`）只列了 `flexible_step1..5` + `flexible_state` + `request_user_action`，**没有 `flexible_save_template`**；
   - 而 `allowed_tools = Some(tokens)` 的语义是「只放行 tokens 命中的工具」（`planned-agent/src/chat/tools/mod.rs:24-28`，测试 `none_named_or_unknown_tokens_yield_nothing` 明确未列出即不放行）；
   - 结论：**协调器看不到也调不到 `flexible_save_template`**，而 `flexible_global_system.toml` 第 9 步要求调用它完成落库 → 灵活模式创建的最后一步会失败。
   - 与保活设计文档 TODO 阶段 4「[x] 注册 `flexible_save_template` 到 registry 并加入协调器 `allowed_tools`」矛盾 → 疑似后续改动误删。

2. **`chat_service_factory.rs` 有测试用临时残留**（文件内注释已自认）：
   - `max_tool_rounds: 2`（`:87`）——正常协调器调度 step1~5 需多轮，默认应为 10；
   - `builtin_read_documentation` 相关条目（`:66-71` 注释说明为触顶复现测试用）。

> 这两项建议单独修，不与本方案混提。

## 13. 实施记录

阶段 1–3（step2 路径）已落地，三 crate 编译通过（`planned-agent` / `planned-agent-gui` / `planned-agent-tool-manager`）：

- **核心库**：新增 `SubAgentCallContext`（`agent_name` / `tool_call_id` / `arguments`）并扩 `SubAgentResultCallback::on_result` 签名；`collect_until_outcome` 增 `arguments` 形参，`Suspended` 分支随 `ChatSubAgentSession` 保存、`resume()` 回传；`SubAgentRunner::start()` 透传。
- **控制字段隔离**：未在核心库硬编码字段名，而是新增 `ChatConfig.hidden_args: Vec<String>`（默认空 ⇒ 行为不变），由 GUI 在 step2/3/4 配置里声明 `host_session_id`，`runner` 据此在拼接 task 文本前剔除。
- **服务层去重**：新增 `PlansFlexibleService::merge_state`（读-改-写合并，值为 `null` ⇒ 删除），`flexible_state` 工具的 `save` 改为复用它。
- **回调**：`FlexibleStep2Callback` 携带 `plan_id` + `service`，按契约判定定稿后登记 `executed`（写入 `execution_trace`/`compressed_context`，并清 `field_selection_result`/`parameter_confirmation_result`）；`error` / 非 JSON 不推进。**回调统一放在 `pages/plan/flexible/step_callback/` 目录**（`mod.rs` 存共享的 `HOST_SESSION_ID_FIELD` / `read_host_session_id`，各 step 回调一个子模块），便于 step3/step4 后续扩展。
- **契约收紧**：`flexible_step3.toml` 非定稿输出改严格首行标记。

阶段 4 · 全量回调（step1 / step3 / step4 / step5 补齐）已落地，`cargo check -p planned-agent-gui` 通过：

- **通用实现**：新增 `step_callback/step_commit.rs`（`StepSpec` + `StepCallback`），把「读会话归属 → 判定定稿 → 写产物 → 推进 `current_step` → 清下游产物」收敛成一份代码；五个 step 各自只给一份 `StepSpec`，回调实现细节不再重复。
- **step1**：`status:"task_defined"` ⇒ 写 `task_definition` + `output_format`，清 `execution_trace` / `compressed_context` / `field_selection_result` / `parameter_confirmation_result`，推进 `task_defined`。
- **step2**：行为不变（改写为复用通用实现）。
- **step3**：`status:"fields_selected"` ⇒ 写 `field_selection_result`，清 `parameter_confirmation_result`，推进 `fields_selected`。
- **step4**：`status:"params_confirmed"` ⇒ 写 `parameter_confirmation_result`，推进 `params_confirmed`。
- **step5**：`status:"success"` ⇒ 推进 `current_step="templated"`，并把**整段输出**登记为 `products.template_payload`（模板副本，阶段 6 补）；**不做落库**（`plans_flexible_sessions` 的写入与结构校验仍由 `flexible_save_template` 工具负责，见 §15）。
- **接线**：`page.rs` 四个 step 的 `register_sub_agent` 传入各自回调；step1 / step5 的 `input_schema` 补 `host_session_id`（`required`）并加入 `hidden_args`；协调器 prompt 的传参要求从「step2/3/4」扩到「`flexible_step1` ~ `flexible_step5`」，并在 §需求澄清（step1）与 §输出确认与模板化（step5）两处调用说明里补上该参数。

待办 / 偏差：

- **运行时验收未做**：§10 的并发归属、挂起-恢复归属仍需启动应用手测（step3/step4 的挂起-恢复路径虽已透传 `arguments`，但未实测）。
- ~~§12 的两项现存问题未修~~ → **已修（阶段 5）**：`flexible_save_template` 已进协调器 `allowed_tools`；`max_tool_rounds: 2` 与 `builtin_read_documentation` 测试残留已还原（回默认 10）。
- ~~协调器 prompt 中各处 `flexible_state` 的 `save` 步骤保留（与回调双写）~~ → **已移除（阶段 5）**：`flexible_state` 改为**只读**（删 `save` 及 `action`/`current_step`/`products` 参数），prompt 删除全部 `save` 与「产物一致性原则」整节、换为「重做与状态」；回调成为状态登记的**唯一写入方**。取舍：回调若因缺 `host_session_id` 而跳过登记，该步将没有任何状态记录（已用 schema `required` 约束协调器务必传参）。另同步：step5 入参 `field_selection_result` 由 `string` 改为 `object`；step3 入参 `output_format` 列入 `required`；step2 的 `runtime_context` 改为**补传**（上一轮 `compressed_context`）；step1 任务基线来源唯一化为 `flexible_state` 的 `products`。验证：6 个 prompt 经 `FilePromptManager` 实测全部可加载、`{{ session_id }}` 渲染正常。
- step5 的回调在协调器调用 `flexible_save_template` **之前**触发，即 `current_step` 会先变成 `templated`；落库失败时协调器重跑 step5，回调再写一次（幂等）。

## 14. 回调硬失败的中断通道（`ResultDecision::Abort`）

**问题**：回调在完成态做的事不全是纯内存操作——`StepCallback` 需要 `merge_state` **写库**。写库失败时旧实现只 `tracing::error!` 然后 `Accept`（静默跳过登记），于是：协调器看到「正常」的 step 输出、以为已定稿并继续下一步，而 `flexible_state` 里根本没有产物。`Retry` 也不适用——模型重出一次照样写不进去。

**方案**：`ResultDecision` 增加第四个变体 `Abort(String)`——中断本次子 agent 调用、**不重试**，语义＝「模型没错，但外部动作失败了，立刻收场并如实上报」：

- `crates/planned-agent/src/chat/sub_agent/callback.rs`：新增 `Abort(String)` 及文档（与 `Retry` 的分工）。
- `crates/planned-agent/src/chat/sub_agent/collect.rs`：`ResultDecision::Abort(reason)` 直接 `return Ok(SubAgentRunOutcome::Done(ToolResult { is_error: true, content: reason }))`，复用既有的 `SendOutcome::Failed` 错误通道，跳过重试循环。
- `step_callback/step_commit.rs`：`merge_state` 返回 `Err` ⇒ `ResultDecision::Abort("… 流程状态登记失败（plan_id=…, host_session_id=…）：{e}")`，不再静默 `Accept`。
- `flexible_global_system.toml`：补一条——step 工具返回 `is_error` 结果时，把原因如实转述给用户并**停下**，不进入下一步、不反复重试同一工具。

`crates/tool-manager/src/sub_agent/executor.rs:137` 在 `Done` 分支只补 `call_id` 后原样返回，故 `is_error` 一路透传到父 agent 的 tool result（GUI 侧显示为错误结果）。验证：`cargo check -p planned-agent-gui` 通过。

## 15. 模板副本：消除「协调器转抄」（阶段 6）

**问题（运行期实测）**：一次完整流程跑通后，`plans_flexible_sessions.execution_plan[1].expected_schema` 落库成了 `""`。按 `chat_messages.sequence_order` 逐层取证：

| 环节 | `expected_schema`（step_2） | `lines`（step_1） |
|---|---|---|
| step5 子 agent 原始输出（`seq=19`） | `null` ✅ 合乎契约 | `"array"` |
| 协调器传给 `flexible_save_template` 的 `template` 参数（`seq=20`） | `""` ❌ | `{"type":"array"}` |
| 落库后 | `""` | `{"type":"array"}` |

即：**step5 没错、回调没错、落库也没错** —— 坏在「协调器 LLM 把 step5 的输出转抄成下一个工具的参数」这一步：它不是无损搬运，而是理解后重新生成 JSON，顺手把 `null` 写成 `""`、把 `"array"`「修正」成对象。（`flexible_step5.toml` 本就允许 `expected_schema` 为 `null`，表示该步不返回结构化数据。）

**方案**：让模板**不经 LLM 的手**。

- `StepSpec` 新增 `payload_key: Option<&'static str>`：定稿时把**整段输出对象**原样登记到该 key（`step_commit.rs` 的 `build_patch`）。
- `step5_callback`：`payload_key: Some("template_payload")`，`products: &[]`。
- `flexible_save_template`：schema **移除 `template` 参数**（只剩 `session_id`），改为 `load_state` 读 `products.template_payload`；缺副本时返回 error 提示重跑 step5。落库前的 `steps` / `execution_plan` 结构校验保留。
- prompt：`flexible_global_system.toml` 的状态表、工具清单、step5 调用说明三处同步为「只传 `session_id`，模板由系统取用，不得转抄」。

**顺带澄清（非缺陷）**：同一会话的 `flexible_state` 里没有 `field_selection_result`，不是登记失败 —— `seq=13` 显示 step3 返回 `empty_result`（无可提取的结构化字段），回调按契约**正确地**不登记；随后用户选「继续推进」，协调器直接把 `field_selection_result` 从 step3 返回内容构造后传给 step4。判定原则「宁可「不写」也不写错」在此按预期生效。

验证：`cargo check -p planned-agent-gui` 0 error；`cargo test --bin planned-agent-gui` 20 passed（含 `build_patch` 的 3 个新单测：整段复制保留 `null`、`products` + `clear` 组合、缺产物不写 `null`）。

## 16. 结果链：前置分析槽位 + 轨迹从会话历史导出（阶段 7）

**问题**：两件事各自缺一个「正确的落点」。
1. `execution_trace` 由**模型自述**（`flexible_step2.toml` 原先要求它逐条记录 `tool` / `input` / `output_summary`）。模型会漏步骤、编参数、甚至宣称调用过从未调用的工具 —— 而轨迹是产品核心资产，不能建立在一份「回忆」上。
2. 五个 step 的机械活（解析输出、判 `status`、定位 `host_session_id`）挤在 `step_commit.rs` 里；某个 step 想插一条自己的产物（如 step2 的 `execution_trace`），就只能往通用 `StepSpec` 上加特例字段，或让每个注册点自己记得挂一个「拦截器」。

**方案**（planned-agent 核心库只给槽位，不解释业务规则）：
- 新增 `SubAgentChainPrelude` + `PreludeOutcome { Proceed { analysis, outer }, Stop(decision) }`：**前置分析槽位** —— 链跑之前跑一次，产物随 `on_result` 交给每个回调；`outer` 可**定稿对外文本**（去围栏 / 紧凑 JSON / 路径正斜杠），`Stop` 则直接收场、链一个回调都不跑。
- `on_result` 签名聚合成 `SubAgentCall { ctx, result, history, analysis, is_last }`（`is_last` 让回调不必关心自己在链上的位置）。
- 注册侧：`Vec<Arc<dyn SubAgentResultCallback>>` → `SubAgentResultChain`（prelude + callbacks）。
- `run_chain`：先跑 prelude（**重试后每轮重跑**，分析永远对应当前输出），`final_content` 初值取 `outer`；末位 `Next` 仍等价 `Accept`。

**方案**（agent-gui：默认前置分析，业务代码看不见它）：
- `step_callback/analysis.rs`（新）：解析 + 规范化**纯函数**层（从 `step_commit.rs` 搬出，后者在阶段 9 被删除），`StepAnalysis` 是回调读取分析产物的视图（字段契约见 `analysis::keys`）。
- `step_callback/prelude.rs`（新）：`FlexibleStepPrelude` = 各 step 的**默认**前置分析 + 守门 —— 解析失败 ⇒ `Retry`（最多 2 次）；非定稿 `status` / 缺 `host_session_id` ⇒ `Stop(Accept)`（**不写任何产物**）。由各 step 的 `create_stepN_callback` 统一挂上，业务代码里看不到它，也无从漏挂。
- `step_callback/step_commit.rs`：曾把五个 step 的定稿登记收敛成一份 `StepCallback` + `StepSpec`；**阶段 9 已删除**，改为各 step 在 `stepN/mod.rs` 里自己写（见 16.1）。
- `step_callback/step2/tool_trace.rs`（新）：`export_cleaned_trace` —— 在零策略的 `export_tool_trace` 之上做产品策略：过滤 `request_user_action`（UI 交互不是执行步骤）、单条输出截断 2000 字符、路径转正斜杠。
- `step_callback/step2/execution_trace.rs`（新）：step2 结果链的一环（排在定稿登记**之前**），从**会话历史**导出真实轨迹写 `execution_trace`；写库失败 ⇒ `Abort`（如实上报，不静默）。
- **目录结构**（阶段 8）：`step_callback/` 根只留通用件（`analysis` / `prelude` / `before_inject`），各 step 收进 `stepN/`（`mod.rs` = 链组装 + 本 step 的定稿登记回调 + 单测，step 专属回调放同目录）—— 判断依据：只有某个 step 用就进 `stepN/`，多个 step 共用才提到根。对外接口（`step_callback::{create_stepN_callback, StateInjectCallback, HOST_SESSION_ID_FIELD}`）不变。
- `flexible_step2.toml`：删除 `execution_trace` 自述要求与输出字段，输出收敛为 `status` + `compressed_context` / `error_message`。

**step2 结果链**（顺序不可颠倒）：前置分析（解析 + 守门 + 定稿对外文本）→ 轨迹提取（`execution_trace`）→ 定稿登记（`compressed_context` + 清下游 + 推进 `executed`）。轨迹排在定稿登记**之前**：它落库失败就 `Abort`，此刻状态尚未推进（协调器重跑 step2 即自愈）；反过来先推进再落轨迹会留下「已 `executed` 却没有轨迹」的状态，step4/step5 会在缺轨迹的情况下继续。「没定稿却落轨迹」则由前置分析挡住。

**验证**：`cargo test -p planned-agent --lib chat::sub_agent` 20 passed（含 6 个 prelude / 链语义新单测）；`cargo test -p planned-agent-gui step_callback` 31 passed（含 `analysis` / `tool_trace` / `StepAnalysis` 契约 / 各 step 补丁构建新单测）。

### 16.1 阶段 9：各 step 自己保存状态（拆掉 `step_commit.rs`）

**问题**：`StepSpec` + `StepCallback` 把五个 step 的「保存状态」收敛成一份实现，而五者的产物 / 清理 / 推进规则只会越来越不一样 —— 它已经长出特例字段（step5 的 `payload_key` 就是为「整段输出」加的第一个口子）。继续这么长下去，通用结构会被特例撑变形。

**决定**：`step_commit.rs` **删除**，每个 `stepN/mod.rs` 自己写 `on_result` 全流程（取分析结论、组产物补丁、`merge_state`、错误处理、决策）。用冗余换各 step 能自由演化。

**边界**（不是保存状态、而是接口与把关，故不下放）：
- `prelude.rs`：`FlexibleStepPrelude`（默认前置分析 + 守门）—— `SubAgentChainPrelude` 是框架槽位，业务规则（`host_session_id` / `status`）属 flexible，step 作者看不见；
- `analysis.rs`：解析 / 规范化纯函数 + `StepAnalysis` —— prelude 产出、各回调消费的**跨文件契约**，各写一份迟早对不上。

**各 step 回调必须共同遵守的行为契约**（已写进 `step_callback/mod.rs` 文档）：
1. 前置分析产物缺失 ⇒ `Abort`（接线错误，不猜着写）；
2. 写库失败 ⇒ `Abort`（不可重试，如实上报，不静默 `Accept`）；
3. 决策只用 `call.is_last` 区分：非末位 `Next(call.text())`，末位 `Accept`（对外结果由前置分析定稿）；
4. 产物值为 `null` 或字段缺失 ⇒ 跳过写入（`merge_state` 把 `null` 当删除，直接写会静默清掉已有产物）；
5. 解析 / 定稿判定 / 会话定位一律用 `StepAnalysis`，不在回调里重复实现。

**各 step 的差异就地可读**：step1 写 `task_definition` + `output_format` 并清四个下游产物；step2 只写 `compressed_context`（轨迹由同目录 `execution_trace.rs` 单独落库）；step3 写 `field_selection_result` 并清 `parameter_confirmation_result`；step4 写 `parameter_confirmation_result`、无下游可清；step5 整段输出存 `template_payload`（`null` 原样保留）。

**验证**：`cargo check -p planned-agent-gui --all-targets` 0 error；`cargo test -p planned-agent-gui step_callback` 31 passed（另补回 `tool_trace` 的 5 个清洗测试：空历史空数组、过滤 `request_user_action`、短输出保结构、超长截断、路径转正斜杠）。

### 16.2 阶段 10：抽出零策略纯工具（`commit.rs`）

**问题**：阶段 9 把保存状态下放到各 step 后，出现了另一种重复 —— `on_result` 里四段**逐字相同**的代码（入口日志、取分析或 `Abort`、写库 + 错误处理、`is_last` 决策），五个文件各一份，合计约 250 行；`build_patch` 也在 step1~step3 逐字重复。

**决定**：只抽**零策略纯工具**（同样的入参必然同样的行为，不含任何 step 语义），不动「回调编排」：
- `step_callback/commit.rs`（新）：`build_patch(agent, parsed, products, clear)`、`commit_state(agent, service, plan_id, session_id, next_step, patch) -> Result<(), String>`、`hand_off(call)`；
- `analysis.rs` 增 `require_analysis(agent, call) -> Result<StepAnalysis, ResultDecision>`（与 `StepAnalysis` 同处，是它的读取封装）。

**边界**：**没有**恢复成 `StepSpec` / `StepCallback` —— 各 step 仍各自持有 `AGENT` / `OK_STATUS` / `NEXT_STEP` / `PRODUCTS` / `CLEAR` 常量与 `on_result` 编排，差异一眼可见；step5 的「整段输出存 `template_payload`」仍是它的私有函数（只此一处用）。`step_callback/mod.rs` 里的 5 条行为契约现在直接标注了对应的公共函数 —— 契约由函数**强制**，不再是「各写一遍的纪律」。

**收益**：五个 step 文件合计约 700 → 400 行；公共行为只在 `commit.rs` 测一次（`null` 跳过 / 缺失跳过 / `clear` 覆盖 / 空补丁 / 原样克隆），各 step 只留「自己的常量取值正确」用例。

**验证**：`cargo check -p planned-agent-gui --all-targets` 0 error；`cargo test -p planned-agent-gui step_callback` 31 passed（`commit` 4 例 + 各 step 各 1 例常量锁定期 + `analysis` 12 + `before_inject` 5 + `tool_trace` 5）。
