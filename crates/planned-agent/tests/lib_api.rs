//! planned-agent library —— 公开 API 路径解析 sanity 检查
//!
//! 不构造任何运行时实例，只验证 `pub use` 路径全部可达。
//! 若某个 re-export 漏配或可见性不足，编译期即可发现。

use planned_agent::{
    // 透传自 core 的类型
    CoarseGrainedPlan,
    CoarseGrainedStep,
    CoarsePlanValidationResult,
    CoarsePlanner,
    DataRequirement,
    PlanComplexity,
    RiskLevel,

    ReActAgent,
    ReActAgentConfig,
    ReActExecutionResult,
    ReActStep,
    Thought,
    Action,
    Observation,
    ObserveResult,

    PlanContext,

    // 本 crate 粗粒度实现
    LlmCoarsePlanner,

    // 本 crate ReAct 实现 + 编排
    DefaultReActAgent,
    PlanAndExecuteAgent,
    PlanAndExecuteConfig,
    PlanAndExecuteResult,
    StepResult,
    ExecutorContext,
    StepStore,

    // 轨迹记录
    TraceRecorder,
    TraceRecorderConfig,

    // chat 模块
    ChatService,
    ChatConfig,
    ChatResponse,
    ChatEvent,
};

// 仅用于强制编译器把 trait 路径视为已使用
fn _assert_paths_resolve(_p: &dyn CoarsePlanner) {}
fn _assert_react_paths<R: ReActAgent>() {}

#[test]
fn library_api_paths_resolve() {
    // 全部类型/函数路径编译通过即视为通过
    let _: Option<PlanAndExecuteConfig> = None;
    let _: Option<ReActAgentConfig> = None;
    let _: Option<PlanAndExecuteResult> = None;
    let _: Option<CoarseGrainedPlan> = None;
    let _: Option<ReActExecutionResult> = None;
    let _: Option<TraceRecorderConfig> = None;
    let _: Option<ChatConfig> = None;
}