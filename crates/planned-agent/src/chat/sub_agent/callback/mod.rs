//! 子 agent 回调：启动前（before）与完成后（result）两套互不相交的 trait。
//!
//! # 分工
//!
//! - [`before`]：**只碰入参** —— 在 task 文本生成前注入系统侧数据，不改子 agent 输出；
//! - [`result`]：**只碰产物** —— 子 agent 已有输出之后的决策。
//!
//! 两者可独立启用，互不影响。
//!
//! # 目录
//!
//! ```text
//! callback/
//! ├── mod.rs     模块声明 + 重导出 + 共享上下文
//! ├── before.rs   启动前注入：BeforeDecision + SubAgentBeforeCallback
//! └── result.rs   完成后决策：ResultDecision + SubAgentChainPrelude +
//!                 SubAgentResultCallback + SubAgentResultChain
//! ```

mod before;
mod result;

pub use before::{BeforeDecision, SubAgentBeforeCallback};
pub use result::{
    PreludeOutcome, ResultDecision, SubAgentCall, SubAgentChainPrelude, SubAgentResultCallback,
    SubAgentResultChain,
};

use serde_json::Value;

/// 本次子 agent 调用的上下文。
///
/// 两侧共用：before 侧由 `SubAgentRunner::start` 构造，result 侧由 `collect_until_outcome`
/// 构造。核心库只做**透传**：它不理解 `arguments` 里各字段的业务含义，具体取哪个字段
/// （如宿主/会话标识）由回调自行决定。
///
/// 放在模块根而非某一侧：before 与 result 都要用它，归属任一侧都会让另一侧反向依赖。
///
/// 注意：`arguments` 是**父 agent 传入本子 agent 的原始工具参数**，与子 agent 自身的
/// 挂起-恢复会话（`session_id` / `run_id`）是不同概念，不要混用。
#[derive(Debug, Clone)]
pub struct SubAgentCallContext {
    /// 子 agent 工具名（如 `"flexible_parameterize"`）。
    pub agent_name: String,
    /// 本次调用的 tool_call_id（等于 `run_id` / invocation id）。
    pub tool_call_id: String,
    /// 父 agent 传给该子 agent 的原始参数（LLM tool_call 的 `arguments`）。
    pub arguments: Value,
}
