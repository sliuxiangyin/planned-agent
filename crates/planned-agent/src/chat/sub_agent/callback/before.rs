//! 子 agent **启动前**回调：在 task 文本生成前注入系统侧数据。

use async_trait::async_trait;
use serde_json::Value;

use super::SubAgentCallContext;

/// 子 agent **启动前**的注入决策。
///
/// 由 [`SubAgentBeforeCallback`] 返回，决定是否在 task 文本生成之前，
/// 用系统侧数据修正 / 补齐父 agent 传入的参数。
pub enum BeforeDecision {
    /// 注入数据：`Value` 的**顶层字段合并**进 task 参数，同名 key 由注入方覆盖。
    ///
    /// 组合规则是「合并 + 注入方优先」：不同名字段各自保留，同名字段以注入值为准。
    /// 因此即使父 agent（LLM）仍在传某个字段，系统注入的真实数据也**永远赢**——
    /// 这是一条防污染保证，而不是单纯的覆盖开关。
    ///
    /// 非对象（字符串 / 数组等）会被忽略并记 warn。
    Inject(Value),
    /// 不注入，按原参数继续。
    Continue,
    /// 中断本次调用：**不启动子 agent**，直接把 `String` 作为失败结果返回。
    ///
    /// 用于外部依赖不可用（如状态库读失败）这类不可重试的硬错误。注意 before 阶段
    /// 没有模型输出可重做，因此这里**不提供 `Retry`**；而「state 产物缺失」这种
    /// 可容忍的情况应当返回 [`Continue`](Self::Continue)，不要 Abort。
    Abort(String),
}

/// 子 agent **启动前**回调：在 task 文本生成前注入系统数据。
///
/// 与 [`SubAgentResultCallback`](super::SubAgentResultCallback) 的分工是**互不相交**的：
/// - before：**只碰入参**——把数据合并进 task JSON，不读也不写子 agent 的任何输出；
/// - result：**只碰产物**——子 agent 已有输出之后的决策。
///
/// 因此两者可独立启用：本回调不会改变 result 回调的行为，也不进入子 agent 的 history。
#[async_trait]
pub trait SubAgentBeforeCallback: Send + Sync {
    /// 子 agent 启动前触发（task 文本生成之前，因此**不会**出现在子 agent 的 history 里）。
    ///
    /// - `ctx`：本次调用的上下文（工具名 / tool_call_id / 父 agent 传入的原始参数）
    ///
    /// 返回 [`BeforeDecision`]：
    /// - `Inject(v)`：把 `v` 的顶层字段合并进 task 参数（同名覆盖）
    /// - `Continue`：不注入
    /// - `Abort(reason)`：不启动子 agent，`reason` 作为失败结果返回
    async fn before_start(&self, ctx: &SubAgentCallContext) -> BeforeDecision;

    /// 回调名，仅用于日志。
    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }
}
