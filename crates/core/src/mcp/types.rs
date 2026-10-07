use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MCP 工具定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// MCP 工具调用结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub content: Value,
    pub is_error: bool,
}

impl ToolResult {
    /// 把 `content` 解析成内容块序列（**借用视图**，零拷贝）。
    ///
    /// [`content`](Self::content) 是异构开放容器 —— 内置工具 / 子 agent 往里塞自定义
    /// JSON，只有 `planned-agent-mcp-rmcp` 会产内容块。本方法把那一种 payload 的解析
    /// 收敛到一处，消费方不必再手写 `block.get("data").and_then(Value::as_str)`。
    ///
    /// 返回 `None` = 这个 payload 不是内容块序列（自定义 JSON 的常态）。
    pub fn content_blocks(&self) -> Option<Vec<ContentBlock<'_>>> {
        parse_content_blocks(&self.content)
    }

    /// 把工具结果文本化，**图片块降级为 `[图片]` 占位**。
    ///
    /// - 不含图片的结果**原样返回**（`Value::String` 仍是 `Value::String`，逐字不变）；
    /// - 含图片块时，图片块换成占位，其余块保留。
    ///
    /// 用途：只需要文本的消费路径（chat / ReAct）必须用它，绝不能直接 `to_string()`
    /// —— 那会把 base64 当成文本灌进 LLM 上下文与 UI 事件。
    /// 需要把图片落盘再交给模型的路径（flexible 单步循环）先落盘，再走这里。
    pub fn sanitized_content(&self) -> Value {
        let Some(blocks) = self.content_blocks() else {
            return self.content.clone();
        };
        if !blocks
            .iter()
            .any(|block| matches!(block, ContentBlock::Image { .. }))
        {
            return self.content.clone();
        }
        Value::Array(
            blocks
                .iter()
                .map(|block| match block {
                    ContentBlock::Image { .. } => Value::String("[图片]".to_string()),
                    ContentBlock::Text(text) => Value::String((*text).to_string()),
                    ContentBlock::Unknown(value) => (*value).clone(),
                })
                .collect(),
        )
    }
}

/// 内容块（**借用视图**）。
///
/// [`ToolResult::content`] 是 `Value`：它是异构开放容器，内容块只是它承载的一种
/// payload。本枚举把那一种 payload 的解析收敛到一处。
///
/// **没有** `Resource` / `Audio` 变体：适配层（`planned-agent-mcp-rmcp` 的
/// `convert_content`）已经把非 text/image 的块压成了占位文本，类型信息在那一步就丢了，
/// 视图无法恢复。将来若适配层改为保留对象形态，再来加变体。
///
/// 故意**不**标 `#[non_exhaustive]`：这是同一个工作区内部的类型，加变体时就让编译器
/// 把全部消费方点出来，而不是让它们静默走兵底分支。
#[derive(Debug, Clone, Copy)]
pub enum ContentBlock<'a> {
    /// 文本块。裸字符串（适配层当前产出）与 `{"type":"text","text":…}` 对象都归这里。
    Text(&'a str),
    /// 图片块：`{"type":"image","mime_type":…,"data":"<base64>"}`。
    Image { mime_type: &'a str, data: &'a str },
    /// 认不出的块 —— 原样带出来，由消费方自行决定（通常 `to_string()` 兜底）。
    Unknown(&'a Value),
}

/// 解析 `content` 为内容块序列（[`ToolResult::content_blocks`] 的自由函数版本）。
///
/// - `Value::String` → 单个 [`ContentBlock::Text`]（适配层已把多文本块 join 成一个字符串，
///   与内置工具的纯文本结果形态相同，无需也无法区分）；
/// - `Value::Array` → 逐块解析；
/// - 其它（对象 / 数字 / `Null`）→ `None`，这些 payload 没有统一的块语义。
pub fn parse_content_blocks(content: &Value) -> Option<Vec<ContentBlock<'_>>> {
    match content {
        Value::String(text) => Some(vec![ContentBlock::Text(text)]),
        Value::Array(blocks) => Some(blocks.iter().map(parse_content_block).collect()),
        _ => None,
    }
}

fn parse_content_block(block: &Value) -> ContentBlock<'_> {
    if let Some(text) = block.as_str() {
        return ContentBlock::Text(text);
    }
    match block.get("type").and_then(Value::as_str) {
        Some("image") => ContentBlock::Image {
            mime_type: block
                .get("mime_type")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            data: block.get("data").and_then(Value::as_str).unwrap_or_default(),
        },
        Some("text") => ContentBlock::Text(
            block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
        _ => ContentBlock::Unknown(block),
    }
}

/// MCP 服务器配置（支持多个）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub server_command: String,
    pub server_args: Vec<String>,
    pub transport: String,
    pub timeout_secs: Option<u64>,
    /// 握手阶段超时（秒）。`None` 时使用 client 默认值（如 30s）。
    ///
    /// 与 [`McpServerConfig::timeout_secs`] 的关系：
    /// - `timeout_secs` 是**冷启动总上限**（spawn 子进程 → npx 首次拉包 → initialize 握手），
    ///   首次拉包慢时调大它；
    /// - `handshake_timeout_secs` 是**进程激活前的提前失败线**：在子进程**没有任何输出**
    ///   （stderr 无数据）的情况下，连接必须在此时限内完成，否则快速判失败；
    ///   一旦子进程产生输出（进程已激活，如 npx 开始拉包 / 输出日志），
    ///   改为按 `timeout_secs` 耐心等待。
    #[serde(default)]
    pub handshake_timeout_secs: Option<u64>,
    pub max_retries: Option<u32>,
    pub is_default: bool,
    pub tools_filter: Option<Vec<String>>,
    /// 工具分类（可选，用于分类过滤）
    #[serde(default)]
    pub categories: Option<Vec<String>>,
}

/// 连接状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub last_ping: Option<chrono::DateTime<chrono::Utc>>,
    pub error_count: u32,
    pub uptime_secs: u64,
    /// 最近一次连接失败的结构化错误（成功连接后会被清空）。
    /// UI 层据此展示失败原因；当前未消费，保留作为数据出口。
    #[serde(default)]
    pub last_error: Option<ConnectionError>,
}

/// 连接失败原因分类
///
/// 用途：MCP 客户端 `connect()` 失败时按失败阶段分类记录，
/// 供上层（UI、监控、Agent 自愈）按类别决定处理策略。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectionError {
    /// 启动/握手总耗时超过 `timeout_secs`。
    /// 覆盖完整冷启动链：spawn 子进程 → npx 拉包 → 真实 MCP server 启动 → initialize 握手。
    /// 首次 `npx -y <pkg>` 下载可能耗时数十秒，需要给足余量。
    Timeout {
        /// 实际等待的秒数
        elapsed_secs: u64,
        /// 配置的超时上限（秒）
        timeout_secs: u64,
        /// 子进程 stderr 末尾输出（如果可读且非空）。
        /// 当子进程启动后被超时打断时，这里通常包含真实的报错原因（如 `MODULE_NOT_FOUND`）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_tail: Option<String>,
    },
    /// 无法启动子进程：命令不存在、权限不足等。
    /// 此阶段子进程未运行，无 stderr 可捕获。
    Spawn {
        reason: String,
    },
    /// 进程已启动但 MCP initialize 握手失败（如子进程崩溃、协议错误）。
    /// `stderr_tail` 携带子进程 stderr 末尾输出，便于 UI 展示真实失败原因。
    Handshake {
        reason: String,
        /// 子进程 stderr 末尾输出（如果可读且非空）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_tail: Option<String>,
    },
}

impl ConnectionError {
    /// 机器可读的失败分类（用于持久化 / IPC / UI 标签）
    ///
    /// 返回字符串与 serde `tag` 字段保持一致（`"timeout"` / `"spawn"` / `"handshake"`），
    /// 便于后续 JSON 字段直读。
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Timeout { .. } => "timeout",
            Self::Spawn { .. } => "spawn",
            Self::Handshake { .. } => "handshake",
        }
    }

    /// 供 UI / 日志展示的人类可读消息
    pub fn message(&self) -> String {
        let base = match self {
            Self::Timeout { elapsed_secs, timeout_secs, .. } => format!(
                "MCP server startup timed out after {}s (limit {}s). \
                 This covers the full cold-start chain: process spawn, npx package \
                 download on first run, and the MCP initialize handshake. \
                 If first-run package download is slow, raise `timeout_secs`; \
                 if the process starts but the handshake stalls (no output from the \
                 subprocess), raise `handshake_timeout_secs` in the server config.",
                elapsed_secs, timeout_secs
            ),
            Self::Spawn { reason } => {
                format!("Failed to spawn MCP server process: {}", reason)
            }
            Self::Handshake { reason, .. } => {
                format!("MCP handshake failed: {}", reason)
            }
        };

        // 追加子进程 stderr 末尾（让用户看到真实失败原因）
        let stderr = match self {
            Self::Timeout { stderr_tail, .. } => stderr_tail.as_ref(),
            Self::Handshake { stderr_tail, .. } => stderr_tail.as_ref(),
            Self::Spawn { .. } => None,
        };
        match stderr {
            Some(s) if !s.trim().is_empty() => {
                format!("{}\n\n--- subprocess stderr ---\n{}", base, s.trim_end())
            }
            _ => base,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn result(content: Value) -> ToolResult {
        ToolResult {
            call_id: "c".to_string(),
            content,
            is_error: false,
        }
    }

    #[test]
    fn plain_text_content_is_untouched() {
        let result = result(Value::String("hello".to_string()));
        assert_eq!(result.sanitized_content(), Value::String("hello".to_string()));
    }

    #[test]
    fn structured_content_is_untouched() {
        let content = json!({ "content": "hi", "lines": 3 });
        let result = result(content.clone());
        assert_eq!(result.sanitized_content(), content);
    }

    #[test]
    fn image_blocks_become_placeholders_and_base64_disappears() {
        let result = result(json!([
            { "type": "text", "text": "shot taken" },
            { "type": "image", "mime_type": "image/png", "data": "BIGBASE64" }
        ]));

        let sanitized = result.sanitized_content();
        let blocks = sanitized.as_array().expect("应当是数组");
        // 文本块被规范化成裸字符串（与适配层产出的线格式一致）—— 见 `ContentBlock::Text`。
        assert_eq!(blocks[0], Value::String("shot taken".to_string()));
        assert_eq!(blocks[1], Value::String("[图片]".to_string()));
        assert!(
            !sanitized.to_string().contains("BIGBASE64"),
            "base64 不得残留：{sanitized}"
        );
    }

    #[test]
    fn array_without_images_is_untouched() {
        let content = json!(["first", "second"]);
        let result = result(content.clone());
        assert_eq!(result.sanitized_content(), content);
    }

    #[test]
    fn content_blocks_parses_text_and_image_and_unknown() {
        let result = result(json!([
            "bare text",
            { "type": "text", "text": "object text" },
            { "type": "image", "mime_type": "image/png", "data": "AAA" },
            { "type": "something-new", "x": 1 }
        ]));

        let blocks = result.content_blocks().expect("数组应当能解析");
        assert_eq!(blocks.len(), 4);
        assert!(matches!(blocks[0], ContentBlock::Text("bare text")));
        assert!(matches!(blocks[1], ContentBlock::Text("object text")));
        assert!(matches!(
            blocks[2],
            ContentBlock::Image {
                mime_type: "image/png",
                data: "AAA"
            }
        ));
        assert!(matches!(blocks[3], ContentBlock::Unknown(_)));
    }

    #[test]
    fn content_blocks_treats_plain_string_as_single_text_block() {
        let result = result(Value::String("hello".to_string()));
        let blocks = result.content_blocks().expect("裸字符串也是文本块");
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0], ContentBlock::Text("hello")));
    }

    #[test]
    fn content_blocks_rejects_non_block_payloads() {
        // 内置工具 / 子 agent 的自定义 JSON：不是块序列。
        let structured = result(json!({ "stdout": "x", "exit_code": 0 }));
        assert!(structured.content_blocks().is_none());
        assert!(result(Value::Null).content_blocks().is_none());
        assert!(result(json!(42)).content_blocks().is_none());
    }
}
