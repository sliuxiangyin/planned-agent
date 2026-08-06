# 计划存储设计 —— 周密模式 & 灵活模式

> 版本：v1.0
> 创建：2026-08-06
> 状态：设计中（提前规划，尚未实现）

---

## 1. 背景

Planned Agent 有两种执行模式，分别产出不同类型的计划：

| | 周密模式 (Thorough) | 灵活模式 (Flexible) |
|---|---|---|
| **路径** | 澄清 → Coarse 规划 → ReAct 探路 → **脚本固化** | 自由执行 → 轨迹提取 → Coarse 提炼 → **保存计划** |
| **产出** | **固化计划**：零或极少 AI 调用，可定时调度 | **灵活计划**：保留少量 AI 调用，适合环境多变 |
| **回放方式** | 纯工具调用序列，直接执行 | 大部分工具调用 + 关键节点保留 AI 判断 |
| **适合场景** | 已稳定流程、重复性任务、定时任务 | 探索性任务、首次执行、环境不确定 |

核心问题：**这两种计划产出物应该如何存储？**

---

## 2. 现有存储能力（已实现）

当前项目已具备三层存储基础设施：

```
┌─────────────────────────────────────────────────────────┐
│  SQLite (agent-gui.db)  via SeaORM                       │
│                                                         │
│  ┌──────────────┐  ┌──────────────────┐                │
│  │ plans 表      │  │ messages 表       │               │
│  │ · id         │  │ · plan_id (FK)   │               │
│  │ · name       │  │ · role / content │               │
│  │ · mode       │  │ · created_at     │               │
│  │ · status     │  └──────────────────┘               │
│  │ · todos ([]) │                                       │
│  │ · created_at │                                       │
│  └──────────────┘                                       │
│                                                         │
│  traces/ 目录（文件系统）                                 │
│  ┌──────────────────────────────────────┐              │
│  │ {date}-{seq}.json                   │              │
│  │ ExecutionTrace: 泛化后的工具调用模板  │              │
│  │ (见 trace-rag-design.md Phase 1)     │              │
│  └──────────────────────────────────────┘              │
└─────────────────────────────────────────────────────────┘
```

| 层 | 存什么 | 状态 |
|---|---|---|
| `plans` 表 | 计划元数据（名称、模式、状态） | ✅ 已实现 |
| `plans.todos` | JSON 字段，预留用于存步骤，当前固定为 `[]` | ⬜ 占位 |
| `messages` 表 | 对话历史（按 plan_id 关联） | ✅ 已实现 |
| `traces/*.json` | 每个步骤泛化后的工具调用模板 | ✅ Phase 1 已实现 |

**缺口**：`CoarseGrainedPlan` 生成的步骤内容、固化后的可回放脚本——目前仅在内存中流转，没有持久化到数据库。

---

## 3. 设计目标

1. 两种模式的计划产出物应有**统一的存储模型**（共用 schema，字段区分类型）
2. 支持**渐进式固化**：灵活计划执行多次后可升级为固化计划
3. 固化脚本支持**版本管理**（页面改版后重新探路）
4. 回放时能区分"纯工具调用"和"需要 AI 的步骤"，零歧义

---

## 4. 方案设计

### 4.1 核心思路：Trace 即计划

`CoarseGrainedPlan` = 计划的骨架（步骤意图 + 依赖关系）
`ExecutionTrace` = 每一步的具体执行模板（泛化后的工具调用序列）

**固化计划** = CoarseGrainedPlan + 所有步骤都有成熟的 ExecutionTrace（迭代次数低、稳定）
**灵活计划** = CoarseGrainedPlan + 部分步骤无 trace 或 trace 不稳定

不需要额外创造"计划脚本"新类型。两种计划的区别仅在于：步骤关联的 trace 是否足够成熟。

```
周密模式产出                      灵活模式产出
─────────────                    ─────────────
CoarseGrainedPlan                CoarseGrainedPlan
├── Step1: navigate              ├── Step1: navigate
│   └── Trace (成熟)   ←fixed    │   └── Trace (成熟)   ←fixed
├── Step2: fill                  ├── Step2: fill
│   └── Trace (成熟)   ←fixed    │   └── Trace (成熟)   ←fixed
├── Step3: click                 ├── Step3: AI 判断
│   └── Trace (成熟)   ←fixed    │   └── 无 Trace        ←ai
├── Step4: extract               ├── Step4: extract
│   └── Trace (成熟)   ←fixed    │   └── Trace (不稳定)  ←ai
                                 │
→ 全部 fixed → 零 AI 调用         → 部分 ai → 仍需 LLM
```

### 4.2 存储 Schema 设计

复用 `plans` 表的 `todos` 字段，将计划步骤内容结构化存储：

```jsonc
// plans.todos
[
  {
    "step_id": "S1",
    "order": 1,
    "intent": "打开百度首页",
    "result_reference": "#E1",
    "dependencies": [],
    "step_type": "fixed",         // "fixed" | "ai"
    // 关联的 trace
    "trace_id": "2026-07-30-001",
    "actions": [
      {
        "tool_name": "browser_navigate",
        "params": { "url": "https://baidu.com" },
        "description": "导航到百度首页"
      }
    ]
  },
  {
    "step_id": "S2",
    "order": 2,
    "intent": "输入搜索关键词",
    "result_reference": "#E2",
    "dependencies": ["#E1"],
    "step_type": "fixed",
    "trace_id": "2026-07-30-002",
    "actions": [
      {
        "tool_name": "browser_fill",
        "params": { "selector": "#kw", "text": "{{搜索关键词}}" },
        "description": "在搜索框输入关键词"
      },
      {
        "tool_name": "browser_click",
        "params": { "selector": "#su" },
        "description": "点击搜索按钮"
      }
    ]
  },
  {
    "step_id": "S3",
    "order": 3,
    "intent": "从结果中提取新闻标题",
    "result_reference": "#E3",
    "dependencies": ["#E2"],
    "step_type": "ai",            // 灵活模式：此步仍需要 AI
    "trace_id": null,
    "actions": []
  }
]
```

**字段说明**：

| 字段 | 类型 | 说明 |
|------|------|------|
| `step_id` | string | 步骤唯一标识 |
| `step_type` | `"fixed"` \| `"ai"` | `fixed` = 纯工具调用可回放；`ai` = 需要 LLM |
| `trace_id` | string? | 关联的 ExecutionTrace ID；null 表示无成熟 trace |
| `actions` | array | 具体的工具调用序列（`fixed` 时有内容，`ai` 时为空） |

### 4.3 回放逻辑

```
回放计划时遍历 todos:
  if step_type == "fixed":
    → 按 actions 直接调用工具（零 AI）
    → 如果执行失败（如 selector 失效）→ 降级为 ai 模式重探
  if step_type == "ai":
    → 调用 ReAct Agent（带 trace 作为 reference）
    → 成功后 → 记录新 trace → 评估是否可升级为 fixed
```

### 4.4 渐进式固化流程

```
灵活计划 (step_type: ai)
  │
  │  执行 N 次，每次产生新 trace
  ├── trace-001.json → iterations: 5  (不稳定)
  ├── trace-002.json → iterations: 3  (趋于稳定)
  └── trace-003.json → iterations: 1  (已稳定)
  │
  │  分析: 连续 3 次迭代 ≤ 2 → 可固化
  ▼
更新 plans.todos:
  step_type: "ai" → "fixed"
  trace_id:  "trace-003"
  actions:   从 trace 中提取的工具调用序列（填充用户确认的具体值）
```

### 4.5 版本管理（远期）

当页面改版导致 selector 失效时：

```
plans.todos 增加 version 字段:
  {
    "version": 1,
    "steps": [...]
  }

失败恢复:
  固化计划执行失败 → 自动切换灵活模式重探
  → 生成新 version 的 steps
  → 旧 version 保留（用于对比、回滚）
```

---

## 5. 与现有模块的关系

| 模块 | 关系 |
|------|------|
| `CoarseGrainedPlan` | 提供计划骨架（steps 的 intent + 依赖） |
| `ExecutionTrace` | 提供每一步的执行模板（tools + params），存储在 `traces/` |
| `PlanAndExecuteAgent` | 执行时产生 trace；成功后写入 `plans.todos` |
| `PlanTodoView` (GUI) | 读取 `plans.todos` 渲染执行计划 UI（当前仅 mock） |
| `plan_repo` | 扩展 `update_todos()` 方法，写入结构化的步骤数据 |
| `message_repo` | 对话历史保持现有逻辑不变 |

---

## 6. 实现路线

### 阶段 1：计划内容持久化（当前目标）

- [ ] 定义 `plans.todos` 的 JSON schema（如 4.2 节所示）
- [ ] `PlanRepo` 新增 `update_todos(plan_id, todos_json)` 方法
- [ ] 计划生成后（周密：确认生成；灵活：轨迹总结后），将步骤写入 `todos`
- [ ] `PlanTodoView` 从 `todos` 读取真实数据渲染，替换 mock

### 阶段 2：固化脚本回放

- [ ] 实现回放引擎：遍历 `fixed` 步骤直接调工具，`ai` 步骤走 ReAct
- [ ] 执行失败降级：`fixed` 步骤失败 → 自动切换 `ai` 模式重探
- [ ] 渐进式固化判断逻辑：连续 N 次低迭代 → 升级为 `fixed`

### 阶段 3：版本管理与调度

- [ ] todos 增加 `version` 字段
- [ ] 计划回滚能力
- [ ] 定时执行调度器

---

## 7. 待决策项

| # | 决策点 | 选项 A | 选项 B |
|---|--------|--------|--------|
| 1 | 步骤内容存哪里 | 复用 `plans.todos` JSON 列（简单，不改 schema） | 新增 `plan_steps` 表（规范，可查询步骤状态） |
| 2 | trace 关联方式 | `todos` JSON 中内嵌 actions（自包含） | `todos` 仅存 trace_id，回放时读 `traces/` 文件 |
| 3 | 固化阈值 | 连续 3 次迭代 ≤ 2 → fixed | 可配置（不同步骤类型不同策略） |
