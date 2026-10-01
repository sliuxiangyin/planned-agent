//! `flexible_revise` 的定稿登记回调：在**既有定稿产物**上做最小改动。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`（值由父 agent 原样传入，
//! 语义见 `docs/chat-flexible-回调会话归属设计.md`）。
//!
//! 定稿判定（与 `flexible_revise.toml` 的输出契约一致）：
//! - `{"status":"revised", "steps":[…], …}` → 登记本次改动。
//! - 其它（`status:"needs_restart"` / `"error"` / 非 JSON）→ **不登记任何产物**
//!   （由前置分析拦下，本回调不执行）—— 这正是「改不动就别乱动」的落点。
//!
//! # 与其它 step 的关键差别
//!
//! 1. **`CLEAR` 为空**：绝不无条件清下游产物（其余五个 step 的定稿回调都会清下游，这是
//!    「用户改需求 → 整链重做」的根源之一）。这里只在用户本轮真的改了某个产物时才写它。
//! 2. **`current_step` 不动**（`next_step = None`）：修订不是「推进阶段」。
//! 3. **两个产物都不是直接透传 LLM 给的数组**，而是各自过一道系统侧把关：
//!    - `steps` → [`apply_step_patches`]：按 `result_reference` **定位替换**，其余步骤对象连碰都碰不到
//!      （所以「没让改的步骤逐字保留」是结构保证，不靠 LLM 纪律，也不需要事后比对还原）；
//!    - `inputs` → [`validate_inputs_patch`]：只许改**已有**参数的 `default` / `description`，
//!      增删参数 / 改名 / 重排一律拒绝（那是参数化步的职责，该走 `needs_restart` 全量重做）。
//!
//! 本 step 的差异全部写在下面几个常量里；编排用 [`super::super::commit`] 与
//! [`super::super::analysis::require_analysis`] 提供的零策略工具，解析与守门在
//! [`super::super::prelude`]，链组装在 [`super`]。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent::chat::{ResultDecision, SubAgentCall, SubAgentResultCallback};
use serde_json::{Map, Value};

use crate::pages::plan::shared::session::TemplateNotifier;
use crate::services::plans_flexible_service::PlansFlexibleService;

use super::super::analysis::require_analysis;
use super::super::commit::{commit_state, hand_off, persist_template};
use super::super::super::placeholder;

/// 子 agent 工具名（日志用，链组装也要用）。
pub(super) const AGENT: &str = "flexible_revise";
/// 定稿 status：输出顶层 `status` 等于它才算定稿（链组装要拿它建前置分析）。
pub(super) const OK_STATUS: &str = "revised";

/// `steps` 的产物 key —— 走「定位替换」，不是直接写 LLM 给的数组。
const STEPS_KEY: &str = "steps";

/// `inputs` 的产物 key —— 走「校验后写入」，不是直接写 LLM 给的数组。
const INPUTS_KEY: &str = "inputs";

/// 「模板已落库」的档位：只有在这个档位上，修订才需要问用户「要不要更新已保存的模板」。
const SAVED_STEP: &str = "saved";

/// 修订输出里的「用户授权更新已保存模板」标志。
///
/// **它是落库的唯一开关**：`true` 的唯一合法来源是 revise 已经用 `request_user_action`
/// 问过用户、用户选了「更新」。缺失 / `false` ⇒ **不落库**（「不要默认保存」——
/// 没问过就没授权）。
const SAVE_TEMPLATE_KEY: &str = "save_template";

/// 本次修订是否要落库到已保存的模板。
///
/// 两个条件**同时**满足才写：档位是 `saved`（模板确实存在），且用户授权了（`save_template`
/// 为 `true`）。抽成纯函数是为了让「授权与否」这条安全判断有回归测试 —— 它的失败模式是
/// **静默覆盖用户没同意改的模板**。
fn should_persist_template(current_step: &str, save_template: bool) -> bool {
    current_step == SAVED_STEP && save_template
}

/// 修订**可能**改动的其它产物（既不在 `steps` 也不在 `inputs` 之列）。
///
/// 与外层 `PRODUCTS` 的区别：这里是「出现才写、缺失**不告警**」—— 修订的常态本就只动
/// `steps` 或 `inputs`，用通用 `build_patch` 会对每个未出现的 key 各刷一条误导性 warn。
const OPTIONAL_PRODUCTS: &[&str] = &["task_definition", "output_schema"];

/// 补丁里的步骤对象必须齐全的字段（缺一个即认为 LLM 违约，不猜着补）。
const STEP_FIELDS: &[&str] = &["result_reference", "intent", "expected_output", "dependencies"];

/// `flexible_revise` 的定稿登记回调。
pub(super) struct ReviseCallback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（读写流程中间状态 + 落库）。
    service: Arc<PlansFlexibleService>,
    /// 模板更新通知句柄：已保存状态下同步落库成功后，通知左侧面板重读模板。
    notifier: TemplateNotifier,
}

impl ReviseCallback {
    pub(super) fn new(
        plan_id: String,
        service: Arc<PlansFlexibleService>,
        notifier: TemplateNotifier,
    ) -> Self {
        Self {
            plan_id,
            service,
            notifier,
        }
    }

    /// 读当前会话已定稿的 `steps` 与 `inputs`（缺失为 `Null`）。
    ///
    /// 返回 `Value` 而非 `Vec`，是为了能原样交给 [`placeholder::validate`]（它收 `&Value`）。
    ///
    /// 刻意**在这里重新读一次**而不是用注入侧的快照：注入发生在子 agent 启动前，
    /// 与写回之间隔着一次完整的 LLM 往返；读旧值与写新值之间必须贴着同一次读取，
    /// 否则同一会话上的两次修订会互相覆盖。
    async fn load_current_products(&self, session_id: &str) -> Result<(Value, Value), String> {
        let state = self
            .service
            .load_state(&self.plan_id, session_id)
            .await
            .map_err(|e| {
                format!(
                    "[{}] 读取流程状态失败（plan_id={}, host_session_id={}）：{}",
                    AGENT, self.plan_id, session_id, e
                )
            })?;

        let Some((_, products)) = state else {
            return Err(format!(
                "[{}] 本会话尚无流程状态，无法修订（plan_id={}, host_session_id={}）",
                AGENT, self.plan_id, session_id
            ));
        };

        let products = serde_json::from_str::<Value>(&products).unwrap_or(Value::Null);
        let pick = |key: &str| products.get(key).cloned().unwrap_or(Value::Null);

        Ok((pick(STEPS_KEY), pick(INPUTS_KEY)))
    }
}

#[async_trait]
impl SubAgentResultCallback for ReviseCallback {
    async fn on_result(&self, call: &SubAgentCall<'_>) -> ResultDecision {
        tracing::info!(
            "[{}] 子 agent '{}' 完成, tool_call_id={}, content_len={}, is_error={}",
            AGENT,
            call.ctx.agent_name,
            call.ctx.tool_call_id,
            call.text().len(),
            call.result.is_error,
        );

        // ── 取前置分析结论（解析 / 定稿判定 / 会话归属都已由它完成）──
        let analysis = match require_analysis(AGENT, call) {
            Ok(analysis) => analysis,
            Err(decision) => return decision,
        };

        // ── 1. 组补丁 ──
        // `steps` 走定位替换、`inputs` 走校验后写入，其余产物出现才写。
        //
        // 两者缺失都合法：`steps` 空/缺 = 本次没改步骤对象（典型是「只改某个参数的默认值」，
        // 此时 `steps` 里是占位符、文字本来就不变）；`inputs` 缺 = 本次没动参数表。
        let steps_patch = analysis
            .parsed
            .get(STEPS_KEY)
            .and_then(Value::as_array)
            .filter(|items| !items.is_empty());
        let inputs_patch = analysis
            .parsed
            .get(INPUTS_KEY)
            .and_then(Value::as_array);

        // 需要旧产物时才读库：`steps` 与 `inputs` 的修订都必须与「旧值」对照着写。
        let (old_steps, old_inputs) = if steps_patch.is_some() || inputs_patch.is_some() {
            match self.load_current_products(analysis.session_id).await {
                Ok(products) => products,
                Err(reason) => return abort(&reason),
            }
        } else {
            // `revised` 但一个产物都没带：白跑一次，无害（不动任何既有产物）。
            tracing::warn!("[{}] 定稿但未携带任何产物改动，状态保持不变", AGENT);
            return hand_off(call);
        };

        let merged_steps = match steps_patch {
            Some(patches) => {
                // 定位替换：按 `result_reference` 找到就让补丁对象顶替它，其余对象逐字不动。
                let old = old_steps.as_array().map(Vec::as_slice).unwrap_or_default();
                match apply_step_patches(old, patches) {
                    Ok(merged) => Some(Value::Array(merged)),
                    Err(reject) => {
                        // LLM 违约（指向不存在的步骤 / 字段残缺 / 重复 target）：
                        // 如实上报，不猜着改、也不悄悄 append。
                        return abort(&format!("[{}] 修订补丁无法应用：{}", AGENT, reject));
                    }
                }
            }
            None => None,
        };

        if let Some(new_inputs) = inputs_patch {
            // 参数表只许改已有参数的取值 / 说明：增删参数（或改名 / 重排）属于参数化步的职责，
            // 由系统兜底拒绝 —— 提示词是要求，这里才是保证。
            let old = old_inputs.as_array().map(Vec::as_slice).unwrap_or_default();
            if let Err(reject) = validate_inputs_patch(old, new_inputs) {
                return abort(&format!("[{}] 参数表修订无法应用：{}", AGENT, reject));
            }
        }

        // 改后的步骤仍须「占位符全部有定义」：LLM 可能顺手在 `intent` / `expected_output` 里
        // 插一个新 `${name}`（提示词禁了，这里兜底）。**在写状态之前挡住** —— 否则会留下
        // 「state 已改、模板没落」的残局（落库时的 `build_payload` 也会拒，但那时状态已被污染）。
        let steps_for_check = merged_steps.as_ref().unwrap_or(&old_steps);
        // 本次没给 `inputs` 就用现状参数表 —— 只改了 `steps` 时也要校验它与现状参数表自洽。
        let inputs_for_check: &Value = analysis
            .parsed
            .get(INPUTS_KEY)
            .filter(|v| !v.is_null())
            .unwrap_or(&old_inputs);
        if let Err(reason) = placeholder::validate(steps_for_check, None, inputs_for_check) {
            return abort(&format!(
                "[{}] 修订后的步骤占位符校验未通过：{}",
                AGENT, reason
            ));
        }

        let mut patch = Map::new();
        if let Some(merged) = merged_steps {
            patch.insert(STEPS_KEY.to_string(), merged);
        }
        if let Some(new_inputs) = inputs_patch {
            patch.insert(INPUTS_KEY.to_string(), Value::Array(new_inputs.clone()));
        }
        for key in OPTIONAL_PRODUCTS {
            // 缺失或 `null` ⇒ **跳过写入**（`merge_state` 把 `null` 当「删除」，
            // 直接写会在子 agent 漏字段时静默清掉既有产物）。
            if let Some(value) = analysis.parsed.get(*key).filter(|v| !v.is_null()) {
                patch.insert((*key).to_string(), value.clone());
            }
        }

        // ── 2. 写回：`next_step = None` ⇒ 保留原档位（修订不推进 `current_step`）──
        let (step, _) = match commit_state(
            AGENT,
            &self.service,
            &self.plan_id,
            analysis.session_id,
            None,
            &patch,
        )
        .await
        {
            Ok(state) => state,
            // 写库失败不可重试，如实上报（见 `super::super::commit::commit_state` 的说明）
            Err(reason) => return ResultDecision::Abort(reason),
        };

        // ── 3. 已保存过 **且用户刚授权** ⇒ 把修订同步回模板 ──
        // 不做这一步，`plans_flexible_sessions.parameterized_task` 会停在旧值：
        // 左侧面板显示的参数默认值、执行时用的模板，都还是改动前的。
        //
        // **落库必须由用户授权**：`save_template: true` 的唯一合法来源是 revise 在
        // `request_user_action` 里问过、用户选了「更新模板」。缺失 = 没问过 = 没授权 → 不落库。
        let save_template = analysis
            .parsed
            .get(SAVE_TEMPLATE_KEY)
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if step == SAVED_STEP && !save_template {
            // 改动本身已落在会话状态里（不回滚），只是不写模板 —— 用户下次仍可再说一次。
            tracing::warn!(
                "[{}] 未获保存授权（{} 非 true），改动只保留在会话状态，模板未更新",
                AGENT,
                SAVE_TEMPLATE_KEY
            );
        }

        if should_persist_template(&step, save_template) {
            let products = match self
                .service
                .load_state(&self.plan_id, analysis.session_id)
                .await
            {
                Ok(Some((_, products))) => products,
                Ok(None) => {
                    return abort(&format!(
                        "[{}] 同步模板失败：会话 {} 的流程状态记录不见了",
                        AGENT, analysis.session_id
                    ))
                }
                Err(e) => {
                    return abort(&format!("[{}] 同步模板时读取流程状态失败：{}", AGENT, e))
                }
            };

            if let Err(reason) = persist_template(
                &self.service,
                &self.notifier,
                &self.plan_id,
                analysis.session_id,
                &products,
            )
            .await
            {
                return abort(&format!("[{}] {reason}", AGENT));
            }

            tracing::info!(
                "[{}] 已把修订同步到已保存的模板（plan_id={}, host_session_id={}）",
                AGENT,
                self.plan_id,
                analysis.session_id
            );
        }

        // 对外结果不由这里决定（前置分析已定稿）：只决定要不要把值交给下一位。
        hand_off(call)
    }

    fn name(&self) -> &str {
        AGENT
    }
}

/// 记日志并返回 `Abort` —— 修订里的失败一律「如实上报，不猜着改」。
fn abort(reason: &str) -> ResultDecision {
    tracing::error!("{}", reason);
    ResultDecision::Abort(reason.to_string())
}

/// 修订补丁被拒绝的原因（都可读，直接作为 `Abort` 理由）。
#[derive(Debug, PartialEq)]
pub(super) enum PatchReject {
    /// 补丁对象本身不合法（缺字段 / 字段类型不对）。
    Malformed { reference: String, reason: String },
    /// `result_reference` 在旧 `steps` 里不存在 —— 语义上等于**新增步骤**，不被允许。
    UnknownReference(String),
    /// 同一个 `result_reference` 在补丁里出现多次（定位有歧义）。
    Duplicate(String),
    /// 参数表被增删（或改名 / 重排）—— 修订只允许改已有参数的取值 / 说明。
    InputsChanged {
        added: Vec<String>,
        removed: Vec<String>,
        reordered: bool,
    },
}

impl std::fmt::Display for PatchReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed { reference, reason } => {
                write!(f, "补丁对象不合法（{reference}）：{reason}")
            }
            Self::UnknownReference(reference) => write!(
                f,
                "补丁指向了不存在的步骤 `{reference}`（修订不允许增删步骤，需整体重做）"
            ),
            Self::Duplicate(reference) => {
                write!(f, "同一个步骤 `{reference}` 在补丁里出现了多次")
            }
            Self::InputsChanged {
                added,
                removed,
                reordered,
            } => {
                let mut parts: Vec<String> = Vec::new();
                if !added.is_empty() {
                    parts.push(format!("新增参数 {}", added.join(" / ")));
                }
                if !removed.is_empty() {
                    parts.push(format!("删除参数 {}", removed.join(" / ")));
                }
                if *reordered {
                    parts.push("参数顺序被调整".to_string());
                }
                write!(
                    f,
                    "参数表只能修改已有参数的取值 / 说明，不能增删参数、改名或调整顺序（需重做整份计划）：{}",
                    parts.join("、")
                )
            }
        }
    }
}

/// 按 `result_reference` 把补丁对象**整块替换**进旧 `steps`。
///
/// 返回的新数组里，未被补丁覆盖的对象**引用自旧数组、逐字不变**（含顺序与条数）；
/// 条数不同即说明发生了增删，而增删在结构上无法表达（只做定位替换）→ 一律走 `needs_restart`。
///
/// 与旧对象**深度相等**的补丁（等价于「原样抄回来表示保持不变」）静默忽略：冗余无害，
/// 不升级成流程中断。
pub(super) fn apply_step_patches(
    old_steps: &[Value],
    patches: &[Value],
) -> Result<Vec<Value>, PatchReject> {
    let mut merged: Vec<Value> = old_steps.to_vec();
    let mut seen: Vec<String> = Vec::with_capacity(patches.len());

    for patch in patches {
        let reference = validate_step_object(patch)?;

        if seen.contains(&reference) {
            return Err(PatchReject::Duplicate(reference));
        }
        seen.push(reference.clone());

        let Some(index) = index_of_reference(&merged, &reference) else {
            return Err(PatchReject::UnknownReference(reference));
        };

        if merged[index] == *patch {
            tracing::warn!(
                "[{}] 补丁对象与旧对象完全相同，忽略该条：{}",
                AGENT,
                reference
            );
            continue;
        }
        merged[index] = patch.clone();
    }

    Ok(merged)
}

/// 校验修订后的参数表：只许改**已有**参数的 `default` / `description`。
///
/// 不许增删参数、不许改 `name`、不许调整顺序 —— 顺序也是契约的一部分（参数化步规定
/// 「按在 `steps` 中首次出现的先后」），顺序变了意味着 `steps` 里的占位符要跟着重排。
/// 这些改动本该走 `needs_restart` 全量重做：新增的参数必然要在 `steps` 里插入新的
/// `${name}`，而「哪些值该参数化、同一原值共用哪个占位符」是参数化步的职责。
///
/// 违反时**拒绝本次修订**（由调用方 `Abort` 如实上报）—— 让一个「`inputs` 里多了个没有
/// 对应占位符的死参数」的坏状态进库，比让用户重做一次糟得多。
pub(super) fn validate_inputs_patch(
    old_inputs: &[Value],
    new_inputs: &[Value],
) -> Result<(), PatchReject> {
    let name_of = |item: &Value| -> Option<String> {
        item.get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    let mut new_names: Vec<String> = Vec::with_capacity(new_inputs.len());
    for item in new_inputs {
        let Some(name) = name_of(item) else {
            return Err(PatchReject::Malformed {
                reference: preview(item),
                reason: "参数项缺少非空字符串 `name`".to_string(),
            });
        };
        new_names.push(name);
    }

    let old_names: Vec<String> = old_inputs.iter().filter_map(name_of).collect();

    if old_names != new_names {
        let added: Vec<String> = new_names
            .iter()
            .filter(|name| !old_names.contains(*name))
            .cloned()
            .collect();
        let removed: Vec<String> = old_names
            .iter()
            .filter(|name| !new_names.contains(*name))
            .cloned()
            .collect();
        // 名字集合相同而序列不同 ⇒ 只是重排
        let reordered = added.is_empty() && removed.is_empty();
        return Err(PatchReject::InputsChanged {
            added,
            removed,
            reordered,
        });
    }

    Ok(())
}

/// 校验补丁是一个合法的步骤对象，返回它的 `result_reference`。
fn validate_step_object(patch: &Value) -> Result<String, PatchReject> {
    let Some(object) = patch.as_object() else {
        return Err(PatchReject::Malformed {
            reference: preview(patch),
            reason: "补丁项不是对象".to_string(),
        });
    };

    for field in STEP_FIELDS {
        if !object.contains_key(*field) {
            return Err(PatchReject::Malformed {
                reference: preview(patch),
                reason: format!("缺少字段 `{field}`（四个字段必须齐全）"),
            });
        }
    }

    let Some(reference) = object
        .get("result_reference")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Err(PatchReject::Malformed {
            reference: preview(patch),
            reason: "`result_reference` 必须是非空字符串".to_string(),
        });
    };

    match object.get("dependencies") {
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => {}
        _ => {
            return Err(PatchReject::Malformed {
                reference: reference.to_string(),
                reason: "`dependencies` 必须是字符串数组".to_string(),
            })
        }
    }

    Ok(reference.to_string())
}

/// 在步骤数组里按 `result_reference` 定位下标。
fn index_of_reference(steps: &[Value], reference: &str) -> Option<usize> {
    steps
        .iter()
        .position(|step| step.get("result_reference").and_then(Value::as_str) == Some(reference))
}

/// 把非法对象截成可读的短摘要（日志 / 报错里带上下文）。
fn preview(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() <= 60 {
        text
    } else {
        format!("{}…", text.chars().take(60).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「落库必须有用户授权」是安全判断：错在「多写」会静默覆盖用户没同意改的模板。
    #[test]
    fn template_is_persisted_only_with_step_and_consent() {
        assert!(should_persist_template("saved", true));
        // 没授权（字段缺失 / false）⇒ 不落库 —— 「不要默认保存」的落点
        assert!(!should_persist_template("saved", false));
        // 还没保存过（模板不存在）⇒ 无可落库
        assert!(!should_persist_template("planned", true));
        assert!(!should_persist_template("parameterized", true));
        assert!(!should_persist_template("output_defined", true));
        assert!(!should_persist_template("none", true));
    }
    use serde_json::json;

    /// 造一个四字段齐全的步骤对象。
    fn step(reference: &str, intent: &str, deps: &[&str]) -> Value {
        json!({
            "result_reference": reference,
            "intent": intent,
            "expected_output": format!("{intent} 的产出"),
            "dependencies": deps,
        })
    }

    /// 造一个参数项。
    fn input(name: &str, default: Value) -> Value {
        json!({
            "name": name,
            "default": default,
            "description": format!("{name} 的说明"),
        })
    }

    // ───────────────────────── `steps`：定位替换 ─────────────────────────

    /// 用户场景：只调第 2 步 → 只有 #E2 被替换，其余对象**逐字不变**（含顺序与条数）。
    #[test]
    fn replaces_only_the_targeted_step_and_keeps_the_rest_verbatim() {
        let old = vec![
            step("#E1", "读取 /var/log/app.log", &[]),
            step("#E2", "筛 ERROR 并按时间升序", &["#E1"]),
            step("#E3", "写入 /tmp/errors.log", &["#E2"]),
        ];
        let patches = vec![step("#E2", "筛 ERROR 并按时间降序", &["#E1"])];

        let merged = apply_step_patches(&old, &patches).expect("定位替换应成功");

        assert_eq!(merged.len(), 3, "条数不变");
        assert_eq!(merged[0], old[0], "#E1 必须逐字不变");
        assert_eq!(merged[2], old[2], "#E3 必须逐字不变");
        assert_eq!(merged[1]["intent"], "筛 ERROR 并按时间降序");
        assert_eq!(merged[1]["dependencies"], json!(["#E1"]));
    }

    /// 一次点名多处：两处都被替换，未涉及的不动。
    #[test]
    fn replaces_multiple_targets() {
        let old = vec![
            step("#E1", "a", &[]),
            step("#E2", "b", &["#E1"]),
            step("#E3", "c", &["#E2"]),
        ];
        let patches = vec![step("#E1", "a2", &[]), step("#E3", "c2", &["#E2"])];

        let merged = apply_step_patches(&old, &patches).unwrap();

        assert_eq!(merged[0]["intent"], "a2");
        assert_eq!(merged[2]["intent"], "c2");
        assert_eq!(merged[1], old[1], "#E2 未涉及，必须逐字不变");
    }

    /// 更新依赖关系也走同一条路（只动该对象自己的 `dependencies`）。
    #[test]
    fn dependency_change_is_applied_in_place() {
        let old = vec![step("#E1", "a", &[]), step("#E2", "b", &["#E1"])];
        let patches = vec![step("#E2", "b", &[])];

        let merged = apply_step_patches(&old, &patches).unwrap();

        assert_eq!(merged[1]["dependencies"], json!([]));
        assert_ne!(merged[1], old[1], "深度不等，应真的替换");
    }

    /// 指向不存在的 `result_reference` = 想新增步骤 → 拒绝（由调用方转全量重做）。
    #[test]
    fn unknown_reference_is_rejected() {
        let old = vec![step("#E1", "a", &[])];
        let patches = vec![step("#E9", "新步骤", &["#E1"])];

        assert_eq!(
            apply_step_patches(&old, &patches),
            Err(PatchReject::UnknownReference("#E9".to_string()))
        );
    }

    /// 四字段缺一 → 拒绝（不猜着补字段）。
    #[test]
    fn missing_field_is_rejected() {
        let old = vec![step("#E1", "a", &[])];
        let patches = vec![json!({
            "result_reference": "#E1",
            "intent": "改后",
            // 缺 expected_output 与 dependencies
        })];

        match apply_step_patches(&old, &patches) {
            Err(PatchReject::Malformed { reason, .. }) => {
                assert!(reason.contains("expected_output"), "实际：{reason}");
            }
            other => panic!("应为 Malformed，实际 {other:?}"),
        }
    }

    /// `dependencies` 类型不对 → 拒绝。
    #[test]
    fn non_string_dependencies_are_rejected() {
        let old = vec![step("#E1", "a", &[])];
        let patches = vec![json!({
            "result_reference": "#E1",
            "intent": "改后",
            "expected_output": "产出",
            "dependencies": [1, 2],
        })];

        match apply_step_patches(&old, &patches) {
            Err(PatchReject::Malformed { reason, .. }) => {
                assert!(reason.contains("dependencies"), "实际：{reason}");
            }
            other => panic!("应为 Malformed，实际 {other:?}"),
        }
    }

    /// 同一个 target 出现两次 → 拒绝（定位有歧义）。
    #[test]
    fn duplicate_target_is_rejected() {
        let old = vec![step("#E1", "a", &[]), step("#E2", "b", &["#E1"])];
        let patches = vec![step("#E2", "b2", &["#E1"]), step("#E2", "b3", &["#E1"])];

        assert_eq!(
            apply_step_patches(&old, &patches),
            Err(PatchReject::Duplicate("#E2".to_string()))
        );
    }

    /// 与旧对象深度相等（原样抄回来表示「保持不变」）→ 静默忽略，不算错。
    #[test]
    fn identical_patch_is_silently_ignored() {
        let old = vec![step("#E1", "a", &[]), step("#E2", "b", &["#E1"])];
        let patches = vec![old[1].clone()];

        let merged = apply_step_patches(&old, &patches).unwrap();

        assert_eq!(merged, old, "忽略后应与旧数组完全一致");
    }

    /// 空补丁 → 原样返回（调用方负责跳过写入，不产生空数组覆盖）。
    #[test]
    fn empty_patches_keep_old_steps() {
        let old = vec![step("#E1", "a", &[])];
        assert_eq!(apply_step_patches(&old, &[]).unwrap(), old);
    }

    // ─────────────────────── `inputs`：只改值、不改集合 ───────────────────────

    /// 用户场景：只改已有参数的默认值 / 说明 → 通过（这就是「只更新 inputs」）。
    #[test]
    fn inputs_patch_allows_changing_existing_default() {
        let old = vec![input("file_path", json!("/var/log/app.log"))];
        let new = vec![json!({
            "name": "file_path",
            "default": "D:/logs/a.log",
            "description": "要读取的日志文件绝对路径",
        })];

        assert_eq!(validate_inputs_patch(&old, &new), Ok(()));
    }

    /// 未被改动的参数原样带上（系统整块替换）→ 通过。
    #[test]
    fn inputs_patch_allows_untouched_entries() {
        let old = vec![input("a", json!(1)), input("b", json!("x"))];
        let new = vec![input("a", json!(1)), input("b", json!("y"))];

        assert_eq!(validate_inputs_patch(&old, &new), Ok(()));
    }

    /// 新增参数 → 拒绝（该走 `needs_restart`：要在 `steps` 里插新占位符）。
    #[test]
    fn inputs_patch_rejects_added_parameter() {
        let old = vec![input("a", json!(1))];
        let new = vec![input("a", json!(1)), input("timeout", json!(30))];

        match validate_inputs_patch(&old, &new) {
            Err(PatchReject::InputsChanged { added, removed, reordered }) => {
                assert_eq!(added, vec!["timeout".to_string()]);
                assert!(removed.is_empty());
                assert!(!reordered);
            }
            other => panic!("应为 InputsChanged，实际 {other:?}"),
        }
    }

    /// 删除参数 → 拒绝。
    #[test]
    fn inputs_patch_rejects_removed_parameter() {
        let old = vec![input("a", json!(1)), input("b", json!(2))];
        let new = vec![input("a", json!(1))];

        match validate_inputs_patch(&old, &new) {
            Err(PatchReject::InputsChanged { added, removed, reordered }) => {
                assert!(added.is_empty());
                assert_eq!(removed, vec!["b".to_string()]);
                assert!(!reordered);
            }
            other => panic!("应为 InputsChanged，实际 {other:?}"),
        }
    }

    /// 参数全清空 → 拒绝（等价于删光全部参数）。
    #[test]
    fn inputs_patch_rejects_clearing_all() {
        let old = vec![input("a", json!(1))];

        match validate_inputs_patch(&old, &[]) {
            Err(PatchReject::InputsChanged { removed, .. }) => {
                assert_eq!(removed, vec!["a".to_string()]);
            }
            other => panic!("应为 InputsChanged，实际 {other:?}"),
        }
    }

    /// 只调整顺序 → 拒绝（顺序也是参数化步的产物契约）。
    #[test]
    fn inputs_patch_rejects_reordered() {
        let old = vec![input("a", json!(1)), input("b", json!(2))];
        let new = vec![input("b", json!(2)), input("a", json!(1))];

        assert_eq!(
            validate_inputs_patch(&old, &new),
            Err(PatchReject::InputsChanged {
                added: Vec::new(),
                removed: Vec::new(),
                reordered: true,
            })
        );
    }

    /// 参数项缺 `name` → 拒绝（形状不合法，不猜）。
    #[test]
    fn inputs_patch_rejects_item_without_name() {
        let old = vec![input("a", json!(1))];
        let new = vec![json!({ "default": 1, "description": "缺 name" })];

        match validate_inputs_patch(&old, &new) {
            Err(PatchReject::Malformed { reason, .. }) => {
                assert!(reason.contains("name"), "实际：{reason}");
            }
            other => panic!("应为 Malformed，实际 {other:?}"),
        }
    }

    /// 原本没有参数、也不新增 → 通过（空表不动）。
    #[test]
    fn inputs_patch_allows_empty_to_empty() {
        assert_eq!(validate_inputs_patch(&[], &[]), Ok(()));
    }

    // ───────────────────────────── 常量 ─────────────────────────────

    /// 本 step 独有的回归价值：**常量取值正确**。
    /// 公共行为（`null` 跳过、写库失败 `Abort` 等）见 `super::super::commit` 的测试，不在此重复。
    #[test]
    fn revise_only_touches_declared_products_and_keeps_current_step() {
        assert_eq!(AGENT, "flexible_revise");
        assert_eq!(OK_STATUS, "revised");
        assert_eq!(STEPS_KEY, "steps");
        assert_eq!(INPUTS_KEY, "inputs");
        // `steps` / `inputs` 各自走校验后写入，不在「直接透传」的可选产物之列。
        assert!(!OPTIONAL_PRODUCTS.contains(&"steps"));
        assert!(!OPTIONAL_PRODUCTS.contains(&"inputs"));
        // 修订绝不清下游产物 —— 这正是诉求「改一处不该作废其余」的落点。
        assert!(OPTIONAL_PRODUCTS.contains(&"output_schema"));
    }
}
