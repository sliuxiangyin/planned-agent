# 灵活模式 · 会话/版本架构改造方案（sessions 表版 · 当前实现基线）

> 目标：把「**会话（生产过程）**」与「**版本（定稿成果）**」解耦并记录各自归属，
> 使下次进入能定位会话、每个版本可关联产出它的会话。
>
> **状态**：数据模型与主要会话生命周期已落地，`cargo build` 通过。
> `plans_flexible_tool`（无会话旁路写入口）已废弃删除；`chat_messages` / `plans_flexible`
> 的 `session_id` 已收紧为必填（`NOT NULL`，实体/仓库均去掉 `Option`）。
> 剩余「封版开新、参考注入、翻回历史、prompt 分支」见「五、待做」。

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
- 语义与落地情况：
  - **已落地**：同一 session 内首次 step5 定稿 → `INSERT` 一条 `plans_flexible`；同 session 后续改需求再定稿 → **UPDATE 覆盖**同一行，version 不变。
    由 `PlansFlexibleService::save_snapshot` 统一实现（见 4.3）。
  - **待落地**：真正「封版（`produced`）+ 开新会话」由用户「定稿完成」触发，不自动开新（见第五节）。

### 1.2 会话指针如何定位（plans.current_session_id 的作用）

- `plans.current_session_id` 记录「当前活动/上次会话」，下次进入时默认定位该会话。
- **已落地**（`PlansFlexibleService::ensure_current_session`）：
  - 指针指向的会话存在且有效 → 直接返回它；
  - 指针为空 / 指向的会话已不存在 → 新建一个 `active` 会话并回写指针。
- **未落地**（与最初方案的差异）：暂未实现「指针失效 → 先回退 `sessions.active` 再新建」的中间分支，
  当前指针失效会直接开新会话。见「五、待做」第 1 条。
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

### 2.2 `chat_messages`（加列，已收紧必填）

| 字段 | 变更 | 约束 |
| --- | --- | --- |
| session_id | 新增 | string **not null**, FK→sessions(cascade) |

> 消息按 session 隔离读写，`ChatMessageStore` 只读写 `(plan_id, session_id)`。
> 会话内序号查询索引：`idx_chat_messages_session(session_id, sequence_order)`。

### 2.3 `plans_flexible`（加列，已收紧必填）

| 字段 | 变更 | 约束 |
| --- | --- | --- |
| session_id | 新增 | string **not null**, FK→sessions(cascade) |

> 快照必然归属某个会话（step5 落库路径总会带当前 session_id）；
> 原「无会话旁路写入口」`plans_flexible_tool` 已废弃删除，故不再需要可空列。
> 索引：`idx_plans_flexible_session(session_id)`。

### 2.4 `plans`（加列）

| 字段 | 变更 | 约束 |
| --- | --- | --- |
| current_session_id | 新增 | string **nullable**（无物理 FK，指向 sessions.id） |

> 保留 `flexible_version`（当前生效版本指针），两者各自独立：一个记「当前会话」，一个记「当前版本」。

---

## 三、已完成的改造（清单）

### 新增文件
- `storage/migrations/m20260901_create_sessions.rs`（sessions 表）
- `storage/entities/session.rs`
- `storage/repository/session_repo.rs`
  - `status` 常量：`ACTIVE/PRODUCED/ABANDONED`
  - 方法：`create(plan_id, derived_from_version, reference_context)`、`find_by_id`、`find_by_plan_id`、`find_active_by_plan_id`、`update_status`
- `pages/plan/flexible/controller.rs`：`FlexibleController` + `ChatServiceFactory`
  - 收编 ChatService 异步初始化：storage ready → `ensure_current_session` → 绑定 session store
  - 同步 `session_id` 信号与共享 `SessionSlot`（tokio::sync::watch：Sender 承载当前会话最新值，`step5_callback` 等 `'static` 旁路经 Receiver 读取）
  - 预留 `switch_session`（切换/翻回历史用，暂 `#[allow(dead_code)]`）
- `pages/plan/flexible/chat_flexible_message_storage.rs`：`ChatMessageStore`（绑定 plan+session 的 `ChatHistoryStore`）

### 改动的文件
- `migrations/m20260801_create_plans.rs`：加 `current_session_id`
- `migrations/m20260801_create_plans_flexible.rs`：加 `session_id` + FK + 索引，列 **not null**
- `migrations/m20260801_create_chat_messages.rs`：加 `session_id` + FK + 索引，列 **not null**
- `migrations/mod.rs`：注册 sessions，顺序 `tests → plans → sessions → plans_flexible → chat_messages`（满足 FK）
- `entities/plan.rs`（`current_session_id: Option<String>`）
- `entities/plans_flexible.rs`（`session_id: String`，必填）
- `entities/chat_message.rs`（`session_id: String`，必填）
- `entities/mod.rs`（登记 `session`）
- `repository/plan_repo.rs`：`create` 初始 `current_session_id=None`；新增 `update_current_session_id`
- `repository/plans_flexible_repo.rs`：`create` 收必填 `session_id: &str`；新增 `find_by_plan_and_session`、`update_content`
- `repository/chat_message_repo.rs`：`create` 收必填 `session_id: &str`；新增 `find_by_plan_and_session`
- `repository/mod.rs`（登记并 re-export `SessionRepo`）
- `context/storage.rs`：装配 `SessionRepo`，新增 `session_repo()` 访问器
- `services/plans_flexible_service.rs`：新增 `ensure_current_session`、`save_snapshot`
- `pages/plan/flexible/step5_callback.rs`：从共享 watch Receiver `borrow()` 读当前会话，调 `save_snapshot` 落库

### 删除（废弃/被取代）
- `context/tools/plans_flexible_tool.rs`：废弃的无会话旁路写入口，已移除
- `storage/chat_message_storage.rs`：旧的非会话绑定存储，被 `chat_flexible_message_storage.rs` 取代

---

## 四、关键流程（当前已落地）

### 4.1 进入/切换会话（controller）
- ChatService 初始化时经 `storage.ensure_current_session(&plan_id)` 定位（必要时新建 active 会话）。
- `ChatServiceFactory::build_for_session(&session_id)` 构造绑定该 session 的 `ChatMessageStore` 的 `ChatService`。
- 把当前 `session_id` 写入 `session_id` 信号，并对 `SessionSlot`（watch）`set()` 当前会话，使旁路（如 step5 回调）能定位归属 session。

### 4.2 聊天消息读写
- `ChatMessageStore.load / append / clear` 只作用于 `(plan_id, session_id)`。
- `ChatMessageRepo::create` 的 `session_id` 为必填 `&str`，杜绝空归属。

### 4.3 step5 定稿落库
- `step5_callback.on_result` 解析子 agent 输出的 JSON，从 watch Receiver `borrow()` 读取当前会话 id。
- 同一 session 首定稿 → `save_snapshot` 走 `INSERT`（version = 该 plan 最新 +1）；再产出 → `UPDATE` 覆盖（version 不变）。
- 读取不到当前会话 id（不变量被破坏）时 loud error 并跳过落库，防止写入脏数据。

---

## 五、待做

1. **定位回退 active**：`ensure_current_session` 目前「指针失效 → 直接新建」；补上「先回退 `sessions.active`」的中间分支。
2. **`plans.flexible_version` 更新**：`save_snapshot` 落库后回写当前生效版本指针（`plan_repo.update_flexible_version` 已就绪未接）。
3. **定稿后分支（父 Agent prompt）**：`flexible_global_system.toml` step5 后加 `request_user_action`——「在当前版本上修改需求」→ 同 session 回澄清/执行；「定稿完成」→ 封版该 session（`produced`）+ 开新 session 并写 `plans.current_session_id`。
4. **参考注入**：开新 session 时把上一版成果（input_schema/output.fields/steps 概览）写入 `reference_context`/首条上下文（`session_repo.create` 已支持 `derived_from_version` / `reference_context`，暂传 `None`）。
5. **翻回历史**：按 `plans_flexible` 列出版本，用 `session_id` 反查对应会话对话；接 `controller.switch_session`（已预留）。
6. **prompt 适配**：同步 `flexible_global_system.toml`。

---

## 六、验证

- `cargo build`（在 `crates/agent-gui`）已通过（仅 never-used 警告，多为尚未接入业务的 repo / controller 预留方法，属预期）。
- 起库跑 migration，确认 `sessions` 表、三处 session 字段（`chat_messages` / `plans_flexible` 为 NOT NULL，`plans.current_session_id` 可空）、FK、索引创建成功。
- 注意：本轮仅改动 migration 源文件（开发期定型）；若本地库此前已跑过 migration，需删库重跑或补 `ALTER` 迁移才能使列收紧生效。
