# 灵活模式 · step 回调会话归属设计（让回调动态拿到会话标识）

> 状态：**设计待评审**（本文件只描述方案，未改动任何代码）
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

（step1 在两端都不走回调路径，可暂不加；step5 若将来也下沉再加。）

**⑧ 协调器 prompt**（`prompts/flexible/flexible_global_system.toml`）说明：调用 `flexible_step2` / `flexible_step3` / `flexible_step4` 时必须带上 `host_session_id`，值照抄「会话上下文」段。

> ⚠️ **不要**在 step 子 agent 的 prompt 里提 `host_session_id`：它已列入 `hidden_args`，**不会**出现在子 agent 收到的 task 文本里（子 agent 也不需要它）。传参义务只在父协调器侧。

### 7.3 配套契约收紧（回调要判定定稿，必须先修）

回调需要在**代码**里判定「本次是定稿还是非定稿」。现状：

| step | 定稿信号 | 非定稿信号 | 机器可判定？ |
|---|---|---|---|
| step2 | JSON `status:"success"` | JSON `status:"error"` | ✅ 达标（`flexible_step2.toml:52-70`） |
| step3 | 首行 `# 输出确认` | `back_to_execute` / `empty_result` / `cancelled` | ⚠️ `flexible_step3.toml:100-104` 只说「返回 X 状态」，**无精确首行模板** |
| step4 | 首行 `# 参数确认` | `back_to_step3：...` / `cancelled：...` | ✅ 接近达标（`flexible_step4.toml:65-74`） |

→ **必须先收紧 step3 的非定稿输出为严格首行标记**，否则代码解析不稳。（判定原则：宁可「不写」也不错写。）

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
- [ ] 扩到 step3 / step4
- [ ] 跑 §10 验收 1、2

### 收尾

- [ ] `cargo check` 三 crate 全绿
- [ ] 视情况更新 `docs/chat-flexible-多会话保活设计.md` §3 D，加指向本文件的回链

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
