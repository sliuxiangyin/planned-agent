# 灵活模式 · 会话/版本架构改造方案（sessions 表版 · 当前实现基线）

> 目标：把「**会话（生产过程）**」与「**版本（定稿成果）**」解耦并记录各自归属，
> 使下次进入能定位会话、每个版本可关联产出它的会话。
>
> **数据库改造已完成（`cargo build` 通过）**；会话生命周期业务尚未接入。

---

## 一、最终模型与关键语义

### 1.1 会话（session）与版本（version）

```
plan（一个计划）
 ├─ session_A（生产 v1 的会话，封版）
 ├─ session_B（在 v1 基础上生产 v2 的会话，封版）
 └─ session_C（当前 active 草稿）
```

- 一个 session = 一次「发起意图 → 澄清 → 执行 → 定稿」的生产过程。
- 状态：`active`（进行中/未定稿）、`produced`（已定稿封版）、`abandoned`（中途被弃）。
- 目标语义（业务阶段落地）：
  - 同一 session 内：首次 step5 定稿 → `INSERT` 一条 `plans_flexible`（version=该 session 的版本号）；同 session 后续改需求再定稿 → **UPDATE 覆盖**同一行，version 不变。
  - 新开 session → `INSERT` 新行，version 递增。
  - 真正「封版 + 开新会话」由用户「点击定稿/定稿完成」触发，不自动开新。

### 1.2 会话指针如何定位（plans.current_session_id 的作用）

- `plans.current_session_id` 记录「当前活动/上次会话」，**下次进入时默认定位**该会话：
  - 指向的会话存在且可用 → 加载它；
  - 否则回退到 `sessions` 中该 plan 的 `active` 会话；
  - 都没有 → 开新 `active` 会话并回写指针。
- 因此 `plans.current_session_id` 是指针/快捷，权威状态仍以 `sessions.status` 为准。

---

## 二、数据模型（当前已落地）

### 2.1 `sessions`（新增表）

| 字段 | 类型/约束 | 说明 |
| --- | --- | --- |
| id | string PK | 即 `session_id` |
| plan_id | string not null, FK→plans(cascade) | 所属计划 |
| status | string not null | `active` / `produced` / `abandoned` |
| derived_from_version | int null | 从哪个版本衍生；首会话为 null |
| reference_context | string null | 参考注入文本 |
| created_at / updated_at | string not null | |
| closed_at | string null | 封版时间 |

索引：`idx_sessions_plan(plan_id)`

### 2.2 `chat_messages`（加列）

| 字段 | 变更 | 约束 |
| --- | --- | --- |
| session_id | 新增 | string **nullable**, FK→sessions(cascade) |

> 会话生命周期接入前可空；接入后按 session 隔离读写，届时可选收紧为 not null。
> 已有会话内序号查询索引：`idx_chat_messages_session(session_id, sequence_order)`。

### 2.3 `plans_flexible`（加列）

| 字段 | 变更 | 约束 |
| --- | --- | --- |
| session_id | 新增 | string **nullable**, FK→sessions(cascade) |

> `plans_flexible_tool`（无会话的 LLM 通用写入口）等旁路可写空，故可空；step5 落库路径总会填。
> 索引：`idx_plans_flexible_session(session_id)`。

### 2.4 `plans`（加列）

| 字段 | 变更 | 约束 |
| --- | --- | --- |
| current_session_id | 新增 | string **nullable**（无物理 FK，指向 sessions.id） |

> 保留 `flexible_version`（当前生效版本指针），两者各自独立：一个记「当前会话」，一个记「当前版本」。

---

## 三、已完成的数据库改造（清单）

**新增文件**
- `storage/migrations/m20260901_create_sessions.rs`（sessions 表）
- `storage/entities/session.rs`
- `storage/repository/session_repo.rs`
  - `status` 常量：`ACTIVE/PRODUCED/ABANDONED`
  - 方法：`create(plan_id, derived_from_version, reference_context)`、`find_by_id`、`find_by_plan_id`、`find_active_by_plan_id`、`update_status`

**改动的文件**
- `migrations/m20260801_create_plans.rs`：加 `current_session_id`
- `migrations/m20260801_create_plans_flexible.rs`：加 `session_id` + FK + 索引
- `migrations/m20260801_create_chat_messages.rs`：加 `session_id` + FK + 索引
- `migrations/mod.rs`：注册 sessions，顺序 `tests → plans → sessions → plans_flexible → chat_messages`（满足 FK）
- `entities/plan.rs`（+`current_session_id: Option<String>`）
- `entities/plans_flexible.rs`（+`session_id: Option<String>`）
- `entities/chat_message.rs`（+`session_id: Option<String>`）
- `entities/mod.rs`（登记 `session`）
- `repository/plan_repo.rs`：`create` 初始 `current_session_id=None`；新增 `update_current_session_id`
- `repository/plans_flexible_repo.rs`：`create` 加 `session_id: Option<String>`
- `repository/chat_message_repo.rs`：`create` 加 `session_id: Option<String>`；新增 `find_by_plan_and_session`
- `repository/mod.rs`（登记并 re-export `SessionRepo`）
- `context/storage.rs`：装配 `SessionRepo`，新增 `session_repo()` 访问器

---

## 四、最小占位（保证编译，业务待接入）

| 位置 | 现状 | TODO |
| --- | --- | --- |
| `storage/chat_message_storage.rs` | `append` 调 `create` 时 `session_id=None` | 绑定当前 session |
| `services/plans_flexible_service.rs` | `write(plan_id, session_id: Option<String>, …)` 已透传 | — |
| `pages/plan/flexible/step5_callback.rs` | `write` 传 `None` | 传产出该版本的 session_id |
| `context/tools/plans_flexible_tool.rs` | `write` 传 `None` | 工具无会话上下文，保持 `None` |

---

## 五、待做：会话生命周期业务阶段

1. **首次进入/定位会话**：读 `plans.current_session_id` → 回退 `sessions` 的 active → 无则开新 active 会话并回写 `plans.current_session_id`；`ChatMessageStore` 绑定该 session 读写。
2. **step5 落库带 session**：`step5_callback` 携带当前 session_id；同一 session 首定稿 `INSERT`、再改需求定稿 **UPDATE 覆盖**（需在 `PlansFlexibleRepo` 增按 session 查/更新，version 不变）；`plans.flexible_version` 更新。
3. **定稿后分支（父 Agent prompt）**：`flexible_global_system.toml` step5 后加 `request_user_action`——「在当前版本上修改需求」→ 同 session 回澄清/执行；「定稿完成」→ 封版该 session（`produced`）+ 开新 session 并写 `plans.current_session_id`。
4. **参考注入**：开新 session 时把上一版成果（input_schema/output.fields/steps 概览）写入 `reference_context`/首条上下文，父 Agent 按 delta 澄清。
5. **翻回历史**：按 `plans_flexible` 列出版本，`session_id` → `chat_messages` 反查对应会话对话。
6. **prompt 适配**：同步 `flexible_global_system.toml`。

---

## 六、验证

- `cargo build`（在 `crates/agent-gui`）已通过（仅 never-used 警告，多为尚未接入业务的 repo 方法，属预期）。
- 起库跑 migration，确认 `sessions` 表与三处 session 字段、FK、索引创建成功。
