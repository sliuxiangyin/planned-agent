//! 工具结果路由器（路由 + handler 注册中心 + 一站式执行）
//!
//! 这个文件是工具结果后处理的**唯一聚合点**，三件事都集中在一处：
//! 1. **路由**——根据 `(categories, obs)` 决策该走哪些 handler（`route` 静态函数）
//! 2. **注册**——`ToolResultRouter::register` 把 `(PostProcessKind, handler)` 加入内部表
//! 3. **执行**——`ToolResultRouter::process` 一站式跑完 `route → 按序 apply 已注册 handler`
//!
//! 具体 handler 实现放在 [`crate::planner::react::tool_result_handler`] 子模块下，
//! 每种 handler 自己管自己的依赖（如 LLM 客户端、`max_bytes`），构造时注入。
//!
//! 设计原则：
//! - **`#[non_exhaustive]`**：`PostProcessKind` 留扩展口，新增 kind 不破坏下游。
//! - **找不到 handler 不报错**：`process()` 在 `HashMap` 里 miss 时静默跳过——
//!   路由层已决定"该不该做"，调度层不替业务决策。
//! - **handler trait 只有 3 参数**（`obs` / `current_intent` / `next_intent`）：
//!   `categories` 只在路由层参与判断，不下沉到 handler —— 减少冗余，便于扩展。
//! - **handler 自己持有依赖**：例如 `HtmlBrowserPostHandler` 构造时持有 `Arc<HtmlCleanSubAgent>`，
//!   路由器完全不感知 LLM 客户端存在。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use planned_agent_core::planner::react::Observation;
use planned_agent_core::tool_registry::ToolCategory;

use crate::planner::react::sub_agents::html_clean_subagent::{looks_like_html, HtmlCleanSubAgent};
use crate::planner::react::tool_result_handler::{
    BinaryTruncatePostHandler, HtmlBrowserPostHandler,
};

// =====================================================================
// 后处理类型枚举
// =====================================================================

/// 所有可能的后处理类型。`#[non_exhaustive]` 留给上游按需扩展。
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PostProcessKind {
    /// Browser 分类 + 输出疑似 HTML → 走 HTML 清洗（结构化后进入主上下文）
    HtmlClean,
    /// 任意分类 + 输出过大 → 截断，防止 token 爆炸
    BinaryTruncate,
    /// 默认透传（路由命不中任何规则时的兜底）
    Passthrough,
}

impl PostProcessKind {
    /// 中文标签（用于日志 / 调试）
    pub fn label(&self) -> &'static str {
        match self {
            PostProcessKind::HtmlClean => "HTML 清洗",
            PostProcessKind::BinaryTruncate => "大输出截断",
            PostProcessKind::Passthrough => "透传",
        }
    }
}

// =====================================================================
// 后处理计划
// =====================================================================

/// 有序的后处理步骤列表。`Vec` 顺序就是执行顺序。
#[derive(Debug, Clone)]
pub struct PostProcessPlan {
    pub steps: Vec<PostProcessKind>,
}

impl PostProcessPlan {
    pub fn empty() -> Self {
        Self { steps: Vec::new() }
    }
    pub fn contains(&self, kind: PostProcessKind) -> bool {
        self.steps.contains(&kind)
    }
}

// =====================================================================
// Handler 契约
// =====================================================================

/// 所有工具结果后处理器的统一接口。
///
/// 只接收 `obs` / `current_intent` / `next_intent` 三个参数。
/// `categories` **不在** handler 签名里——路由层已处理分类判断，
/// handler 不需要重复感知上下文。
#[async_trait]
pub trait ObservationPostHandler: Send + Sync {
    async fn handle(
        &self,
        obs: Observation,
        current_intent: &str,
        next_intent: &str,
    ) -> Observation;
}

// =====================================================================
// 默认截断阈值
// =====================================================================

/// 默认截断阈值：超过该字节数认为输出过大。
pub const DEFAULT_TRUNCATE_THRESHOLD_BYTES: usize = 50_000;

// =====================================================================
// 路由器
// =====================================================================

/// 工具结果路由器。同时承担**注册中心** + **路由决策** + **链式执行** 三个职责。
///
/// 设计取舍：合并到一个 struct 里避免"调度器只转一手"的空壳层，
/// 同时让 handler 注册（开发态）和 process 执行（运行态）在同一个 API 表面。
pub struct ToolResultRouter {
    handlers: HashMap<PostProcessKind, Arc<dyn ObservationPostHandler>>,
}

impl Default for ToolResultRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolResultRouter {
    /// 创建一个空的路由器（无任何 handler 注册）。
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// **便捷构造**：直接预装"标准两个 handler"（HTML 清洗 + 大输出截断）。
    ///
    /// 这是 `DefaultReActAgent` 场景下的常见配置——避免调用方写 4 行 register。
    /// 若需要非标准配置，仍可走 `new() + register()` 路径。
    pub fn with_standard_handlers(
        html_sub_agent: std::sync::Arc<HtmlCleanSubAgent>,
        binary_truncate_max_bytes: usize,
    ) -> Self {
        let mut r = Self::new();
        r.register(
            PostProcessKind::HtmlClean,
            std::sync::Arc::new(HtmlBrowserPostHandler::new(html_sub_agent)),
        );
        r.register(
            PostProcessKind::BinaryTruncate,
            std::sync::Arc::new(BinaryTruncatePostHandler::new(binary_truncate_max_bytes)),
        );
        r
    }

    /// 注册一个 handler 到指定 kind。同 kind 重复注册会覆盖。
    pub fn register(
        &mut self,
        kind: PostProcessKind,
        handler: Arc<dyn ObservationPostHandler>,
    ) {
        self.handlers.insert(kind, handler);
    }

 
    /// **路由决策**（静态方法，无状态）：根据 `(categories, obs)` 决定走哪些步骤。
    ///
    /// 决策规则（按优先级）：
    /// 1. Browser 分类 + 输出疑似 HTML → `HtmlClean`
    /// 2. 输出超过 [`DEFAULT_TRUNCATE_THRESHOLD_BYTES`] 字节 → `BinaryTruncate`
    /// 3. 以上都不命中 → `Passthrough`
    pub fn route(categories: &[ToolCategory], obs: &Observation) -> PostProcessPlan {
        let mut steps = Vec::new();

        if categories.contains(&ToolCategory::Browser) && looks_like_html_obs(obs) {
            steps.push(PostProcessKind::HtmlClean);
        }

        if output_too_large(obs, DEFAULT_TRUNCATE_THRESHOLD_BYTES) {
            steps.push(PostProcessKind::BinaryTruncate);
        }

        if steps.is_empty() {
            steps.push(PostProcessKind::Passthrough);
        }

        PostProcessPlan { steps }
    }

    /// **一站式执行**：`route` → 按 `plan.steps` 顺序链式调用已注册的 handler。
    ///
    /// ## 链式语义（重要）
    /// 后一个 handler 收到的是**前一个 handler 处理后的 `Observation`**，
    /// 不是原始 `obs`。每个 handler 在 `Self::route` 之后按 plan 顺序
    /// 把上一步的输出接力下去：
    ///
    /// ```text
    ///   raw_obs ──► handler_a.handle(raw_obs) = obs_a
    ///            ──► handler_b.handle(obs_a)    = obs_b   ← 不是 obs
    ///            ──► handler_c.handle(obs_b)    = obs_c   ← 不是 obs_a
    ///            ──► ... 最终返回 obs_n
    /// ```
    ///
    /// 这样 `HtmlClean` 清洗后的字符串会作为 `BinaryTruncate` 截断的输入，
    /// 避免清洗前的脏数据再次进入主 LLM 上下文。
    ///
    /// 找不到对应 kind 的 handler 静默跳过（路由层不替业务决策）。
    pub async fn process(
        &self,
        obs: Observation,
        categories: &[ToolCategory],
        current_intent: &str,
        next_intent: &str,
    ) -> Observation {
        let plan = Self::route(categories, &obs);
        // 关键：`current` 在循环中被覆盖 —— 下一个 handler 收到的是上一个的输出
        let mut current = obs;
        for kind in plan.steps {
            if let Some(handler) = self.handlers.get(&kind) {
                current = handler
                    .handle(current, current_intent, next_intent)
                    .await;
            }
        }
        current
    }
}

// =====================================================================
// 内部辅助函数（模块私有）
// =====================================================================

/// 判断 `Observation.output` 是否疑似 HTML。
fn looks_like_html_obs(obs: &Observation) -> bool {
    let Some(s) = obs.output.as_str() else {
        return false;
    };
    looks_like_html(s)
}

/// 判断 `Observation.output` 是否过大（按 UTF-8 字节数）。
fn output_too_large(obs: &Observation, threshold: usize) -> bool {
    observation_output_bytes(obs) > threshold
}

fn observation_output_bytes(obs: &Observation) -> usize {
    if let Some(s) = obs.output.as_str() {
        return s.len();
    }
    serde_json::to_string(&obs.output)
        .map(|s| s.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn obs(output: Value) -> Observation {
        Observation {
            output,
            is_complete: false,
            error: None,
            duration_ms: 0,
        }
    }

    fn obs_html() -> Observation {
        obs(json!(
            "<!doctype html><html><body><article><h1>Title</h1><p>some long enough body for detection here</p></article></body></html>"
        ))
    }

    fn obs_short_text() -> Observation {
        obs(json!("just plain text"))
    }

    #[test]
    fn browser_html_triggers_html_clean() {
        let plan = ToolResultRouter::route(&[ToolCategory::Browser], &obs_html());
        assert!(plan.contains(PostProcessKind::HtmlClean));
    }

    #[test]
    fn non_browser_html_does_not_trigger_html_clean() {
        let plan = ToolResultRouter::route(&[ToolCategory::File], &obs_html());
        assert!(!plan.contains(PostProcessKind::HtmlClean));
    }

    #[test]
    fn browser_but_not_html_does_not_trigger_html_clean() {
        let plan = ToolResultRouter::route(&[ToolCategory::Browser], &obs_short_text());
        assert!(!plan.contains(PostProcessKind::HtmlClean));
    }

    #[test]
    fn oversized_triggers_binary_truncate() {
        let big = "x".repeat(DEFAULT_TRUNCATE_THRESHOLD_BYTES + 1);
        let plan = ToolResultRouter::route(&[ToolCategory::Text], &obs(json!(big)));
        assert!(plan.contains(PostProcessKind::BinaryTruncate));
    }

    #[test]
    fn short_output_falls_through_to_passthrough() {
        let plan = ToolResultRouter::route(&[ToolCategory::Text], &obs_short_text());
        assert_eq!(plan.steps, vec![PostProcessKind::Passthrough]);
    }

    #[test]
    fn browser_with_huge_html_triggers_both() {
        let mut html = String::from("<!doctype html><html><body>");
        html.push_str(&"x".repeat(DEFAULT_TRUNCATE_THRESHOLD_BYTES));
        html.push_str("</body></html>");
        let plan = ToolResultRouter::route(&[ToolCategory::Browser], &obs(json!(html)));
        assert!(plan.contains(PostProcessKind::HtmlClean));
        assert!(plan.contains(PostProcessKind::BinaryTruncate));
        assert_eq!(plan.steps[0], PostProcessKind::HtmlClean);
        assert_eq!(plan.steps[1], PostProcessKind::BinaryTruncate);
    }

    #[tokio::test]
    async fn process_skips_missing_handlers_silently() {
        let router = ToolResultRouter::new(); // 空注册表
        let obs = router
            .process(
                obs(json!("plain text")),
                &[ToolCategory::File],
                "cur",
                "next",
            )
            .await;
        assert!(obs.error.is_none());
        assert_eq!(obs.output.as_str().unwrap(), "plain text");
    }

    // 假 handler：把 obs.output 接成"输入 + 后缀"，便于观察链式行为
    struct AppendSuffixHandler(&'static str);
    #[async_trait::async_trait]
    impl ObservationPostHandler for AppendSuffixHandler {
        async fn handle(
            &self,
            obs: Observation,
            _current_intent: &str,
            _next_intent: &str,
        ) -> Observation {
            let mut o = obs;
            let cur = o.output.as_str().unwrap_or("").to_string();
            o.output = Value::String(format!("{cur}{}", self.0));
            o
        }
    }

    /// **链式语义**：后一个 handler 收到的必须是前一个 handler 的输出，而不是原始 obs。
    /// —— HtmlClean 处理后，BinaryTruncate 拿到的是清洗结果（这里以追加后缀模拟）。
    #[tokio::test]
    async fn process_chains_each_handler_receives_previous_output() {
        let mut router = ToolResultRouter::new();
        router.register(
            PostProcessKind::HtmlClean,
            Arc::new(AppendSuffixHandler("A")),
        );
        router.register(
            PostProcessKind::BinaryTruncate,
            Arc::new(AppendSuffixHandler("B")),
        );

        // 构造同时触发 HtmlClean（Browser + HTML）和 BinaryTruncate（> 50K 字节）的 obs
        let mut big_html = String::from("<!doctype html><html><body>");
        big_html.push_str(&"x".repeat(DEFAULT_TRUNCATE_THRESHOLD_BYTES));
        big_html.push_str("</body></html>");
        let original_len = big_html.len();

        let result = router
            .process(
                obs(json!(big_html.clone())),
                &[ToolCategory::Browser],
                "cur",
                "next",
            )
            .await;

        let s = result.output.as_str().unwrap();
        // 链式：A 拿原 big_html，追加 "A" → B 拿 "...A"，再追加 "B" → 末尾应为 "AB"
        assert_eq!(
            &s[original_len..],
            "AB",
            "链式：原内容后应先追加 A 再追加 B（如果拿到的是原始 obs 这里会失败）"
        );
        assert!(
            s.starts_with("<!doctype"),
            "原 HTML 头应保留（A 透传原内容后追加）"
        );
        // 完整比对：原始 + A + B
        assert_eq!(s, format!("{big_html}AB"));
    }

    /// 只有一个 handler 触发时，链式行为退化为"单步"，仍然正确。
    #[tokio::test]
    async fn process_single_registered_handler_appends_once() {
        let mut router = ToolResultRouter::new();
        router.register(
            PostProcessKind::BinaryTruncate,
            Arc::new(AppendSuffixHandler("X")),
        );

        let big = "x".repeat(DEFAULT_TRUNCATE_THRESHOLD_BYTES + 1);
        let original_len = big.len();

        let result = router
            .process(obs(json!(big)), &[ToolCategory::Text], "c", "n")
            .await;

        let s = result.output.as_str().unwrap();
        assert_eq!(&s[original_len..], "X");
    }
}