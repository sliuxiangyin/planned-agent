//! 各 step 回调共用的「定稿登记」实现。
//!
//! 所有 `flexible_stepN` 子 agent 都按同一约定输出**纯 JSON 且顶层带 `status`**
//! （契约见各 `crates/agent-gui/prompts/flexible/flexible_stepN.toml`）。本模块把
//! 「读会话归属 → 判定是否定稿 → 写产物 → 推进 `current_step` → 清下游产物」
//! 收敛成一份代码，各 step 只提供自己的 [`StepSpec`]。
//!
//! 判定原则：**宁可「不写」也不写错** —— 非定稿 status、拿不到 `host_session_id` 一律
//! 跳过登记（只记日志）；输出不是合法 JSON 则要求子 agent 重新输出（`Retry`，最多 2 次）；
//! 写库失败属不可重试的硬错误，直接 `Abort` 并如实上报，绝不静默放过。
//!
//! 回调只负责**状态推进 / 定稿记录**（写 `flexible_state`），与「用户是否认同本次需求」
//! 无关：用户不认同就会重跑该 step，回调再登记一次即覆盖旧值，两者不冲突。
//! 模板**落库**（`plans_flexible_sessions`）不在这里做，仍由 `flexible_save_template` 工具负责；
//! 但 step5 定稿时会把整段模板输出登记为产物（`payload_key`），供该工具直接读取，
//! 避免协调器 LLM 转抄模板 JSON 时改坏字段。
//!
//! 设计背景见 `docs/chat-flexible-回调会话归属设计.md`。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent::chat::{ResultDecision, SubAgentCallContext, SubAgentResultCallback};
use planned_agent_core::mcp::types::ToolResult;
use serde_json::{Map, Value};

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::{read_host_session_id, HOST_SESSION_ID_FIELD};

/// 一个 step 的定稿契约。
pub(crate) struct StepSpec {
    /// 子 agent 工具名（仅用于日志），如 `"flexible_step3"`。
    pub agent: &'static str,
    /// 定稿 status：子 agent 输出顶层 `status` 等于该值才算定稿。
    pub ok_status: &'static str,
    /// 定稿后推进到的 `current_step` 档位。
    pub next_step: &'static str,
    /// 定稿时要登记的产物 key（值取输出 JSON 中的同名字段；缺失或 `null` 则跳过写入）。
    pub products: &'static [&'static str],
    /// 可选：把**整段定稿输出对象**原样额外登记到该 key 下。
    ///
    /// 用途见 `step5`：模板副本存进 `flexible_state`，让 `flexible_save_template` 直接从状态
    /// 取模板落库，不再由协调器 LLM 把 step5 的输出「转抄」成工具参数（转抄会改坏字段，
    /// 例如把 `expected_schema: null` 写成 `""`）。
    pub payload_key: Option<&'static str>,
    /// 定稿时要清除（传 `null`）的下游产物 key。
    pub clear: &'static [&'static str],
}

/// 通用 step 回调：按 [`StepSpec`] 把子 agent 的定稿输出登记到 `flexible_state`。
pub(crate) struct StepCallback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（读/写流程中间状态）。
    service: Arc<PlansFlexibleService>,
    /// 本 step 的定稿契约。
    spec: StepSpec,
}

impl StepCallback {
    pub(crate) fn new(
        plan_id: String,
        service: Arc<PlansFlexibleService>,
        spec: StepSpec,
    ) -> Self {
        Self {
            plan_id,
            service,
            spec,
        }
    }
}

#[async_trait]
impl SubAgentResultCallback for StepCallback {
    async fn on_result(&self, ctx: &SubAgentCallContext, result: &ToolResult) -> ResultDecision {
        let spec = &self.spec;
        let text = result.content.as_str().unwrap_or("");
        tracing::info!(
            "[{}] 子 agent '{}' 完成, tool_call_id={}, content_len={}, is_error={}",
            spec.agent,
            ctx.agent_name,
            ctx.tool_call_id,
            text.len(),
            result.is_error,
        );

        // ── 会话归属：值来自父 agent 传入的原始参数（核心库只透传，语义由这里定义）──
        let Some(session_id) = read_host_session_id(&ctx.arguments) else {
            tracing::warn!(
                "[{}] 回调未拿到 {}（父 agent 是否按 schema 传参？），跳过状态登记",
                spec.agent,
                HOST_SESSION_ID_FIELD
            );
            return ResultDecision::Accept;
        };

        // ── 定稿判定：step 契约约定输出为纯 JSON ──
        // 子 agent 常把 JSON 包进 markdown 代码块（```json ... ```）或前后带说明文字，
        // 故先做宽松提取；仍失败才要求它重新输出（由 planned-agent 的重试循环兜底，最多 2 次）。
        let Some((parsed, dirty)) = parse_step_output(text) else {
            let preview: String = text.chars().take(500).collect();
            tracing::warn!(
                "[{}] 输出不是可解析的 JSON，要求子 agent 重新输出。原文前 500 字符：{}",
                spec.agent,
                preview
            );
            return ResultDecision::Retry(RETRY_PROMPT.to_string());
        };
        let status = parsed.get("status").and_then(serde_json::Value::as_str);
        if status != Some(spec.ok_status) {
            // 非定稿（error / back_to_* / empty_result / cancelled 等）：本次未产生有效定稿产物，
            // **不推进** current_step，也不动任何已有产物（协调器按 prompt 决定重试或取消）。
            tracing::warn!(
                "[{}] 非定稿 status（{:?} ≠ {:?}），不登记产物",
                spec.agent,
                status,
                spec.ok_status,
            );
            return ResultDecision::Accept;
        }

        let patch = build_patch(spec, &parsed);

        match self
            .service
            .merge_state(&self.plan_id, &session_id, Some(spec.next_step), &patch)
            .await
        {
            Ok((step, _)) => tracing::info!(
                "[{}] 状态已登记: plan_id={}, host_session_id={}, current_step={}",
                spec.agent,
                self.plan_id,
                session_id,
                step,
            ),
            Err(e) => {
                // 写库失败属**不可重试**的硬错误：重试同样会失败，而静默 Accept 会让协调器
                // 误以为该步已定稿（后续 step 会在缺产物的情况下继续）。故中断本次调用并如实上报。
                let reason = format!(
                    "[{}] 流程状态登记失败（plan_id={}, host_session_id={}）：{}",
                    spec.agent, self.plan_id, session_id, e
                );
                tracing::error!("{}", reason);
                return ResultDecision::Abort(reason);
            }
        }

        // 返回给父 agent 的文本做两件事：
        //   1. 原文本带 ``` 围栏或额外说明 → 换成紧凑 JSON，免得父 agent 再去猜格式；
        //   2. **Windows 路径统一改成正斜杠** —— 协调器会照抄我们返回的文本传给下一个 step，
        //      而它转抄时会把 JSON 里已转义的 `\\` 再转义一次（实测 `C:\Users\…` 传一轮变成
        //      `C:\\Users\\…`）。正斜杠没有可转义的形态，从根上断掉这类翻倍。
        let canonical_value = canonicalize_windows_paths(&parsed);
        let paths_changed = canonical_value != parsed;
        if dirty || paths_changed {
            ResultDecision::Transform(canonical_value.to_string())
        } else {
            ResultDecision::Accept
        }
    }
}

/// 由定稿输出构造 `flexible_state` 的产物补丁。
///
/// - `products`：取输出 JSON 中的同名字段；缺失或 `null` **跳过写入**（否则 `merge_state`
///   会把 `null` 当「删除」，在子 agent 漏字段时静默清掉已有产物）。
/// - `payload_key`：把整段定稿输出原样登记（step5 的模板副本）。
/// - `clear`：把下游产物置 `null` —— 重做本 step ⇒ 其下游各阶段的定稿产物作废。
fn build_patch(spec: &StepSpec, parsed: &Value) -> Map<String, Value> {
    let mut patch = Map::new();
    for key in spec.products {
        match parsed.get(*key).filter(|v| !v.is_null()) {
            Some(value) => {
                patch.insert((*key).to_string(), canonicalize_windows_paths(value));
            }
            None => tracing::warn!("[{}] 定稿但缺产物 '{}'，跳过该产物写入", spec.agent, key),
        }
    }
    if let Some(key) = spec.payload_key {
        patch.insert(key.to_string(), canonicalize_windows_paths(parsed));
    }
    for key in spec.clear {
        patch.insert((*key).to_string(), Value::Null);
    }
    patch
}

/// 输出格式不合契约时发给子 agent 的纠正消息（触发其重新生成）。
const RETRY_PROMPT: &str = "你的上一条输出不是合法 JSON。请只输出一个 JSON 对象：不要用 markdown 代码块包裹（不要 ```），不要任何说明文字，也不要前后空行；字段与取值保持与上一次输出一致。";

/// 把 JSON 里所有「看起来像 Windows 路径」的字符串片段中的 `\` 换成 `/`。
///
/// 背景：协调器 LLM 在 step 之间转抄数据时，会把 JSON 里已经转义过的 `\\` 当成值本身再
/// 转义一次 —— 每传一手，反斜杠翻一倍（实测：`C:\Users\…` 传一轮变 `C:\\Users\\…`，
/// 老/新会话都被污染）。正斜杠没有可转义的形态，于是从源头断掉这类污染；
/// PowerShell 与 Rust `std::fs` 均已实测接受正斜杠路径。
///
/// 只识别以盘符（`C:\`）或 UNC（`\\`）开头的片段，遇到引号 / 真换行 / `;` / `|` /
/// `<` / `>` 等明显不属于路径的字符即停止；最坏情况是「少改」（漏掉末尾被误判为
/// 转义序列的部分），不会把普通文本改坏（正则 `\d`、字面量 `\n` 都不会被碰到）。
fn canonicalize_windows_paths(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(canonicalize_paths_in_text(text)),
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_windows_paths).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), canonicalize_windows_paths(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn canonicalize_paths_in_text(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        match windows_path_len(&chars[i..]) {
            Some(len) => {
                out.extend(
                    chars[i..i + len]
                        .iter()
                        .map(|&c| if c == '\\' { '/' } else { c }),
                );
                i += len;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// 从 `chars` 起始处若是一个 Windows 路径片段，返回它的字符长度。
fn windows_path_len(chars: &[char]) -> Option<usize> {
    let drive_prefixed =
        chars.len() >= 3 && chars[0].is_ascii_alphabetic() && chars[1] == ':' && chars[2] == '\\';
    let unc_prefixed = chars.len() >= 2 && chars[0] == '\\' && chars[1] == '\\';
    if !drive_prefixed && !unc_prefixed {
        return None;
    }
    let mut n = if drive_prefixed { 3 } else { 2 };
    while n < chars.len() && !is_path_boundary(chars[n]) {
        n += 1;
    }
    Some(n)
}

/// 明显不属于路径、出现即结束路径片段的字符。
fn is_path_boundary(c: char) -> bool {
    matches!(
        c,
        '"' | '\'' | '\n' | '\r' | '\t' | ';' | '|' | '<' | '>' | '`' | '，' | '。' | '；'
    )
}

/// 解析子 agent 的流程 JSON，返回 `(解析结果, 原文是否不干净)`。
///
/// 逐级放宽：严格解析 → 剥 markdown 代码块围栏 → 取首个花括号平衡的对象片段。
/// 后两级命中说明原文本带 ``` 围栏或说明文字（不干净），回调应以 `Transform`
/// 换成紧凑 JSON，父 agent 便不必再面对脏格式。均失败则返回 `None`。
fn parse_step_output(text: &str) -> Option<(Value, bool)> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if value.is_object() {
            return Some((value, false));
        }
    }
    for candidate in [strip_code_fence(trimmed), extract_first_object(trimmed)]
        .into_iter()
        .flatten()
    {
        if let Ok(value) = serde_json::from_str::<Value>(candidate.trim()) {
            if value.is_object() {
                return Some((value, true));
            }
        }
    }
    None
}

/// 取出第一对 ``` 围栏之间的内容（自动跳过 ```json 这种语言标记行）。
fn strip_code_fence(text: &str) -> Option<&str> {
    let start = text.find("```")?;
    let after = &text[start + 3..];
    let body_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
    let body = &after[body_start..];
    let end = body.find("```")?;
    Some(&body[..end])
}

/// 截取第一个「花括号平衡」的 JSON 对象片段（跳过字符串字面量内的括号）。
fn extract_first_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in text[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + offset + ch.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_of(text: &str) -> Option<String> {
        parse_step_output(text)
            .and_then(|(v, _)| v.get("status").and_then(Value::as_str).map(str::to_string))
    }

    /// 复现线上失败：step1 把 JSON 包在 ```json 代码块里，且前面还有两个空行
    /// （serde_json 原报错：`expected value at line 3 column 1`）。
    #[test]
    fn parses_json_wrapped_in_code_fence_with_leading_blank_lines() {
        let raw = "\n\n```json\n{\n  \"status\": \"task_defined\",\n  \"output_format\": \"text\"\n}\n```";
        assert_eq!(status_of(raw).as_deref(), Some("task_defined"));
        let (_, dirty) = parse_step_output(raw).unwrap();
        assert!(dirty, "带围栏时应标记为不干净（需要替换）");
    }

    #[test]
    fn clean_json_is_accepted_without_rewrite() {
        assert!(!parse_step_output("{\"status\":\"success\"}").unwrap().1);
    }

    #[test]
    fn windows_paths_become_forward_slashes() {
        // 复现线上污染：反斜杠路径经协调器转抄后翻倍。规范化后不再有可转义的 `\`。
        let raw = r#"{"task":{"params":{"path":"C:\\Users\\woddp\\Desktop\\Downloads"}}}"#;
        let parsed: Value = serde_json::from_str(raw).unwrap();
        let canonical = canonicalize_windows_paths(&parsed);
        assert_eq!(
            canonical["task"]["params"]["path"],
            Value::String("C:/Users/woddp/Desktop/Downloads".into())
        );
        assert!(!canonical.to_string().contains("\\\\"), "不应再残留双反斜杠");
    }

    #[test]
    fn path_inside_command_string_is_rewritten_but_quotes_kept() {
        let value = Value::String(
            r"Add-Content -Path 'C:\Users\woddp\Desktop\Downloads\text.txt' -Value $line".into(),
        );
        assert_eq!(
            canonicalize_windows_paths(&value),
            Value::String(
                r"Add-Content -Path 'C:/Users/woddp/Desktop/Downloads/text.txt' -Value $line"
                    .into()
            )
        );
    }

    #[test]
    fn non_path_backslashes_are_left_alone() {
        // 正则、转义序列、普通文本里的 `\` 不得被改动。
        for text in [r"^\d{4}-\d{2}-\d{2}$", r"换行符是 \n", r"路径在 C: 盘"] {
            assert_eq!(
                canonicalize_windows_paths(&Value::String(text.into())),
                Value::String(text.into()),
                "不应改动非路径文本: {text}"
            );
        }
    }

    #[test]
    fn multiple_and_unc_paths_are_handled() {
        let value = Value::String(r"从 C:\a\b 复制到 D:\c\d 完成".into());
        assert_eq!(
            canonicalize_windows_paths(&value),
            Value::String("从 C:/a/b 复制到 D:/c/d 完成".into())
        );
    }

    #[test]
    fn parses_json_with_surrounding_prose() {
        let raw = "好的，结果如下：\n{\"status\":\"fields_selected\"}\n以上。";
        assert_eq!(status_of(raw).as_deref(), Some("fields_selected"));
    }

    #[test]
    fn braces_inside_string_do_not_break_extraction() {
        let raw = "说明 {\"task\":\"写入 {A} 与 }B{\",\"status\":\"task_defined\"} 结束";
        assert_eq!(status_of(raw).as_deref(), Some("task_defined"));
    }

    #[test]
    fn non_json_is_rejected() {
        assert!(parse_step_output("抱歉，我无法完成该任务。").is_none());
        assert!(parse_step_output("").is_none());
    }

    #[test]
    fn payload_key_stores_whole_output_verbatim() {
        // step5 形态：products 为空，整段输出登记为模板副本，`null` 必须原样保留
        // （曾因协调器 LLM 转抄把预期字段写成 ""，改由此处直接落库）。
        let spec = StepSpec {
            agent: "flexible_step5",
            ok_status: "success",
            next_step: "templated",
            products: &[],
            payload_key: Some("template_payload"),
            clear: &[],
        };
        let parsed: Value = serde_json::from_str(
            r#"{"status":"success","steps":[],"execution_plan":[{"expected_schema":null}]}"#,
        )
        .unwrap();
        let patch = build_patch(&spec, &parsed);
        assert_eq!(patch.len(), 1);
        assert_eq!(patch.get("template_payload"), Some(&parsed));
        assert!(
            patch["template_payload"]["execution_plan"][0]["expected_schema"].is_null(),
            "模板副本必须原样保留 null（不得被改写成 \"\"）"
        );
    }

    #[test]
    fn products_and_clear_are_composed() {
        // step2 形态：登记两个产物 + 清掉两个下游产物。
        let spec = StepSpec {
            agent: "flexible_step2",
            ok_status: "success",
            next_step: "executed",
            products: &["execution_trace", "compressed_context"],
            payload_key: None,
            clear: &["field_selection_result", "parameter_confirmation_result"],
        };
        let parsed: Value = serde_json::from_str(
            r#"{"status":"success","execution_trace":[{"tool":"builtin_read_file"}],"compressed_context":"已追加一行"}"#,
        )
        .unwrap();
        let patch = build_patch(&spec, &parsed);
        assert_eq!(patch["compressed_context"], Value::String("已追加一行".into()));
        assert_eq!(patch.get("field_selection_result"), Some(&Value::Null));
        assert_eq!(patch.get("parameter_confirmation_result"), Some(&Value::Null));
    }

    #[test]
    fn missing_product_is_skipped_not_nulled() {
        // 定稿输出漏字段时不得写 null —— 否则会静默清掉已有产物。
        let spec = StepSpec {
            agent: "flexible_step3",
            ok_status: "fields_selected",
            next_step: "fields_selected",
            products: &["field_selection_result"],
            payload_key: None,
            clear: &[],
        };
        let parsed: Value = serde_json::from_str(r#"{"status":"fields_selected"}"#).unwrap();
        assert!(build_patch(&spec, &parsed).is_empty());
    }
}
