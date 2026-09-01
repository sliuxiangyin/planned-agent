//! 从子 agent 输出文本中提取 JSON 结构化数据。
//!
//! 子 agent（如 flexible_step2）的 `content` 通常是 Markdown 混合文本，
//! 末尾包含一个 ` ```json ... ``` ` 代码块，内含执行轨迹和压缩上下文。
//! 本模块负责：
//! 1. 定义对应的强类型结构
//! 2. 从混合文本中提取并解析 JSON 块

use serde::Deserialize;

// ────────────────────────── 类型定义 ──────────────────────────

/// 执行轨迹中的单个工具调用步骤。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TraceStep {
    /// 使用的工具名称，如 `"browser_navigate"`。
    pub tool: String,
    /// 工具入参（JSON 对象）。
    pub input: serde_json::Value,
    /// 执行结果的文本摘要。
    pub output_summary: String,
}

/// 子 agent 最终输出中的结构化 JSON 载荷。
///
/// 对应 content 中 ` ```json ``` ` 代码块内的对象。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Step2Payload {
    /// 执行状态，如 `"success"`、`"partial"`、`"error"`。
    pub status: String,
    /// 工具调用轨迹，按执行顺序排列。
    pub execution_trace: Vec<TraceStep>,
    /// 压缩上下文，用于传递给下一轮执行。
    pub compressed_context: String,
}

/// JSON 提取结果。
#[derive(Debug, Clone, PartialEq)]
pub enum ExtractResult {
    /// 成功提取并解析出 payload，附带去除 JSON 块后的纯文本描述部分。
    Ok {
        /// 结构化载荷。
        payload: Step2Payload,
        /// JSON 块之前的文本描述部分（已 trim）。
        description: String,
    },
    /// 文本中未找到 `json` 代码块。
    NoJsonBlock,
    /// 找到了代码块但 JSON 解析失败。
    ParseError(String),
}

// ────────────────────────── 提取逻辑 ──────────────────────────

/// 从 Markdown 混合文本中提取 ` ```json ... ``` ` 代码块并解析为 [`Step2Payload`]。
///
/// 支持的格式：
/// - 任意前导文本 + `\`\`\`json\n{...}\n\`\`\`` 后缀
/// - 代码块标记前后可有空白
pub fn extract_step2_payload(text: &str) -> ExtractResult {
    // 查找 ```json 开始标记
    let lower = text.to_ascii_lowercase();
    let block_start = match lower.find("```json") {
        Some(pos) => pos,
        None => return ExtractResult::NoJsonBlock,
    };

    // 从 ```json 之后开始找结束的 ```
    let after_start = block_start + "```json".len();
    // 跳过 ```json 后可能存在的换行符
    let rest = &text[after_start..];
    let trimmed_start = rest.trim_start_matches(|c: char| c == '\r' || c == '\n');
    let json_start = after_start + (rest.len() - trimmed_start.len());

    // 找结束的 ```
    let json_end = match trimmed_start.find("```") {
        Some(pos) => json_start + pos,
        None => return ExtractResult::ParseError("未找到结束的 ``` 标记".to_string()),
    };

    let json_str = text[json_start..json_end].trim();

    // 提取描述文本（JSON 块之前的部分）
    let description = text[..block_start]
        .trim()
        .to_string();

    // 解析 JSON
    match serde_json::from_str::<Step2Payload>(json_str) {
        Ok(payload) => ExtractResult::Ok { payload, description },
        Err(e) => ExtractResult::ParseError(format!("JSON 解析失败: {e}")),
    }
}

// ────────────────────────── 测试 ──────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个完整的 agent 输出文本（与用户提供的示例一致）。
    fn sample_content() -> String {
        r#"已在百度资讯板块获取到「达州安仁乡」的新闻列表（共14条相关资讯）。前三篇新闻信息如下：

1. **四川省达州市达川区气象台发布高温橙色预警信号**（3天前，来源：湘湘带你看社会）
2. **四川省达州市达川区发布高温橙色预警信号**（4天前，来源：北青网）
3. **四川省达州市达川区发布高温橙色预警信号**（5天前，来源：北青网）

```json
{
  "status": "success",
  "execution_trace": [
    {
      "tool": "browser_navigate",
      "input": { "url": "https://www.baidu.com" },
      "output_summary": "成功打开百度首页，页面标题为「百度一下，你就知道」，页面正常加载。"
    },
    {
      "tool": "browser_type",
      "input": { "target": "e36", "text": "达州安仁乡", "element": "百度搜索框" },
      "output_summary": "已向搜索框输入关键词「达州安仁乡」。"
    },
    {
      "tool": "browser_press_key",
      "input": { "key": "Enter" },
      "output_summary": "提交搜索，跳转至搜索结果页「达州安仁乡_百度搜索」。"
    },
    {
      "tool": "browser_snapshot",
      "input": {},
      "output_summary": "搜索结果页加载完成，顶部导航栏包含「网页、图片、资讯、视频、笔记、地图、贴吧、文库」等板块，定位到「资讯」入口。"
    },
    {
      "tool": "browser_click",
      "input": { "target": "e187", "element": "资讯板块标签" },
      "output_summary": "成功点击「资讯」标签，跳转至百度资讯搜索页（URL 含 tn=news），页面标题为「百度资讯搜索_达州安仁乡」。"
    },
    {
      "tool": "browser_snapshot",
      "input": {},
      "output_summary": "资讯板块显示「百度为您找到相关资讯14个」，按焦点排序提取前三篇新闻：1.《四川省达州市达川区气象台发布高温橙色预警信号》发布时间3天前（来源：湘湘带你看社会）；2.《四川省达州市达川区发布高温橙色预警信号》发布时间4天前（来源：北青网）；3.《四川省达州市达川区发布高温橙色预警信号》发布时间5天前（来源：北青网）。"
    }
  ],
  "compressed_context": "任务成功：百度资讯搜索达州安仁乡，提取前3篇新闻均为高温预警(3/4/5天前)，无高风险操作被拒。"
}
```"#
            .to_string()
    }

    // ── 基本提取 ──

    #[test]
    fn extract_from_full_content() {
        let content = sample_content();
        let result = extract_step2_payload(&content);

        match &result {
            ExtractResult::Ok { payload, description } => {
                assert_eq!(payload.status, "success");
                assert_eq!(payload.execution_trace.len(), 6);
                assert!(description.starts_with("已在百度资讯板块获取到"));
                assert!(description.contains("高温橙色预警信号"));
                assert!(!description.contains("```json"));
            }
            other => panic!("期望 Ok，实际: {other:?}"),
        }
    }

    #[test]
    fn trace_steps_have_correct_tools() {
        let content = sample_content();
        let result = extract_step2_payload(&content);

        if let ExtractResult::Ok { payload, .. } = result {
            let tools: Vec<&str> = payload.execution_trace.iter().map(|s| s.tool.as_str()).collect();
            assert_eq!(
                tools,
                vec![
                    "browser_navigate",
                    "browser_type",
                    "browser_press_key",
                    "browser_snapshot",
                    "browser_click",
                    "browser_snapshot",
                ]
            );
        }
    }

    #[test]
    fn compressed_context_is_preserved() {
        let content = sample_content();
        if let ExtractResult::Ok { payload, .. } = extract_step2_payload(&content) {
            assert!(payload.compressed_context.contains("达州安仁乡"));
            assert!(payload.compressed_context.contains("高温预警"));
        }
    }

    // ── 边界情况 ──

    #[test]
    fn no_json_block() {
        let text = "这是一段纯文本，没有任何代码块。";
        assert_eq!(extract_step2_payload(text), ExtractResult::NoJsonBlock);
    }

    #[test]
    fn non_json_code_block() {
        let text = "看看这段代码：\n```rust\nfn main() {}\n```";
        assert_eq!(extract_step2_payload(text), ExtractResult::NoJsonBlock);
    }

    #[test]
    fn json_block_without_fence() {
        // 只有裸 JSON，没有 ```json 包裹
        let text = r#"{"status":"ok","execution_trace":[],"compressed_context":""}"#;
        assert_eq!(extract_step2_payload(text), ExtractResult::NoJsonBlock);
    }

    #[test]
    fn invalid_json_in_block() {
        let text = "前导文本\n```json\n{ not valid json }\n```";
        match extract_step2_payload(text) {
            ExtractResult::ParseError(msg) => assert!(msg.contains("JSON 解析失败")),
            other => panic!("期望 ParseError，实际: {other:?}"),
        }
    }

    #[test]
    fn missing_required_field() {
        // 缺少 execution_trace
        let text = r#"```json
{
  "status": "success",
  "compressed_context": "完成"
}
```"#;
        match extract_step2_payload(text) {
            ExtractResult::ParseError(msg) => assert!(msg.contains("JSON 解析失败")),
            other => panic!("期望 ParseError，实际: {other:?}"),
        }
    }

    #[test]
    fn empty_trace_array() {
        let text = r#"```json
{
  "status": "success",
  "execution_trace": [],
  "compressed_context": "无需工具调用"
}
```"#;
        match extract_step2_payload(text) {
            ExtractResult::Ok { payload, .. } => {
                assert_eq!(payload.execution_trace.len(), 0);
                assert_eq!(payload.compressed_context, "无需工具调用");
            }
            other => panic!("期望 Ok，实际: {other:?}"),
        }
    }

    #[test]
    fn json_block_with_surrounding_whitespace() {
        let text = "   \n  ```json  \n  \n{\"status\":\"ok\",\"execution_trace\":[],\"compressed_context\":\"\"}\n  ```   \n  ";
        match extract_step2_payload(text) {
            ExtractResult::Ok { payload, .. } => assert_eq!(payload.status, "ok"),
            other => panic!("期望 Ok，实际: {other:?}"),
        }
    }

    #[test]
    fn description_only_blank_lines() {
        // JSON 块前只有空白
        let text = "\n\n```json\n{\"status\":\"ok\",\"execution_trace\":[],\"compressed_context\":\"\"}\n```";
        match extract_step2_payload(text) {
            ExtractResult::Ok { description, .. } => {
                assert!(description.is_empty(), "空白描述应被 trim 为空字符串");
            }
            other => panic!("期望 Ok，实际: {other:?}"),
        }
    }

    #[test]
    fn multiple_json_blocks_takes_first() {
        let text = r#"第一次输出：
```json
{"status":"ok","execution_trace":[],"compressed_context":"第一次"}
```

第二次输出：
```json
{"status":"ok","execution_trace":[],"compressed_context":"第二次"}
```"#;
        match extract_step2_payload(text) {
            ExtractResult::Ok { payload, .. } => {
                assert_eq!(payload.compressed_context, "第一次");
            }
            other => panic!("期望 Ok，实际: {other:?}"),
        }
    }

    #[test]
    fn trace_step_input_is_json_value() {
        let text = r#"```json
{
  "status": "success",
  "execution_trace": [
    {
      "tool": "browser_type",
      "input": { "target": "e36", "text": "查询" },
      "output_summary": "输入完成"
    }
  ],
  "compressed_context": "ok"
}
```"#;
        match extract_step2_payload(text) {
            ExtractResult::Ok { payload, .. } => {
                let step = &payload.execution_trace[0];
                assert_eq!(step.tool, "browser_type");
                assert_eq!(step.input["target"], "e36");
                assert_eq!(step.input["text"], "查询");
                assert_eq!(step.output_summary, "输入完成");
            }
            other => panic!("期望 Ok，实际: {other:?}"),
        }
    }

    #[test]
    fn mixed_crlf_line_endings() {
        // Windows 换行符 \r\n
        let text = "描述文本\r\n```json\r\n{\"status\":\"ok\",\"execution_trace\":[],\"compressed_context\":\"crlf\"}\r\n```";
        match extract_step2_payload(text) {
            ExtractResult::Ok { payload, .. } => assert_eq!(payload.compressed_context, "crlf"),
            other => panic!("期望 Ok，实际: {other:?}"),
        }
    }

    #[test]
    fn input_field_with_nested_object_and_array() {
        let text = r#"```json
{
  "status": "success",
  "execution_trace": [
    {
      "tool": "complex_tool",
      "input": {
        "config": { "depth": 3 },
        "items": ["a", "b"]
      },
      "output_summary": "done"
    }
  ],
  "compressed_context": "nested"
}
```"#;
        match extract_step2_payload(text) {
            ExtractResult::Ok { payload, .. } => {
                let input = &payload.execution_trace[0].input;
                assert_eq!(input["config"]["depth"], 3);
                assert_eq!(input["items"][0], "a");
                assert_eq!(input["items"][1], "b");
            }
            other => panic!("期望 Ok，实际: {other:?}"),
        }
    }

    #[test]
    fn json_block_case_insensitive_start() {
        // ```JSON (大写)
        let text = "text\n```JSON\n{\"status\":\"ok\",\"execution_trace\":[],\"compressed_context\":\"upper\"}\n```";
        match extract_step2_payload(text) {
            ExtractResult::Ok { payload, .. } => assert_eq!(payload.compressed_context, "upper"),
            other => panic!("期望 Ok，实际: {other:?}"),
        }
    }
}
