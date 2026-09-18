//! step 子 agent 的**结果链**与启动前注入（`SubAgentBeforeCallback`）及共享工具。
//!
//! # 目录结构
//!
//! ```text
//! step_callback/
//! ├── mod.rs              通用：HOST_SESSION_ID_FIELD / read_host_session_id + 对外重导出
//! ├── analysis.rs         通用：解析 + 规范化纯函数 + StepAnalysis / require_analysis
//! ├── commit.rs           通用：零策略纯工具（build_patch / commit_state / hand_off）
//! ├── prelude.rs          通用：FlexibleStepPrelude（默认前置分析 + 守门）
//! ├── before_inject.rs    通用：StateInjectCallback
//! └── stepN/              各 step 自己的东西（N = 1..5）
//!     ├── mod.rs                 组装点：只写 create_stepN_callback
//!     ├── stepN_callback.rs      定稿登记回调 + 本 step 的常量 + 单测
//!     └── …                      该 step 专属文件（step2 另有 step2_execution_trace_callback.rs、tool_trace.rs）
//! ```
//!
//! 命名：文件 = 其中主要类型的 snake_case（`StepNCallback` → `stepN_callback.rs`）；
//! 只装函数的文件按职责命名（`commit.rs` / `tool_trace.rs`）。
//!
//! 判断新代码放哪：**只有某个 step 用**就放进 `stepN/`；**多个 step 共用**才提到本层。
//!
//! # 结果链
//!
//! ```text
//! FlexibleStepPrelude（前置分析：解析 + 守门 + 定稿对外文本，所有 step 默认共用）
//!   → 各 step 自己的定稿登记回调（写产物 / 推进 step；step1/3/4/5 到这一层为止）
//!   → 可选的额外回调（step2 的轨迹提取，排在定稿登记**之前**：轨迹失败要 Abort 且状态未推进）
//! ```
//!
//! **「保存状态」不共用实现**：每个 `stepN/stepN_callback.rs` 自己写 `on_result` 全流程（取分析结论、
//! 组产物补丁、`merge_state`、错误处理、决策）。五个 step 的产物 / 清理 / 推进规则只会越来越
//! 不一样，共享一份实现只会被特例字段撑变形；冗余换来的是各 step 能自由演化。
//! 本层只保留两种「跨 step 的约定」，它们不是保存状态、而是接口与把关：
//! - [`prelude`]：把前置分析做进框架槽位（`planned_agent::chat::SubAgentChainPrelude`），
//!   各 step 的 `create_stepN_callback` 统一带上 —— «默认，不必手挂»的原因见该模块说明；
//! - [`analysis`]：解析 / 规范化纯函数 + `StepAnalysis`（prelude 产出、各回调消费的契约）。
//!
//! 抽出去的只有**零策略纯工具**（[`commit`]、[`analysis::require_analysis`]）：同样的入参必然
//! 同样的行为，不含任何 step 语义；各 step 拿自己的常量去调它，差异仍写在各自文件里。
//!
//! # 各 step 回调的共同行为契约
//!
//! 这些不是「各写一遍的纪律」，而是由公共函数**强制**的 —— 各 step 只需拿自己的常量去调：
//!
//! 1. **前置分析产物缺失 ⇒ `Abort`**（接线错误，不猜着写、不静默跳过）→ [`analysis::require_analysis`]
//! 2. **写库失败 ⇒ `Abort`**（不可重试，如实上报，不静默 `Accept`）→ [`commit::commit_state`]
//! 3. **产物值为 `null` 或字段缺失 ⇒ 跳过写入**（`merge_state` 把 `null` 当「删除」）→ [`commit::build_patch`]
//! 4. **决策只看 `call.is_last`**：非末位 `Next(call.text())`、末位 `Accept` → [`commit::hand_off`]
//! 5. **解析 / 定稿判定 / 会话定位一律用 prelude 的结论**（[`analysis::StepAnalysis`]），不重复实现。
//!
//! 各 step 的 `stepN/mod.rs` 只剩「挂哪几环」（[`step1`]~[`step5`]），回调与常量在
//! `stepN/stepN_callback.rs`。
//!
//! 启动前注入见 [`before_inject`]：把 state 里已定稿的产物直接塞给子 agent，
//! 取代「协调器 LLM 转抄」。
//!
//! 设计背景见 `docs/chat-flexible-回调会话归属设计.md`。

// ── 通用件（所有 step 共用）──
pub(crate) mod analysis;
pub(crate) mod before_inject;
pub(crate) mod commit;
pub(crate) mod prelude;

// ── 各 step（一个 step 一个目录）──
pub(crate) mod step1;
pub(crate) mod step2;
pub(crate) mod step3;
pub(crate) mod step4;
pub(crate) mod step5;

pub(crate) use before_inject::StateInjectCallback;
pub(crate) use step1::create_step1_callback;
pub(crate) use step2::create_step2_callback;
pub(crate) use step3::create_step3_callback;
pub(crate) use step4::create_step4_callback;
pub(crate) use step5::create_step5_callback;

/// 子 agent 调用参数中承载「宿主会话 id」的字段名。
///
/// 见 `docs/chat-flexible-回调会话归属设计.md` §5：该字段由父协调器把 system prompt
/// 「会话上下文」里的 session_id **原样照抄**进 step 子 agent 的调用参数，供子 agent
/// 完成回调（`SubAgentResultCallback`）定位「本次执行属于哪个会话」。
///
/// 注意：不要与子 agent 挂起-恢复用的 `session_id`（`SubAgentSessionStore` 的内存键）
/// 混用 —— 那个是运行期句柄，这个是宿主会话（落库归属）。
pub(crate) const HOST_SESSION_ID_FIELD: &str = "host_session_id";

/// 从子 agent 调用参数里读取宿主会话 id（字段缺失或类型不符时返回 `None`）。
pub(crate) fn read_host_session_id(args: &serde_json::Value) -> Option<String> {
    args.get(HOST_SESSION_ID_FIELD)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}
