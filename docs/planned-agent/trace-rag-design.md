# 执行轨迹记忆系统 设计文档

> 版本：v2.0
> 创建：2026-07-30
> 状态：设计中

---

## 1. 概述

### 1.1 目标

将粗粒度步骤的成功执行过程（ReAct 工具调用序列）通过 LLM 泛化为可复用的操作模板。后续遇到相似步骤时，参照已验证的流程执行，仅替换参数值，大幅降低摸索成本。

### 1.2 核心价值

```
传统 ReAct: 每步从零摸索 → 平均 3~5 轮迭代
模板复用:   直接参照模板 → 平均 1~2 轮完成
```

### 1.3 一句话描述

**把 ReAct Agent 的成功经验通过 LLM 泛化为"操作模板"，实现跨任务复用。**

### 1.4 总体路线

```
Phase 1（当前）：记录 → LLM 泛化 → JSON 文件存储
Phase 2（后续）：嵌入 → 检索 → System Prompt 注入
```

Phase 1 先验证"泛化 → 复用"这个核心假设是否成立，不引入检索系统的复杂度。

---

## 2. 整体架构

### 2.1 Phase 1 数据流

```
┌──────────────────────────────────────────────────────────┐
│                  PlanAndExecuteAgent                      │
│                                                          │
│  Step 执行成功                                            │
│    │                                                     │
│    ▼                                                     │
│  收集数据：StepResult { intent, history (ReActStep[]) }    │
│    │                                                     │
│    ▼                                                     │
│  TraceRecorder                                            │
│    │  (intent + 工具调用序列) ──→ LLM 泛化                │
│    │  "在百度搜索安仁乡"                        │
│    │  browser_type(text="安仁乡")                │
│    │         ↓ LLM 替换具体值                                │
│    │  "在搜索引擎搜索{{关键词}}"                        │
│    │  browser_type(text="{{关键词}}")              │
│    │                                                     │
│    ▼                                                     │
│  ExecutionTrace ──→ JSON 文件 (traces/xxx.json)           │
└──────────────────────────────────────────────────────────┘
```

### 2.2 Phase 1 数据够用吗？

**够用。** 现有的 `StepResult` 已包含泛化所需的全部输入：

```rust
// plan_execute_agent.rs - StepResult
pub struct StepResult {
    pub step_id: String,
    pub intent: String,           // ✅ "在百度搜索安仁乡"
    pub history: Vec<ReActStep>,  // ✅ 完整工具调用序列
    pub iterations: usize,        // ✅ 质量门槛
    pub duration_ms: u64,
    // ...
}
```

`ReActStep` 中有 `Action { tool_name, parameters }`，可以完整还原每一步"调了什么工具、传了什么参数"。

### 2.3 模块清单（Phase 1）

| 模块 | 文件 | 职责 |
|------|------|------|
| `ExecutionTrace` | `core/src/planner/trace_types.rs` | 泛化后的轨迹数据结构 |
| `TraceRecorder` | `planned-agent/src/planner/trace/recorder.rs` | 收集数据 + LLM 泛化 + 存 JSON |
| `trace/mod.rs` | `planned-agent/src/planner/trace/mod.rs` | 模块入口 |
| 集成 | 修改 `plan_execute_agent.rs` | 步骤成功后调用 recorder |
| 配置 | 修改 `config.toml` | 新增 `[trace]` 配置段 |

---

## 3. 核心数据结构

### 3.1 ExecutionTrace（存储单元）

一条 ExecutionTrace = 一个步骤的泛化操作模板。存入 JSON 文件。

```rust
/// 执行轨迹（泛化后的可复用模板）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTrace {
    /// 轨迹唯一标识
    pub id: String,

    /// 原始意图（保留用于调试）
    pub original_intent: String,

    /// 泛化后的意图（{{变量}} 替换具体值）
    /// 例: "在百度搜索安仁乡" → "在搜索引擎搜索{{关键词}}"
    pub generalized_intent: String,

    /// 前序步骤的意图（用于后续检索锚定，Phase 2 用）
    pub upstream_intent: Option<String>,

    /// 泛化后的工具调用序列
    pub actions: Vec<GeneralizedAction>,

    // ── 质量标记 ──
    /// 总迭代次数（越小 = 质量越高；超过阈值不入库）
    pub total_iterations: usize,
    /// 总执行耗时（毫秒）
    pub total_duration_ms: u64,
    /// 记录时间
    pub recorded_at: String,
}

/// 泛化后的单个工具调用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralizedAction {
    /// 工具名称
    pub tool_name: String,
    /// 泛化参数（具体值 → {{变量}}）
    /// 例: {"selector": "#kw", "text": "{{关键词}}"}
    pub params: serde_json::Value,
    /// 原始参数（保留用于调试/回溯）
    pub original_params: serde_json::Value,
    /// 步骤说明
    pub description: String,
}
```

### 3.2 JSON 存储格式

```
traces/
├── 2026-07-30-001.json   # 每个成功步骤一个文件
├── 2026-07-30-002.json
└── ...
```

文件名格式：`{date}-{seq}.json`。每条 JSON 反序列化后即为 `ExecutionTrace`。

---

## 4. LLM 泛化

### 4.1 为什么需要泛化

原始轨迹包含具体值（"安仁乡"、"https://baidu.com"），不泛化的话下次搜索"天安门"无法复用。

**目标**：将具体实例值替换为 `{{变量名}}`，保留工具名称和选择器等结构字段。

### 4.2 泛化 Prompt

```
将以下操作流程中的具体实例值（人名、地名、关键词、URL、数字、文件名）
替换为 {{变量名}} 占位符，生成可复用的模板。

规则：
1. 只替换具体实例值（如"安仁乡"→{{关键词}}），结构字段（selector、API路径）不变
2. 工具调用顺序不变
3. 变量名用中文，描述其含义（如 {{关键词}}、{{文件路径}}）
4. 只输出 JSON

输入意图：在百度搜索框输入'安仁乡'并执行搜索

工具调用序列：
1. browser_snapshot(selector="#kw") → 获取页面结构
2. browser_type(selector="#kw", text="安仁乡") → 输入搜索关键词
3. browser_click(selector="#su") → 点击搜索按钮
4. browser_snapshot → 获取搜索结果页面

输出 JSON：
{
  "generalized_intent": "在搜索引擎输入框中输入{{搜索关键词}}并执行搜索",
  "actions": [
    {"tool_name": "browser_snapshot", "params": {}, "description": "获取页面结构"},
    {"tool_name": "browser_type", "params": {"selector": "#kw", "text": "{{搜索关键词}}"}, "description": "输入关键词"},
    {"tool_name": "browser_click", "params": {"selector": "#su"}, "description": "点击搜索"},
    {"tool_name": "browser_snapshot", "params": {}, "description": "获取搜索结果页面"}
  ]
}
```

### 4.3 实现要点

- **独立 LLM 调用**（temperature=0）：不影响主 ReAct 上下文
- **复用现有 AiClient**，不走 PromptManager
- **泛化失败兜底**：规则泛化（正则替换引号字符串 + 纯数字）
- **结果校验**：`generalized_intent` 不包含 `{{` → 视为失败，退化为规则兜底

### 4.4 规则泛化（兜底）

当 LLM 调用失败时，用正则做轻量泛化：

| 模式 | 替换为 |
|------|--------|
| `"安仁乡"` 等引号字符串（非 selector/API 路径值） | `{{值}}` |
| 纯数字参数（如 `max_tokens=4096`） | `{{数字}}` |
| URL（如 `https://baidu.com`） | `{{URL}}` |

---

## 5. Record 流程详解

### 5.1 触发时机

在 [PlanAndExecuteAgent.execute()](file:///home/code/planned-agent/crates/planned-agent/src/planner/react/plan_execute_agent.rs) 中，每个步骤成功后调用。

### 5.2 流程

```
1. 步骤执行成功 → 拿到 StepResult { intent, history }
2. 质量检查：iterations > max_iterations_for_record → 跳过
3. 从 history 提取工具调用序列：
   ReActStep.action → (tool_name, parameters)
4. 构造泛化 Prompt（intent + 工具调用序列）
5. 调用 LLM 泛化 → 拿到 ExecutionTrace
6. 校验：generalized_intent 含 {{ }} → 通过
7. 序列化为 JSON → 写入 traces/{date}-{seq}.json
```

### 5.3 伪代码

```rust
// plan_execute_agent.rs
for (i, step) in coarse_plan.steps.iter().enumerate() {
    let result = react_agent.execute_coarse_step(step, &step_context).await?;

    if result.success {
        // ✅ 记录成功轨迹
        let prev_intent = if i > 0 {
            Some(coarse_plan.steps[i - 1].intent.as_str())
        } else {
            None
        };

        // 质量门槛：迭代次数太多 = 不稳定，不记录
        if result.iterations <= self.config.max_iterations_for_record {
            self.trace_recorder.record_successful_step(
                &step.intent,
                prev_intent,
                &result.history,
                result.iterations,
                result.total_duration_ms,
            ).await?;  // 内部完成：LLM 泛化 → 校验 → 写 JSON
        }
    }
}
```

---

## 6. 配置项

```toml
[trace]
# 是否启用轨迹记录
enabled = true
# 轨迹存储目录
storage_dir = "./traces"
# 入库质量门槛：迭代次数超过此值的轨迹不入库
max_iterations_for_record = 5
# 是否使用 LLM 泛化（关闭则仅规则泛化）
use_llm_generalization = true
# 泛化使用的模型名称（为空则用默认 AI 提供商）
generalization_model = ""
```

---

## 7. Phase 1 实现清单

### 7.1 新增文件

| 文件 | 说明 |
|------|------|
| `core/src/planner/trace_types.rs` | `ExecutionTrace`、`GeneralizedAction` |
| `planned-agent/src/planner/trace/mod.rs` | 模块入口 |
| `planned-agent/src/planner/trace/recorder.rs` | `TraceRecorder`：收集 + LLM 泛化 + 规则兜底 + 写 JSON |

### 7.2 修改文件

| 文件 | 改动 |
|------|------|
| `core/src/planner/mod.rs` | 加 `pub mod trace_types;` |
| `planned-agent/src/planner/mod.rs` | 加 `pub mod trace;` |
| `planned-agent/src/planner/react/plan_execute_agent.rs` | 步骤成功后调用 recorder |
| `config.toml` | 新增 `[trace]` 配置段 |

### 7.3 Phase 1 不做的事

- ❌ 不嵌入（Embedding）
- ❌ 不检索（Retrieval）
- ❌ 不注入 System Prompt（Injection）
- ❌ 不引入外部向量库
- ❌ 不修改 `react_system.toml` Prompt 模板

以上是 Phase 2 的内容。

### 7.4 目录结构

```
crates/
├── core/src/planner/
│   └── trace_types.rs              # 新增：ExecutionTrace
└── planned-agent/src/planner/
    ├── trace/                      # 新增目录
    │   ├── mod.rs                  # 模块入口
    │   └── recorder.rs             # TraceRecorder
    └── react/
        └── plan_execute_agent.rs   # 修改：成功后记录
```

---

## 8. Phase 2 规划（后续）

Phase 1 验证"泛化模板能降低迭代次数"后，再实现：

### 8.1 检索与注入

```
新步骤到来 → 嵌入 intent → 向量检索相似模板 → 注入 System Prompt
                                                ↓
                              DefaultReActAgent 参照模板执行
```

### 8.2 新增模块

| 模块 | 职责 |
|------|------|
| `embedder.rs` | 文本嵌入生成（复用 BAAI/bge-m3） |
| `memory_store.rs` | 内存向量存储 + cosine 相似度检索 |
| `retriever.rs` | 多维检索（intent + categories + upstream） |
| `injector.rs` | Prompt 文本格式化注入 |
| 修改 `default_react_agent.rs` | `init_messages` 中检索注入 |
| 修改 `react_system.toml` | 新增 `{{ reference_traces }}` 条件段 |

### 8.3 Phase 3

- 外部向量库（Qdrant / LanceDB）
- 跨会话累积学习
- 轨迹效果追踪（使用模板前/后迭代次数对比）

---

## 9. 风险与对策

| 风险 | 对策 |
|------|------|
| LLM 泛化不准确，变量识别错误 | 规则泛化兜底；校验 `{{}}` 必须存在 |
| 轨迹质量差（迭代多但成功） | 入库门槛 `max_iterations_for_record`，默认 5 |
| 轨迹数量膨胀 | 后续 Phase 2 加入去重（相同 generalized_intent 保留最优） |
| selector 失效（网站改版） | 泛化模板保留 selector；执行时提示"以实际页面为准" |
| 泛化模板过于具体/抽象 | LLM 泛化 Prompt 持续迭代优化 |
