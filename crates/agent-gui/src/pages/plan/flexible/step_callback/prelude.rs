//! flexible 的**默认前置分析**：五个 step 共用同一套「解析输出 → 定位会话 → 判定定稿」。
//!
//! # 为什么是「默认」而不是每个 step 手动挂一个拦截器
//!
//! 这三件事是所有 step 业务回调的**共同前置条件**：解析失败要统一重试、会话归属缺失就不能
//! 写库、非定稿输出一个产物都不该落。靠每个注册点记得挂、还得挂对位置（必须排在业务回调
//! 之前），迟早会漏 —— 所以它做成了框架槽位（`planned_agent::chat::SubAgentChainPrelude`），
//! 由各 step 的 `create_stepN_callback` 统一带上，业务回调里看不见它。
//!
//! 产物（`SubAgentCall.analysis`）的字段契约见 [`super::analysis::keys`]，消费方见
//! [`super::analysis::StepAnalysis`]。
//!
//! # 它同时是「守门员」
//!
//! - 解析失败 → [`ResultDecision::Retry`]：要求子 agent 重新输出（框架最多重试 2 次）；
//! - 非定稿 status / 拿不到 `host_session_id` → [`ResultDecision::Stop(Accept)`]：
//!   **链不跑**，业务回调一个都不执行 —— 对应「宁可『不写』也不写错」。
//!
//! 于是业务回调可以放心假设「我拿到的分析结果一定是可用的定稿」。

use async_trait::async_trait;
use planned_agent::chat::storage::StoreMessage;
use planned_agent::chat::{
    PreludeOutcome, ResultDecision, SubAgentCallContext, SubAgentChainPrelude,
};
use planned_agent_core::mcp::types::ToolResult;
use serde_json::Value;

use super::analysis::{canonicalize_windows_paths, keys, parse_step_output, RETRY_PROMPT};
use super::{read_host_session_id, HOST_SESSION_ID_FIELD};

/// 一个 step 的默认前置分析：`agent` 只用于日志，`ok_status` 是该 step 的定稿值
/// （如 `flexible_step2` 的 `"success"`）。
pub(crate) struct FlexibleStepPrelude {
    agent: &'static str,
    ok_status: &'static str,
}

impl FlexibleStepPrelude {
    pub(crate) fn new(agent: &'static str, ok_status: &'static str) -> Self {
        Self { agent, ok_status }
    }
}

#[async_trait]
impl SubAgentChainPrelude for FlexibleStepPrelude {
    async fn analyze(
        &self,
        ctx: &SubAgentCallContext,
        result: &ToolResult,
        _history: &[StoreMessage],
    ) -> PreludeOutcome {
        let text = result.content.as_str().unwrap_or("");

        // ── 1. 解析（逐级宽松：严格 → 剥围栏 → 取首个花括号平衡对象）──
        let Some((parsed, dirty)) = parse_step_output(text) else {
            let preview: String = text.chars().take(500).collect();
            tracing::warn!(
                "[{}] 输出不是可解析的 JSON，要求子 agent 重新输出。原文前 500 字符：{}",
                self.agent,
                preview
            );
            return PreludeOutcome::Stop(ResultDecision::Retry(RETRY_PROMPT.to_string()));
        };

        // ── 2. 会话归属：值来自父 agent 传入的原始参数（核心库只透传，语义由这里定义）──
        let Some(session_id) = read_host_session_id(&ctx.arguments) else {
            tracing::warn!(
                "[{}] 未拿到 {}（父 agent 是否按 schema 传参？），不登记任何产物",
                self.agent,
                HOST_SESSION_ID_FIELD
            );
            return PreludeOutcome::Stop(ResultDecision::Accept);
        };

        // ── 3. 定稿判定 ──
        let status = parsed.get("status").and_then(Value::as_str).unwrap_or("");
        if status != self.ok_status {
            // 非定稿（error / back_to_* / empty_result / cancelled 等）：本次未产生有效定稿产物，
            // 不推进 current_step，也不动任何已有产物（协调器按 prompt 决定重试或取消）。
            tracing::warn!(
                "[{}] 非定稿 status（{:?} ≠ {:?}），不登记产物、链不执行",
                self.agent,
                status,
                self.ok_status
            );
            return PreludeOutcome::Stop(ResultDecision::Accept);
        }

        // ── 4. 规范化：落库值与对外文本共用同一份（路径改正斜杠）──
        let canonical = canonicalize_windows_paths(&parsed);

        // 只在「原文不干净（带围栏 / 夹说明）」或「路径确实被改动」时才替换对外文本：
        // 与旧的 `Transform` 条件等价，干净且无路径的原文保持原样交给父 agent。
        let outer = (dirty || canonical != parsed).then(|| canonical.to_string());

        // analysis 的字段名以 `analysis::keys` 为准，生产方 / 消费方共用同一份常量。
        let mut analysis = serde_json::Map::new();
        analysis.insert(keys::PARSED.to_string(), canonical);
        analysis.insert(keys::STATUS.to_string(), Value::String(status.to_string()));
        analysis.insert(keys::SESSION_ID.to_string(), Value::String(session_id));
        analysis.insert(keys::DIRTY.to_string(), Value::Bool(dirty));

        PreludeOutcome::Proceed {
            analysis: Value::Object(analysis),
            outer,
        }
    }

    fn name(&self) -> &str {
        self.agent
    }
}
