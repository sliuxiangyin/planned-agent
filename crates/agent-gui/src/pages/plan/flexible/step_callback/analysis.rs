//! flexible 各 step 子 agent 输出的**解析与规范化**：纯函数，无副作用、无状态。
//!
//! 这里只回答两个问题：
//! 1. 「这段输出是我们要的流程 JSON 吗？」——[`parse_step_output`] 逐级放宽地解析
//!    （严格 → 剥 ``` 围栏 → 取首个花括号平衡对象）。
//! 2. 「把它交给下游之前怎么抹平脏格式？」——[`canonicalize_windows_paths`] 把所有
//!    「看起来像 Windows 路径」的片段改成正斜杠，从源头断掉「协调器 LLM 转抄时反斜杠
//!    翻倍」这类污染。
//!
//! 谁在用：
//! - [`super::prelude`]：前置分析在这里解析 + 规范化，并**定稿对外文本**；
//! - 各 step 的定稿登记回调（`super::step1` / `super::step2` / `super::save`）：直接读分析产物里的 `parsed`
//!   （见 [`StepAnalysis`]），不必再解析一次。

use planned_agent::chat::{ResultDecision, SubAgentCall};
use serde_json::Value;

/// 输出格式不合契约时发给子 agent 的纠正消息（触发其重新生成）。
///
/// **必须显式禁止重新执行工具**：重试会在同一会话里再跑一轮，产生额外副作用与重复动作；
/// 而格式问题只该重写输出，不该重做动作。
pub(crate) const RETRY_PROMPT: &str = "你的上一条输出不是合法 JSON。请只重新输出那个 JSON 对象，不要重新执行任何工具、不要新增或删除工具调用：不要用 markdown 代码块包裹（不要 ```），不要任何说明文字，也不要前后空行；字段与取值保持与上一次输出一致。";

/// 前置分析产物（`SubAgentCall.analysis`）的字段契约。
///
/// 生产方是 [`super::prelude`]，消费方是各 step 的业务回调；两边都从这里取 key，
/// 免得字段名散落在两处、改一处忘一处。
pub(crate) mod keys {
    /// 规范化后的定稿 JSON（已去围栏、路径已改正斜杠）。
    pub const PARSED: &str = "parsed";
    /// 输出顶层 `status` 字符串。
    pub const STATUS: &str = "status";
    /// 会话归属 id（即 `host_session_id`）。
    pub const SESSION_ID: &str = "host_session_id";
    /// 原文是否「不干净」（带 ``` 围栏或夹说明文字）—— 不干净时对外文本需要替换。
    pub const DIRTY: &str = "dirty";
}

/// 从前置分析产物里读出业务回调需要的东西。
///
/// 有了它，业务回调可以放心假设「输出可解析、已定稿、会话可定位」，不必各自再解析一遍 ——
/// 这些把关都是 [`super::prelude::FlexibleStepPrelude`] 的职责。
///
/// 只暴露登记产物**真正用到**的字段：`status` / `dirty` 留在产物里供观测（日志 / 排查），
/// 但没人读就不进这个视图（不用的字段进来只会变成死代码）。
pub(crate) struct StepAnalysis<'a> {
    /// 规范化后的定稿 JSON。
    pub parsed: &'a Value,
    /// 会话归属 id。
    pub session_id: &'a str,
}

impl<'a> StepAnalysis<'a> {
    /// 从 `SubAgentCall.analysis` 构造。
    ///
    /// 任一必需字段缺失（例如链上没挂 prelude、或分析与回调的契约版本对不上）返回 `None` ——
    /// 调用方应视为**契约违反**并如实上报，而不是当成「本次不定稿」静默跳过。
    pub(crate) fn from_analysis(analysis: &'a Value) -> Option<Self> {
        Some(Self {
            parsed: analysis.get(keys::PARSED)?,
            session_id: analysis.get(keys::SESSION_ID)?.as_str()?,
        })
    }
}

/// 读前置分析结论；缺失即**接线错误**（链上漏挂 [`super::prelude::FlexibleStepPrelude`]，
/// 或生产 / 消费两侧的契约版本对不上），返回可直接 `return` 的 `Err(Abort)` ——
/// 不猜着写，也不静默跳过。
///
/// 各 step 的定稿登记回调都从这一步开始：拿到它就可以放心假设「输出可解析、已定稿、会话可定位」。
pub(crate) fn require_analysis<'a>(
    agent: &str,
    call: &SubAgentCall<'a>,
) -> Result<StepAnalysis<'a>, ResultDecision> {
    match StepAnalysis::from_analysis(call.analysis) {
        Some(analysis) => Ok(analysis),
        None => {
            let reason = format!(
                "[{}] 前置分析产物缺失（结果链上是否漏挂 FlexibleStepPrelude？），无法登记产物",
                agent
            );
            tracing::error!("{}", reason);
            Err(ResultDecision::Abort(reason))
        }
    }
}

/// 解析子 agent 的流程 JSON，返回 `(解析结果, 原文是否不干净)`。
///
/// 逐级放宽：严格解析 → 剥 markdown 代码块围栏 → 取首个花括号平衡的对象片段。
/// 后两级命中说明原文本带 ``` 围栏或说明文字（不干净），应以规范化文本替换对外结果，
/// 父 agent 便不必再面对脏格式。均失败则返回 `None`。
pub(crate) fn parse_step_output(text: &str) -> Option<(Value, bool)> {
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

/// 把 JSON 里所有「看起来像 Windows 路径」的字符串片段中的 `\` 换成 `/`。
///
/// 背景：协调器 LLM 在 step 之间转抄数据时，会把 JSON 里已经转义过的 `\\` 当成值本身再
/// 转义一次 —— 每传一手，反斜杠翻一倍（实测：`C:\Users\…` 传一轮变 `C:\\Users\…`，
/// 老/新会话都被污染）。正斜杠没有可转义的形态，于是从源头断掉这类污染；
/// PowerShell 与 Rust `std::fs` 均已实测接受正斜杠路径。
///
/// 只识别以盘符（`C:\`）或 UNC（`\\`）开头的片段，遇到引号 / 真换行 / `;` / `|` /
/// `<` / `>` 等明显不属于路径的字符即停止；最坏情况是「少改」（漏掉末尾被误判为
/// 转义序列的部分），不会把普通文本改坏（正则 `\d`、字面量 `\n` 都不会被碰到）。
///
/// 幂等：已经转过的正斜杠路径不会被再次处理。
pub(crate) fn canonicalize_windows_paths(value: &Value) -> Value {
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
    fn windows_paths_become_forward_slashes() {
        // 复现线上污染：反斜杠路径经协调器转抄后翻倍。规范化后不再有可转义的 `\`。
        let raw = r#"{"task":{"params":{"path":"C:\\Users\\woddp\\Desktop\\Downloads"}}}"#;
        let parsed: Value = serde_json::from_str(raw).unwrap();
        let canonical = canonicalize_windows_paths(&parsed);
        assert_eq!(
            canonical["task"]["params"]["path"],
            Value::String("C:/Users/woddp/Desktop/Downloads".into())
        );
        assert!(
            !canonical.to_string().contains("\\\\"),
            "不应再残留双反斜杠"
        );
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
    fn canonicalization_is_idempotent() {
        let once = canonicalize_windows_paths(&Value::String(r"C:\a\b 与 D:\c".into()));
        assert_eq!(canonicalize_windows_paths(&once), once);
    }

    #[test]
    fn step_analysis_reads_all_contract_fields() {
        let analysis = serde_json::json!({
            "parsed": { "status": "success" },
            "status": "success",
            "host_session_id": "s1",
            "dirty": true,
        });
        let view = StepAnalysis::from_analysis(&analysis).expect("契约字段齐全");
        assert_eq!(view.session_id, "s1");
        assert_eq!(view.parsed["status"], "success");
    }

    #[test]
    fn step_analysis_rejects_incomplete_contract() {
        // 没挂 prelude 时 analysis 是 Null：必须视为契约违反（`None`），不能当成「不定稿」。
        assert!(StepAnalysis::from_analysis(&Value::Null).is_none());
        assert!(StepAnalysis::from_analysis(&serde_json::json!({ "status": "success" })).is_none());
    }
}
