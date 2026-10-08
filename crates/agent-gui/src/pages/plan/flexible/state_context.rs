//! 协调器的「每轮状态注入」来源：把本会话**真实**的流程状态现算成一段短文本，
//! 交给 core 的 driver 每轮作为**临时 system 消息**注入（只在当轮请求里存在，不写 history）。
//!
//! # 为什么要有它
//!
//! 协调器的档位原先只能靠 LLM 主动调用 `flexible_state` 获取 —— 一旦它自认为"已经完成"
//! 就会跳过，之后上下文里再无任何纠错信息，于是凭对话历史**编造**档位（实测：库里是
//! `planned`，它却答复"当前 `current_step` 已是 `saved`"）。根因是「事实要靠模型去拿」，
//! 而不是「事实必然在场」。
//!
//! 故改为**系统直取、每轮注入** —— 与各 step 的 `INJECT_MAPPING`（`step_callback/before_inject.rs`）
//! 同一条纪律：产物与状态由系统取，不经 LLM 转抄。完整分析见
//! `docs/planned-agent/flexible-coordinator-state-injection.md`。
//!
//! # 边界
//!
//! - **只注入"存在性"，不注入产物内容**：协调器只需判路由（推进到哪一步、缺哪些产物），
//!   产物正文由各 step 自己注入；要看正文时模型仍可按需调用 `flexible_state`。
//! - **读失败不打断对话**（返回 `None` + 记日志）：注入是增强项，不是前置条件。
//! - **不缓存**：同一会话内档位会被 step 回调改（如 plan 定稿把档位打回 `planned`），
//!   缓存会立刻过期；本地 SQLite 读是微秒级，不值得冒这个险。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent::chat::PerRoundContextSource;
use serde_json::Value;

use crate::services::plans_flexible_service::PlansFlexibleService;

/// 该会话尚无流程状态记录时的档位（与 `flexible_state` 工具一致）。
const NO_STATE: &str = "none";
/// 无记录时的空产物表。
const NO_PRODUCTS: &str = "{}";

/// 协调器判路由需要的核心产物 key（顺序即流程顺序）。
///
/// 刻意不含 `category`：它只影响执行期的技能注入（见 `flexible-plan-category.md`），
/// 与"推进到哪一步 / 差哪些产物"无关，列出来只会稀释信号。
const PRODUCT_KEYS: [&str; 4] = ["task_definition", "steps", "inputs", "output_schema"];

/// 档位 → 一句人话。
///
/// **与 `flexible_state` 工具的档位说明共用同一份**（`tool/flexible_state.rs` 引用本函数）——
/// 两处各写一份必然漂移。
pub(crate) fn step_meaning(step: &str) -> &'static str {
    match step {
        "none" => "尚未开始（未做需求澄清）",
        "task_defined" => "需求澄清已定稿（可执行计划步）",
        "planned" => "计划已定稿（可执行参数化步）",
        "parameterized" => "参数化已定稿（可执行输出定义步；也可跳过输出定义直接落库）",
        "output_defined" => "输出定义已定稿（可执行 flexible_save 落库）",
        "saved" => "flexible_save 已落库",
        _ => "未知档位",
    }
}

/// 按 `plan_id` + `session_id` 现读 `flexible_state` 的注入来源。
pub(crate) struct FlexibleStateContext {
    plan_id: String,
    session_id: String,
    service: Arc<PlansFlexibleService>,
}

impl FlexibleStateContext {
    pub(crate) fn new(
        plan_id: String,
        session_id: String,
        service: Arc<PlansFlexibleService>,
    ) -> Self {
        Self {
            plan_id,
            session_id,
            service,
        }
    }
}

#[async_trait]
impl PerRoundContextSource for FlexibleStateContext {
    async fn render(&self) -> Option<String> {
        match self.service.load_state(&self.plan_id, &self.session_id).await {
            Ok(Some((current_step, products))) => Some(render_state_summary(&current_step, &products)),
            // 尚无记录：等同全新会话，明确告知（避免模型在"没数据"时自由发挥）。
            Ok(None) => Some(render_state_summary(NO_STATE, NO_PRODUCTS)),
            Err(e) => {
                tracing::warn!("[flexible] 每轮状态注入读取失败，本轮跳过: {}", e);
                None
            }
        }
    }
}

/// 生成注入文本（纯函数，便于单测锁文案）。
///
/// `products` 为 `flexible_state.products` 的 JSON 文本；非法 JSON 一律按空表处理
/// （宁可少说，不可报错中断对话）。
pub(crate) fn render_state_summary(current_step: &str, products: &str) -> String {
    let obj: serde_json::Map<String, Value> = serde_json::from_str(products).unwrap_or_default();
    let done: Vec<&str> = PRODUCT_KEYS.iter().copied().filter(|k| has(obj.get(*k))).collect();
    let missing: Vec<&str> = PRODUCT_KEYS
        .iter()
        .copied()
        .filter(|k| !has(obj.get(*k)))
        .collect();

    format!(
        "[系统注入 · 本会话实时状态 · 非用户发言]\n\
         current_step = {step}（{meaning}）\n\
         已定稿产物：{done}\n\
         未定稿产物：{missing}\n\
         本段是本会话**当前**的真实状态；上文任何关于进度 / 是否已保存的旧说法一律作废，以本段为准。",
        step = current_step,
        meaning = step_meaning(current_step),
        done = list_or_none(&done),
        missing = list_or_none(&missing),
    )
}

/// 值是否算「已定稿」：`null` 与空值（`""` / `[]` / `{}`）一律不算。
///
/// 为什么不能只判 `!= null`：`merge_state` 的补丁允许写入任意值，`steps: []` 或
/// `task_definition: ""` 都会让只判 null 的写法给出**错误的「已定稿」信号** —— 正是本模块
/// 要消灭的那类错状态。
///
/// 已知边界：若有写入点把产物存成**字符串化的空 JSON**（`"[]"` / `"{}"`），会被判为「已定稿」
/// —— 方向是放宽而非收紧；当前 `commit.rs` 的 `build_patch` 原样落库子 agent 输出的 JSON 值，
/// 未发现此类写入点。
fn has(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(_) => true,
    }
}

/// 空列表写成「（无）」，避免出现空行歧义。
fn list_or_none(keys: &[&str]) -> String {
    if keys.is_empty() {
        "（无）".to_string()
    } else {
        keys.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 锁住注入文案：档位 + 已定稿 / 未定稿产物都要在，且必须带"以上文为旧"的声明
    /// —— 那正是压制历史里幻觉结论的那一句。
    #[test]
    fn summary_reports_step_and_products() {
        let products = r##"{"category":"Browser","steps":[{"result_reference":"#E1"}],"task_definition":{"task":"t"}}"##;
        let out = render_state_summary("planned", products);
        assert!(out.contains("current_step = planned"), "{out}");
        assert!(out.contains("计划已定稿"), "{out}");
        assert!(out.contains("已定稿产物：task_definition, steps"), "{out}");
        assert!(out.contains("未定稿产物：inputs, output_schema"), "{out}");
        assert!(out.contains("以本段为准"), "{out}");
    }

    /// `null` 与缺失同义（`merge_state` 用 null 表示删除），都不能算"已定稿"。
    #[test]
    fn null_is_treated_as_missing() {
        let out = render_state_summary("parameterized", r#"{"inputs":[{"name":"a"}],"output_schema":null}"#);
        assert!(out.contains("已定稿产物：inputs"), "{out}");
        assert!(out.contains("未定稿产物：task_definition, steps, output_schema"), "{out}");
    }

    /// 无任何产物（全新会话）时不能空着，要写「（无）」。
    #[test]
    fn empty_products_render_as_none_lists() {
        let out = render_state_summary(NO_STATE, NO_PRODUCTS);
        assert!(out.contains("尚未开始"), "{out}");
        assert!(out.contains("已定稿产物：（无）"), "{out}");
        assert!(out.contains("未定稿产物：task_definition, steps, inputs, output_schema"), "{out}");
    }

    /// 坏 JSON 不得 panic，按空表处理。
    #[test]
    fn broken_products_json_is_tolerated() {
        let out = render_state_summary("planned", "not-json");
        assert!(out.contains("已定稿产物：（无）"), "{out}");
    }

    /// 空值（`""` / `[]` / `{}`）不算已定稿 —— 只判 null 会给出错误的「已定稿」信号。
    #[test]
    fn empty_values_count_as_missing() {
        let out = render_state_summary(
            "planned",
            r#"{"task_definition":"","steps":[],"inputs":{},"output_schema":null}"#,
        );
        assert!(out.contains("已定稿产物：（无）"), "{out}");
        assert!(
            out.contains("未定稿产物：task_definition, steps, inputs, output_schema"),
            "{out}"
        );
    }

    /// 档位文案与工厂档位串一一对应（写错会静默说错话）。
    #[test]
    fn every_step_has_meaning() {
        for step in [
            "none",
            "task_defined",
            "planned",
            "parameterized",
            "output_defined",
            "saved",
        ] {
            assert!(!step_meaning(step).starts_with("未知"), "缺少档位文案: {step}");
        }
        assert!(step_meaning("bogus").starts_with("未知"));
    }
}
