# 灵活模式 · 会话/版本架构改造方案（无 session 表 · 最终版）

> 目标：把「**会话（一段生产过程）**」与「**版本（一次定稿成果）**」解耦，
> 使每个版本都能翻回产出它的那段历史会话，并为下一次生产继承上次成果作为参考。
>
> **已确认的设计约束**
> 1. 会话边界：一次「发起意图 → 澄清 → 执行 → 定稿」= **一个 session**；定稿（version 写入）即**封版**。
> 2. 想再做/基于旧版本调整 → **另开新 session**，并继承上次成果作参考。
> 3. **不加独立的 session 表**。
> 4. 不兼容既有代码/数据，可重建相关表。

---

## 一、核心模型

```
plan（一个计划）
 ├─ session_A ─ 定稿 v1（封版）         → 版本 v1 可翻回 session_A 的对话
 ├─ session_B ─ 在 v1 基础上定稿 v2（封版）→ 版本 v2 可翻回 session_B 的对话
 └─ session_C（当前草稿，进行中，尚无 version）
```

**不建 session 表**，session 只是一个「字符串 id + 数据归属」，体现在三处：
- `plans.current_session_id`：**当前工作草稿**指针（谁是正在创作中的 session）。
- `chat_messages.session_id`：哪些消息属于这段会话。
- `plans_flexible.session_id`：哪个版本由哪段会话产出（版本→会话反查的关键）。

**关键规则**
1. `session_id` 在「需要一个草稿」时分配（进入页面/新建草稿），与「是否已产出 version」无关 → 无「首次没版本」问题。
2. 一个 session 最多产出一个 version（定稿即封版），保证「版本 ↔ 会话」1:1 干净。
3. 「某 session 是否封版」= 该 plan 下 `plans_flexible` 是否存在 `session_id = 它` 的记录（可判定，无需 session 表）。

---

## 二、数据模型

### 2.1 `plans`（微调）

| 字段 | 变更 | 说明 |
| --- | --- | --- |
| id / name / description / mode / status | — | 不变 |
| flexible_version | 保留（语义不变） | 当前生效成果版本指针；0=尚无定稿 |
| **current_session_id** | **新增** | 当前工作草稿 session id；可空（首次进入前为空） |

### 2.2 `plans_flexible`（成果库：新增 session_id）

| 字段 | 变更 | 说明 |
| --- | --- | --- |
| id / plan_id | — | 不变 |
| **session_id** | **新增** | 产出该成果的会话 id（版本 → 过程回溯） |
| version | — | 仍按 plan 内递增（1,2,3…） |
| input_schema / output / steps / execution_plan | — | 不变 |
| created_at | — | 不变 |

### 2.3 `chat_messages`（过程库：plan_id → plan_id + session_id）

| 字段 | 变更 | 说明 |
| --- | --- | --- |
| id | — | 不变 |
| plan_id | 保留 | 冗余便于按 plan 粗筛 |
| **session_id** | **新增** | 归属会话；load / append / clear / rollback 都按此过滤 |
| sequence_order | 语义微调 | 改为「会话内」排序序号，非 plan 内全局 |
| message_json / is_error_type / is_agent_tool / created_at | — | 不变 |

> **无 `sessions` 表**。会话元数据（状态、衍生、参考）不落独立表，见 §3。

---

## 三、运行时流程

### 3.1 进入 flexible 页的判定（current_session_id 生命周期）

`current_session_id` 语义 = 「当前工作草稿」。进入页面时分三种情况：

```
进入 flexible 页：
  cur = plans.current_session_id

  if cur 为空：                      # 情况①：plan 首次进入，从没开过草稿
      cur = 生成新 session_id
      plans.current_session_id = cur
      加载空会话（无历史）

  else if 该 cur 已产出版本：         # 情况②：上次定稿后封版，未开新草稿
      # 封版会话只读回溯，不作为编辑草稿 → 自动开新草稿
      cur = 生成新 session_id
      plans.current_session_id = cur
      注入最新版本(flexible_version 指向)的参考摘要 作为新会话首条 system 消息
      加载空会话（含参考首条）

  else：                            # 情况③：上次创作未定稿（仍在澄清/执行中）
      # 回到未完成的草稿继续
      加载 chat_messages where session_id = cur
```

行为对照用户预期：
- 首次进入 → 新草稿 session_A。
- 创作中途退出重进 → 回到 session_A 继续（情况③）。
- **定稿封版后退出重进 → 自动得到新草稿**（情况②）——即「下次进入是新会话」，且参考已带好。
- 定稿后不退出、想续作 → 由按钮触发开新（§3.3）。

### 3.2 定稿即封版（step5 落库时）

`step5_callback` 校验通过后扩展为：
1. 写 `plans_flexible` 时带上当前 `session_id`；
2. 更新 `plans.flexible_version = 新 version`；
3. **当前 session 即封版**——不再有独立状态写入，其含义是「current 指针后续会移走，数据保留，由新 version 的 session_id 反查可达」。

### 3.3 手动开新草稿（基于某版本新建）

页面提供「基于最新版本新建草稿」动作（也可扩展为任意版本）：
1. 若当前仍在未封版草稿且未产出，可选地先放弃或完成；
2. 生成新 session_id，`plans.current_session_id = 新 id`；
3. 从目标 version（默认 `plans.flexible_version` 指向的最新版）提炼参考摘要，作为新会话首条 system 消息写入 `chat_messages`；
4. 页面切到空草稿（参考已注入）。

### 3.4 翻回任何版本的历史会话

不依赖 current 指针，而是版本反查：

```
版本列表（plans_flexible，可按 plan_id 列出）
  → 点某版本 vN
  → 取 vN 的 session_id
  → 查 chat_messages where session_id = vN.session_id
  → 展示 vN 产出时的完整对话
```

因「一 session 一版本」，vN 的会话是**专属**的，不会混入其他版本。

---

## 四、参考提炼注入

`step5_callback` 拿到最新 `plans_flexible::Model` 后生成紧凑摘要（这就是新会话「继承上次成果」的载体）：

```
上次成果 vN（产生于会话 X）：
- 任务：从 MySQL 查询本月已支付订单并导出 Excel
- 输入 input_schema：month(YYYY-MM, 必填), status(默认 paid)
- 输出 output：Excel, fields=[order_id, amount, customer_name]
- steps 概览：step_1 query_tool → step_2 export_tool(引用 step_1.output)
- 本次为在 vN 基础上调整，请只确认变化点
```

**注入落点（无表方案）**：作为新 session 的**首条 system 消息**持久化进 `chat_messages`。
- 优点：无需额外存储字段/表；重进情况②时该消息随会话自动加载，父 Agent 通过「会话首条是上次成果参考」即可感知；无需在 prompt 变量里再塞。
- 展示：可折叠/浅色，或不渲染为气泡，仅作上下文。

> 备选：通过父 Agent system prompt 变量注入。缺点是需要额外状态承载"当前草稿衍生自哪个版本"，故不采用。

---

## 五、涉及改动文件（对照）

| 层 | 文件 | 改动 |
| --- | --- | --- |
| migration | `m20260801_create_chat_messages.rs` | 加 `session_id`；序号改会话内 |
| | `m20260801_create_plans_flexible.rs` | 加 `session_id` |
| | `m20260801_create_plans.rs` | 加 `current_session_id` |
| | （可选）新建「开新版本时会话参考」的既有表复用，无新表 | — |
| entity | `chat_message.rs` / `plans_flexible.rs` / `plan.rs` | 同步加字段（无新 entity） |
| repo | `chat_message_repo.rs` | 查询/删除/序号 加 session 维度 |
| | `plans_flexible_repo.rs` | `create` 加 session_id；加「按 plan 列版本 + 取某 session 是否已产出」辅助查询 |
| store | `chat_message_storage.rs` | `ChatMessageStore` 持 `(plan_id, session_id)` |
| service | `plans_flexible_service.rs` | `write` 加 session_id；加「提炼最新成果为参考摘要」 |
| page | `flexible/page.rs` | 进入判定（§3.1）；按当前 session 加载历史；「基于最新版本新建草稿」按钮；版本列表入口（翻回历史） |
| callback | `flexible/step5_callback.rs` | 写库带 session_id；更新 `flexible_version`；产出参考摘要 |
| prompts | `flexible_global_system.toml` | 说明「会话首条若为上次成果参考，则按 delta 澄清，不重复提问已确认项」 |

---

## 六、实施计划（分阶段）

### Phase 0 — 数据层（表结构重建）
- migration：`chat_messages`+`session_id`（序号会话内）、`plans_flexible`+`session_id`、`plans`+`current_session_id`。
- entity 三文件 + repo 三文件（含「查某 session 是否已产出」辅助）。
- 验证：`cargo build`；迁移后列结构正确。

### Phase 1 — 存储层绑定 session
- `ChatMessageStore` 持 `(plan_id, session_id)`，load/append/clear/rollback 全按会话隔离；append 序号取会话内最大 +1。
- `page.rs` 历史加载传当前 session。
- 验证：同 plan 两个 session 消息互不可见。

### Phase 2 — 进入判定 + 定稿封版
- 页面进入分支逻辑（首次/中途/已封版开新）。
- `step5_callback` 写库带 session_id + 更新 `flexible_version`。
- 验证：定稿后重进自动得到新草稿；v1.session_id 落库。

### Phase 3 — 参考注入 + UI（新建草稿 / 翻回历史）
- `plans_flexible_service` 提炼最新成果摘要。
- 开新草稿时把摘要作为首条 system 消息写入。
- UI：「基于最新版本新建草稿」；「版本列表 → 点开翻回对应会话」。
- prompt 适配 delta 澄清。
- 验证：v1→基于 v1 新建 v2→分别翻回 v1/v2 各自对话，互不混；新草稿带参考、不重复提问已确认参数。

### Phase 4 — 端到端收尾
- 走一遍「澄清→定稿 v1→新建→改参数→定稿 v2→翻回 v1 会话」。
- `cargo build`、跑既有单测（json_extract 等）。

---

## 七、开放问题（已收敛 + 待 UI 细节）

已确认：
- [x] 会话边界 = 一次生产一个 session，定稿封版。
- [x] 不加 session 表。
- [x] 版本→会话反查 = `plans_flexible.session_id`。
- [x] current_session_id = 当前工作草稿；重进已封版自动开新。
- [x] 参考注入落点 = 新会话首条 system 消息。

待实现时敲定（低风险，可按默认）：
- 版本列表的 UI 位置（页头下拉 / 侧栏 / 进入时的版本选择）。
- 翻回历史会话时是「只读展示」还是「可继续编辑」（建议只读，续作走新建草稿）。
- 是否保留 `step2_callback` / `json_extract`（现状预留，本次改造不强制清理）。
