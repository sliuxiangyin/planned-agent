# Planner 模块待办事项

> 最后更新：2026-07-30
>
> 基于 planner 模块全面审查得出的完善清单，按优先级排列。

---

## 🔴 高优先级：核心功能缺失

### 1. 实现 Replanner（重规划器）

**当前状态**：`core` 中已定义完整的 `ReplanningRequest`、`ReplanningResponse`、`ReplanningContext` 类型（[`core/src/planner/replanner/replanner_types.rs`](../../crates/core/src/planner/replanner/replanner_types.rs)），但 `planned-agent` 里完全没有实现。

**问题**：`PlanAndExecuteAgent` 遇到步骤失败直接 `break` 终止流水线，无法动态调整计划。

**建议实现**：在 `react/` 下新建 `replanner.rs`，核心逻辑：

```
步骤失败 → 分析失败原因 → 决定行动:
  - Continue:       跳过失败步骤，继续执行后续
  - UpdatePlan:     重新生成剩余步骤（拆分/合并/替换工具）
  - Abort:          不可恢复，终止流水线
  - RequestUserInput: 信息不足，请求用户补充
```

**涉及文件**：
- 新建 `crates/planned-agent/src/planner/react/replanner.rs`
- 修改 `plan_execute_agent.rs`：失败时调用 replanner 而非直接 break
- 参考类型：`core/src/planner/replanner/replanner_types.rs`

---

### 2. 实现 tool_result_handler（工具结果后处理）

**当前状态**：`react/tool_result_handler/` 目录为空，文档中提到需要 `tool_result_router.process()` 执行链式处理，但未实现。

**问题**：工具返回的原始数据可能包含 HTML 标签、二进制垃圾、超长文本，需要统一的清洗管道。

**建议实现**：三步处理链：

```
原始工具输出
  → StructureClean:   去除非结构化噪声（控制字符、日志前缀）
  → HtmlClean:        调用 html_clean_subagent 清洗 HTML
  → BinaryTruncate:   检测并截断二进制/Base64 内容
  → 返回清洗后结果
```

**涉及文件**：
- 新建 `crates/planned-agent/src/planner/react/tool_result_handler/mod.rs`

---

### 3. 升级 Plan Validation（计划校验）

**当前状态**：`core/src/planner/validation/validation_types.rs` 定义了丰富的验证类型（`CircularDependency`、`MissingDependency`、`ToolNotFound` 等），但 `LlmCoarsePlanner.validate_coarse_plan()` 只做了最基础的字符串检查，未使用这些类型。

**建议补充**：
- 循环依赖检测（DFS 拓扑排序）
- 步骤顺序与依赖的一致性校验
- `recommended_tool_categories` 与 `ToolRegistry` 交叉校验
- 返回结构化的 `PlanValidationResult`（含 `errors` 和 `warnings`）

**涉及文件**：
- 修改 `crates/planned-agent/src/planner/coarse/llm_planner.rs`：`validate_coarse_plan` 方法
- 参考类型：`core/src/planner/validation/validation_types.rs`

---

## 🟡 中优先级：健壮性与工程质量

### 4. ReAct Agent 单元测试

**当前状态**：`coarse/llm_planner.rs` 有完善的 mock 测试和 E2E 测试，但 `default_react_agent.rs`（607 行）零测试覆盖。

**建议覆盖**：
- 单工具调用后 LLM 返回 DONE → 步骤成功完成
- 多 tool_call 顺序执行 → 每个 tool_call 构造正确的 Tool 消息
- 连续 3 次相同 action → 循环检测触发终止
- 超时触发 → 返回 timeout 错误
- `observe` 判定 `is_out_of_scope` → scope warning 注入 messages
- 达到 `max_iterations` → 返回失败

**涉及文件**：
- 修改 `crates/planned-agent/src/planner/react/default_react_agent.rs`：添加 `#[cfg(test)] mod tests`

---

### 5. 统一重试与配置生效

**当前状态**：
- `ReActAgentConfig.retry_delay_ms` 预留字段，未在任何重试逻辑中使用
- `ReActAgentConfig.max_retries` 仅在 `ai_process` 中硬编码使用，未读配置值
- LLM 调用无重试机制

**建议**：
- 在 `tool_executor.rs` 中为通用工具调用添加可配置重试
- 在 LLM 调用辅助函数中添加指数退避重试（网络超时、速率限制）
- 统一读取 `ReActAgentConfig` 中的重试参数

**涉及文件**：
- 修改 `crates/planned-agent/src/planner/react/tool_executor.rs`
- 修改 `crates/planned-agent/src/planner/react/default_react_agent.rs`

---

### 6. 消息上下文膨胀控制

**当前状态**：ReAct 循环多轮时 `AgentContext.messages` 持续增长，无截断机制，可能超出模型 token 限制。

**建议**：
- 添加简单的 token 计数估算（`字符数 / 4` 或集成 `tiktoken-rs`）
- 超过阈值（如 8000 tokens）时自动截断：
  - 保留 System prompt
  - 保留最近 N 轮完整历史（N 可配置，默认 3）
  - 中间轮次替换为摘要
- 工具输出超长时强制走 chunk 路径

**涉及文件**：
- 修改 `crates/planned-agent/src/planner/react/agent_context.rs`
- 修改 `crates/planned-agent/Cargo.toml`：可能需要添加 token 计数依赖

---

## 🟢 低优先级：体验与扩展

### 7. 流水线完成后的结果合成

**当前状态**：`PlanAndExecuteAgent.execute()` 完成后只返回 `HashMap<String, StepResult>`，没有对全部结果做最终摘要。

**建议**：增加可选的 `synthesize` 步骤，调用 LLM 将所有步骤结果整合为面向用户的最终答案。

**涉及文件**：
- 修改 `crates/planned-agent/src/planner/react/plan_execute_agent.rs`

---

### 8. 并行步骤执行（可选）

**当前状态**：步骤严格串行执行。当多个步骤 `dependencies` 为空时理论上可并行。

**建议**：使用 `tokio::join!` 或 `FuturesUnordered` 并行执行无依赖步骤。注意这需要 `DefaultReActAgent` 支持 `Clone` 或多实例。

**涉及文件**：
- 修改 `crates/planned-agent/src/planner/react/plan_execute_agent.rs`

---

### 9. Sub-agent 扩展

**当前状态**：只有 `html_clean_subagent` 一个子 Agent。

**后续可按需添加**：
- `data_analysis_subagent`：结构化数据（JSON/CSV）统计分析
- `code_execution_subagent`：沙箱代码执行与结果捕获

---

### 10. 可观测性提升

**当前状态**：仅使用 `info!`/`warn!`/`error!` 宏记录日志，无结构化 metrics。

**建议**：
- 为关键操作添加 `tracing` span（`#[tracing::instrument]`）
- 收集 metrics：每步平均迭代次数、工具调用成功率、LLM 延迟分布
- 在 `ReActExecutionResult` 中携带更多诊断信息

**涉及文件**：
- 修改 `default_react_agent.rs`、`plan_execute_agent.rs`、`tool_executor.rs`

---

### 11. Coarse Planner 原子动作检查升级

**当前状态**：`validate_atomic_steps` 基于中文关键词启发式检查，可能误报/漏报。

**建议**：
- 增加英文连接词检测（and, then, also）
- 将检查结果从 `debug!` 升级为可配置的 warning/error 级别
- 可选：用小模型二次校验步骤原子性

**涉及文件**：
- 修改 `crates/planned-agent/src/planner/coarse/llm_planner.rs`

---

### 12. 配置外部化

**当前状态**：temperature（0.3）、max_tokens（4000/8000）硬编码在多处。

**建议**：将这些参数提取到 `ReActAgentConfig` 或新增 `PlannerConfig`，支持从 `config.toml` 读取。

---

## 进度跟踪

| # | 事项 | 优先级 | 状态 | 负责人 |
|---|------|--------|------|--------|
| 1 | Replanner 实现 | 🔴 高 | ⬜ 待开始 | |
| 2 | tool_result_handler | 🔴 高 | ⬜ 待开始 | |
| 3 | Plan Validation 升级 | 🔴 高 | ⬜ 待开始 | |
| 4 | ReAct Agent 测试 | 🟡 中 | ⬜ 待开始 | |
| 5 | 统一重试配置 | 🟡 中 | ⬜ 待开始 | |
| 6 | 消息上下文截断 | 🟡 中 | ⬜ 待开始 | |
| 7 | 结果合成 | 🟢 低 | ⬜ 待开始 | |
| 8 | 并行步骤执行 | 🟢 低 | ⬜ 待开始 | |
| 9 | Sub-agent 扩展 | 🟢 低 | ⬜ 待开始 | |
| 10 | 可观测性 | 🟢 低 | ⬜ 待开始 | |
| 11 | 原子动作检查升级 | 🟢 低 | ⬜ 待开始 | |
| 12 | 配置外部化 | 🟢 低 | ⬜ 待开始 | |
