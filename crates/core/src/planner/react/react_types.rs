use serde::{Deserialize, Serialize};
use serde_json::Value;

/// ReAct 步骤
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReActStep {
    /// 思考
    pub thought: Thought,
    /// 行动
    pub action: Action,
    /// 观察
    pub observation: Observation,
}

/// 思考
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thought {
    /// 推理过程
    pub reasoning: String,
    /// 下一步计划
    pub plan: String,
    /// 置信度 (0.0 - 1.0)
    pub confidence: f32,
}

/// 行动
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    /// 工具名称
    pub tool_name: String,
    /// 工具参数
    pub parameters: Value,
    /// 选择该行动的理由
    pub reasoning: String,
}

/// 观察
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    /// 工具输出
    pub output: Value,
    /// 是否完成目标
    pub is_complete: bool,
    /// 错误信息（如果有）
    pub error: Option<String>,
    /// 执行时长（毫秒）
    pub duration_ms: u64,
}

/// ReAct Agent 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReActAgentConfig {
    /// 最大迭代次数
    pub max_iterations: usize,
    /// 单步超时时间（毫秒）
    pub step_timeout_ms: u64,
    /// 是否启用思考链
    pub enable_chain_of_thought: bool,
    /// 失败重试次数
    pub max_retries: u32,
    /// 重试延迟（毫秒）
    pub retry_delay_ms: u64,
}

impl Default for ReActAgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            step_timeout_ms: 30000,
            enable_chain_of_thought: true,
            max_retries: 3,
            retry_delay_ms: 1000,
        }
    }
}

/// Observe 方法返回结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserveResult {
    /// 是否完成目标
    pub is_complete: bool,
    /// 分析说明
    pub summary: String,
}

/// ReAct 执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReActExecutionResult {
    /// 步骤ID
    pub step_id: String,
    /// 是否成功
    pub success: bool,
    /// 最终输出（原始工具输出）
    pub output: Value,
    /// 错误信息（如果有）
    pub error: Option<String>,
    /// 执行历史
    pub history: Vec<ReActStep>,
    /// 总迭代次数
    pub iterations: usize,
    /// 总执行时长（毫秒）
    pub total_duration_ms: u64,
}

impl ReActExecutionResult {
    /// 创建成功的执行结果
    pub fn success(step_id: String, output: Value, history: Vec<ReActStep>, iterations: usize, total_duration_ms: u64) -> Self {
        Self {
            step_id,
            success: true,
            output,
            error: None,
            history,
            iterations,
            total_duration_ms,
        }
    }

    /// 创建失败的执行结果
    pub fn failure(step_id: String, error: String, history: Vec<ReActStep>, iterations: usize, total_duration_ms: u64) -> Self {
        Self {
            step_id,
            success: false,
            output: Value::Null,
            error: Some(error),
            history,
            iterations,
            total_duration_ms,
        }
    }
}
