# 执行轨迹 RAG 记忆系统 设计文档

> 版本：v1.0
> 创建：2026-07-30
> 状态：设计中

---

## 1. 概述

### 1.1 目标

将每个粗粒度步骤的成功执行过程（ReAct 工具调用序列）记录为可检索的操作模板，存入向量数据库。后续遇到相似步骤时，检索历史成功轨迹，注入 System Prompt，让 LLM 参照已验证的流程执行，仅替换参数值，大幅降低摸索成本。

### 1.2 核心价值

```
传统 ReAct: 每步从零摸索 → 平均 3~5 轮迭代
RAG 增强:   直接参照模板 → 平均 1~2 轮完成
```

### 1.3 一句话描述

**把 ReAct Agent 的成功经验沉淀为可检索的"操作模板"，通过多维锚定 + LLM 泛化实现跨任务复用。**

---

## 2. 整体架构

### 2.1 数据流

```
┌──────────────────────────────────────────────────────────────────┐
│                     PlanAndExecuteAgent                           │
│                                                                  │
│  Step 执行成功                                                    │
│    │                                                             │
│    ▼                                                             │
│  TraceRecorder                                                    │
│    │  原始 ReActStep[] ──→ LLM 泛化 ──→ ExecutionTrace           │
│    │  (含具体值)            (小模型)     (含 {{变量}} 模板)        │
│    │                                      │                      │
│    ▼                                      ▼                      │
│  TraceStore ──→ embedding ──→ 向量库                              │
│                                                                  │
│  ════════════════════════════════════════════════════════════     │
│                                                                  │
│  新 Step 到来                                                     │
│    │                                                             │
│    ▼                                                             │
│  TraceRetriever                                                   │
│    │  多维检索 Query (intent + upstream + categories)             │
│    │    │                                                        │
│    ▼    ▼                                                        │
│  粗筛: tool_categories 交集非空                                   │
│  精排: embedding cosine top_k                                     │
│    │                                                             │
│    ▼                                                             │
│  泛化轨迹模板 → 注入 react_system Prompt                          │
│    │                                                             │
│    ▼                                                             │
│  DefaultReActAgent 参照模板执行                                    │
└──────────────────────────────────────────────────────────────────┘
```

### 2.2 模块依赖

| 模块 | 位置 | 职责 |
|------|------|------|
| `ExecutionTrace` 类型 | `core/src/planner/trace_types.rs` | 核心数据结构 |
| `TraceStore` trait | `core/src/planner/trace_trait.rs` | 存储接口抽象 |
| `InMemoryTraceStore` | `planned-agent/src/planner/trace/memory_store.rs` | MVP 内存实现 |
| `TraceRecorder` | `planned-agent/src/planner/trace/recorder.rs` | 轨迹记录 + LLM 泛化 |
| `TraceRetriever` | `planned-agent/src/planner/trace/retriever.rs` | 多维检索 |
| `TraceInjector` | `planned-agent/src/planner/trace/injector.rs` | Prompt 模板注入 |

---

## 3. 核心数据结构

### 3.1 ExecutionTrace（存储单元）

一条 ExecutionTrace = 一个粗粒度步骤的成功执行模板。

```rust
/// 执行轨迹（泛化后的可复用模板）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTrace {
    /// 轨迹唯一标识
    pub id: String,

    // ── 检索锚定字段 ──
    /// 泛化后的意图描述（用于 embedding + 语义检索）
    /// 例如："在搜索引擎输入框中输入{{搜索关键词}}并执行搜索"
    pub generalized_intent: String,

    /// 原始意图（保留用于调试和日志）
    pub original_intent: String,

    /// 前序步骤的泛化意图（锚定上游链路，避免歧义匹配）
    /// 例如："打开{{搜索引擎}}首页"
    pub upstream_generalized_intent: Option<String>,

    /// 工具类别（硬过滤维度）
    /// 检索时仅从 tool_categories 交集非空的轨迹池中搜索
    pub tool_categories: Vec<ToolCategory>,

    /// 所属计划的标题（辅助理解上下文，非检索用）
    pub plan_title: String,

    // ── 注入 Prompt 的操作模板 ──
    /// 泛化后的工具调用序列
    pub actions: Vec<GeneralizedAction>,

    // ── 质量标记 ──
    /// 总迭代次数（越小 = 轨迹质量越高）
    pub total_iterations: usize,
    /// 总执行耗时（毫秒）
    pub total_duration_ms: u64,
    /// 记录时间
    pub recorded_at: DateTime<Utc>,
}

/// 泛化后的单个工具调用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralizedAction {
    /// 工具名称
    pub tool_name: String,
    /// 泛化参数模板（变量替换为 {{变量名}}）
    /// 例如：{"selector": "#kw", "text": "{{搜索关键词}}"}
    pub params_template: Value,
    /// 原始参数（保留用于调试）
    pub original_params: Value,
    /// 步骤说明（注入 Prompt 时展示）
    pub description: String,
}
```

### 3.2 检索条件

```rust
/// 多维检索条件
#[derive(Debug, Clone)]
pub struct SearchQuery {
    /// 当前步骤的原始 intent（用于 embedding）
    pub intent: String,
    /// 当前步骤的工具类别（硬过滤：交集为空则不搜索）
    pub tool_categories: Vec<ToolCategory>,
    /// 前序步骤的 intent（锚定上游）
    pub upstream_intent: Option<String>,
    /// 最大返回条数
    pub limit: usize,
}

/// 检索结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// 匹配的轨迹
    pub trace: ExecutionTrace,
    /// 语义相似度 (0.0 ~ 1.0)
    pub similarity: f32,
}
```

### 3.3 Prompt 注入上下文

```rust
/// 注入 System Prompt 的轨迹提示变量
#[derive(Debug, Clone)]
pub struct TraceInjection {
    /// 是否有可用轨迹
    pub has_reference_traces: bool,
    /// 格式化后的参考流程文本
    pub reference_traces_text: String,
    /// 匹配的轨迹条数
    pub matched_count: usize,
    /// 最高相似度
    pub max_similarity: f32,
}
```

---

## 4. 多维检索锚定

### 4.1 问题

单个 intent 文本做语义检索会产生歧义漂移：

| 当前步骤 intent | 可能错误匹配到 |
|----------------|--------------|
| 整理搜索结果 | 整理文件列表、整理数据库查询结果、整理 API 响应 |
| 提取关键信息 | 提取日志关键信息、提取网页关键信息、提取文档关键信息 |

### 4.2 三维锚定策略

```
检索 Query = intent + tool_categories + upstream_intent
                │            │                │
                ▼            ▼                ▼
           语义匹配      硬过滤            软约束
```

#### 维度 1：Tool Categories（硬过滤）

**仅从 tool_categories 有交集的轨迹池中检索**。

```
当前步骤 categories = [Browser, Data]
  → 只搜索 categories 包含 Browser 或 Data 的轨迹
  → 排除纯 File / System / Dev 轨迹
```

#### 维度 2：Upstream Intent（前序锚定）

**存储时记录上游泛化 intent，检索时参与 embedding**。

```
步骤 B "提取搜索结果":   前序是 A "搜索关键词" → 匹配到"搜索后提取"模式
步骤 B "提取文件内容":   前序是 A "读取文件"   → 匹配到"读取后提取"模式
```

虽然都叫"提取"，但不同的上游语境让它们在向量空间中远离彼此。

#### 维度 3：Intent Embedding（语义精排）

**在硬过滤后的池子里，用 embedding cosine 做精排**。

### 4.3 检索 Query 构造

```rust
fn build_search_query(
    step: &CoarseGrainedStep,
    prev_steps: &[CoarseGrainedStep],
) -> String {
    let mut parts = vec![];
    parts.push(format!("意图: {}", step.intent));
    
    // 工具类别锚定
    if let Some(cats) = &step.recommended_tool_categories {
        parts.push(format!(
            "工具类别: {}",
            cats.iter().map(|c| c.description()).collect::<Vec<_>>().join(", ")
        ));
    }
    
    // 前序步骤锚定
    if let Some(prev) = prev_steps.last() {
        parts.push(format!("前序步骤: {}", prev.intent));
    }
    
    parts.join("\n")
}
```

### 4.4 匹配效果示例

| 当前步骤 | 前序步骤 | Tool Categories | 检索命中 |
|---------|---------|----------------|---------|
| 整理搜索结果 | 在百度搜索 X | [Browser, Data] | ✅ 搜索引擎 → 整理结果 |
| 整理搜索结果 | 用 Google 搜索 X | [Browser, Data] | ✅ 搜索引擎 → 整理结果（跨搜索引擎复用） |
| 整理文件列表 | 读取目录 | [File] | ❌ 类别不匹配，不会检索 |
| 提取网页文本 | 打开某 URL | [Browser] | ❌ 类别交集为空 |

---

## 5. LLM 泛化

### 5.1 为什么需要泛化

存储的原始轨迹包含具体值（"安仁乡"、"https://baidu.com"、"#kw"），LLM 容易照抄而非替换。必须在存储前做泛化。

### 5.2 泛化时机：记录时而非检索时

| 时机 | 做法 | 问题 |
|------|------|------|
| 检索时泛化 | 搜到原始轨迹 → 调 LLM 泛化 → 注入 Prompt | 每次检索多一次 LLM API 调用，延迟翻倍 |
| **记录时泛化** ✅ | 成功后调小模型泛化 → 存泛化版本 → 检索直接用 | 一次调用，永久复用 |

### 5.3 泛化 Prompt

```
你是一个流程模板提取器。请将以下成功执行的操作流程转化为可复用的模板。

规则：
1. 将具体实例数据（人名、地名、关键词、URL、数字、文件名）替换为 {{变量名}} 占位符
2. 保留工具名称、selector、API 路径等结构性字段不变
3. 保留工具调用顺序不变
4. 为每个 {{变量}} 添加中文含义说明
5. 只输出 JSON，不要包含任何其他文字

输入意图：在百度搜索框输入'安仁乡'并执行搜索

工具调用序列：
1. browser_snapshot → 获取页面结构
2. browser_type(selector="#kw", text="安仁乡") → 输入搜索关键词
3. browser_click(selector="#su") → 点击搜索按钮
4. browser_snapshot → 获取搜索结果页面

输出 JSON：
{
  "generalized_intent": "在搜索引擎输入框中输入{{搜索关键词}}并执行搜索",
  "actions": [
    {
      "tool_name": "browser_snapshot",
      "params_template": {},
      "description": "获取页面结构"
    },
    {
      "tool_name": "browser_type",
      "params_template": {"selector": "#kw", "text": "{{搜索关键词}}"},
      "description": "在搜索框中输入关键词"
    },
    {
      "tool_name": "browser_click",
      "params_template": {"selector": "#su"},
      "description": "点击搜索按钮"
    },
    {
      "tool_name": "browser_snapshot",
      "params_template": {},
      "description": "获取搜索结果页面"
    }
  ]
}
```

### 5.4 泛化实现要点

- **使用独立的小模型调用**（temperature=0，max_tokens=2000），不影响主 ReAct 上下文
- **复用现有 AiClient**，不走 PromptManager（避免污染 planning 模板空间）
- **泛化失败不阻塞主流程**：出错时退化为轻量规则泛化（正则替换引号字符串 + 数字），确保轨迹至少能以原始形式存下来
- **结果验证**：检查 `generalized_intent` 是否含 `{{...}}` 占位符，不含 → 规则兜底

---

## 6. Trait 接口设计

### 6.1 TraceStore（存储抽象）

```rust
#[async_trait]
pub trait TraceStore: Send + Sync {
    /// 存入一条泛化后的执行轨迹
    async fn record(&self, trace: ExecutionTrace) -> Result<()>;

    /// 按多维条件检索最相似的 N 条轨迹
    async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>>;

    /// 删除低质量轨迹
    /// - max_iterations: 迭代次数超过此值的轨迹视为低质量
    /// - 返回被删除的轨迹 ID 列表
    async fn prune(&self, max_iterations: usize) -> Result<Vec<String>>;

    /// 统计信息
    async fn stats(&self) -> Result<TraceStoreStats>;

    /// 获取轨迹总数
    async fn count(&self) -> Result<usize>;
}
```

### 6.2 TraceRecorder（记录器）

```rust
#[async_trait]
pub trait TraceRecorder: Send + Sync {
    /// 记录一个成功执行步骤的轨迹
    /// 
    /// 内部流程:
    /// 1. 从 ReActStep[] 提取工具调用序列
    /// 2. 调用 LLM 做泛化
    /// 3. 构建 ExecutionTrace
    /// 4. 存入 TraceStore
    async fn record_successful_step(
        &self,
        step_id: String,
        intent: &str,
        tool_categories: &[ToolCategory],
        upstream_intent: Option<&str>,
        plan_title: &str,
        history: &[ReActStep],
        total_iterations: usize,
        total_duration_ms: u64,
    ) -> Result<String>; // 返回 trace_id
}
```

### 6.3 TraceRetriever（检索器）

```rust
#[async_trait]
pub trait TraceRetriever: Send + Sync {
    /// 检索相似轨迹
    async fn retrieve(
        &self,
        query: &SearchQuery,
        min_similarity: f32,
        limit: usize,
    ) -> Result<Vec<SearchResult>>;

    /// 构建 Prompt 注入上下文
    fn build_injection(
        results: &[SearchResult],
        min_similarity: f32,
    ) -> TraceInjection;
}
```

---

## 7. 集成点

### 7.1 记录时机（PlanAndExecuteAgent）

在 `PlanAndExecuteAgent.execute()` 中，每个步骤成功后记录：

```rust
// plan_execute_agent.rs 伪代码
for (i, step) in coarse_plan.steps.iter().enumerate() {
    let result = react_agent.execute_coarse_step(step, &step_context).await?;

    if result.success {
        // ✅ 记录成功轨迹
        let prev_intent = if i > 0 {
            Some(coarse_plan.steps[i - 1].intent.as_str())
        } else {
            None
        };

        self.trace_recorder.record_successful_step(
            step.id.clone(),
            &step.intent,
            step.recommended_tool_categories.as_deref().unwrap_or(&[]),
            prev_intent,
            &coarse_plan.title,
            &result.history,
            result.iterations,
            result.total_duration_ms,
        ).await?;
    }
}
```

### 7.2 检索注入时机（DefaultReActAgent）

在 `init_messages()` 中，渲染 System Prompt 之前：

```rust
// default_react_agent.rs 伪代码
async fn init_messages(&mut self, coarse_step: &CoarseGrainedStep, context: &PlanContext) {
    // 1. 检索相似轨迹
    let query = SearchQuery {
        intent: coarse_step.intent.clone(),
        tool_categories: coarse_step.recommended_tool_categories.clone().unwrap_or_default(),
        upstream_intent: get_previous_step_intent(context),
        limit: 3,
    };

    let results = self.trace_retriever.retrieve(&query, 0.7, 3).await?;
    let injection = TraceRetriever::build_injection(&results, 0.7);

    // 2. 构建 Prompt 上下文（含轨迹注入）
    let prompt_context = PromptContext::new()
        .with_variable("has_reference_traces", json!(injection.has_reference_traces))
        .with_variable("reference_traces", json!(injection.reference_traces_text))
        // ... 其他变量
        ;

    // 3. 渲染 System Prompt
    let system_prompt = self.prompt_manager.render("planning/react_system", &prompt_context).await?;
    // ...
}
```

### 7.3 Prompt 模板变更

`react_system.toml` 新增模板段：

```text
{% if has_reference_traces %}
【🔍 历史成功参考流程】
以下是与当前任务高度相似的历史成功执行记录，请严格参照工具调用顺序，
仅将 {{变量}} 替换为当前任务的实际值，完成后回复 DONE：

{{ reference_traces }}

⚠️ 注意：
- 必须保持工具调用顺序不变
- 只替换 {{变量}} 为实际值，不要修改 selector、API 路径等结构字段
- 如果当前页面的 selector 与模板不同，使用 snapshot 重新定位元素
{% endif %}
```

### 7.4 配置项

```toml
[trace_rag]
# 是否启用轨迹 RAG
enabled = true
# 泛化使用的小模型（独立调用，轻量即可）
generalization_model = "gpt-4o-mini"
# 检索最小相似度阈值
min_similarity = 0.7
# 检索返回最大条数
max_results = 3
# 入库质量门槛：迭代次数超过此值的轨迹不入库
max_iterations_for_record = 5
# 是否在记录时做 LLM 泛化（关闭则用规则泛化兜底）
use_llm_generalization = true
```

---

## 8. MVP 实现范围（Phase 1）

### 8.1 实现内容

| 模块 | 文件 | 说明 |
|------|------|------|
| 核心类型 | `core/src/planner/trace_types.rs` | ExecutionTrace、GeneralizedAction、SearchQuery、SearchResult |
| 存储 Trait | `core/src/planner/trace_trait.rs` | TraceStore、TraceRecorder、TraceRetriever |
| 内存实现 | `planned-agent/src/planner/trace/memory_store.rs` | InMemoryTraceStore（HashMap + 内存 cosine） |
| 嵌入生成 | `planned-agent/src/planner/trace/embedder.rs` | 复用 AiClient embed API |
| 轨迹记录器 | `planned-agent/src/planner/trace/recorder.rs` | TraceRecorder 实现（含 LLM 泛化 + 规则兜底） |
| 轨迹检索器 | `planned-agent/src/planner/trace/retriever.rs` | TraceRetriever 实现（多维检索） |
| 注入器 | `planned-agent/src/planner/trace/injector.rs` | Prompt 文本格式化 |
| 模块入口 | `planned-agent/src/planner/trace/mod.rs` | 统一导出 |
| 集成（PlanAndExecute） | 修改 `plan_execute_agent.rs` | 成功后调用 recorder |
| 集成（DefaultReAct） | 修改 `default_react_agent.rs` | init_messages 中检索注入 |
| 模板变更 | 修改 `prompts/planning/react_system.toml` | 新增 has_reference_traces 条件段 |

### 8.2 MVP 简化点

- **不使用外部向量库**：纯内存 HashMap + 内存 cosine（手动计算）
- **Embedding 维度**：复用 AiClient embed API（`text-embedding-3-small`，1536 维）
- **不持久化**：会话内记忆，重启丢失
- **LLM 泛化失败**：规则泛化自动兜底（正则替换）

### 8.3 目录结构

```
crates/
├── core/src/planner/
│   ├── trace_types.rs              # 新增
│   └── trace_trait.rs              # 新增
└── planned-agent/src/planner/
    ├── trace/                      # 新增目录
    │   ├── mod.rs
    │   ├── memory_store.rs         # InMemoryTraceStore
    │   ├── embedder.rs             # Embedding 封装
    │   ├── recorder.rs             # TraceRecorder 实现
    │   ├── retriever.rs            # TraceRetriever 实现
    │   └── injector.rs             # Prompt 格式化
    └── react/
        ├── default_react_agent.rs  # 修改：注入检索逻辑
        └── plan_execute_agent.rs   # 修改：成功后记录轨迹
```

---

## 9. 扩展路线（Phase 2/3）

### Phase 2：验证后增强

- LLM 泛化专有 Prompt 优化
- JSON 文件持久化（重启保留轨迹）
- 轨迹去重：相同 `generalized_intent + 相同 tool_categories` → 保留迭代数最小的
- 定期裁剪：`prune(max_iterations=10)` 自动清理低质量轨迹

### Phase 3：生产化

- 外部向量库（Qdrant / LanceDB）
- 跨会话累积学习
- 轨迹可视化（前端查看成功模板）
- 轨迹效果追踪（使用模板后迭代次数 vs 不使用，量化收益）

---

## 10. 风险与对策

| 风险 | 对策 |
|------|------|
| LLM 泛化不准确，变量识别错误 | 规则泛化兜底；泛化结果校验（必须含 `{{}}` 否则重试） |
| 轨迹质量差（迭代多但成功） | 入库门槛 `max_iterations_for_record`，默认 5 |
| 轨迹数量膨胀 | 定期 `prune`；去重合并 |
| selector 失效（网站改版） | 泛化模板中保留"用 snapshot 定位"提示；LLM 执行时自适应 |
| 过度依赖模板导致泛化能力退化 | 模板仅作"建议"，System Prompt 强调"以实际页面为准" |
| Embedding API 额外成本 | MVP 使用轻量模型（`text-embedding-3-small`）；轨迹少时成本极低 |
