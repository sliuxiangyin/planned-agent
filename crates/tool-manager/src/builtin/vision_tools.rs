//! 视觉识别工具族：调用 AI 读一张本地图片并返回文本。
//!
//! 设计依据：`docs/planned-agent/image-recognition-tool.md`。
//!
//! 边界：
//! - **只做「图 → 文本」**，不返回图片数据、不做版面/颜色理解；
//! - 读盘**必须经过 filesystem 工具族的 cap-std 沙箱句柄**（[`FilesystemService`]）：
//!   只用 `resolve` 校验再让适配层按字符串路径读盘会留下 TOCTOU 窗口
//!   （中间目录被换成指向沙箱外的链接即可逃逸）；
//! - 图片挂在 **user** 消息的 image part 上：OpenAI 协议里 tool 消息只装文本，
//!   图片只能走 user（`crates/ai-openai/src/client.rs:457-464` 与 `:398-428`）。

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use planned_agent_core::ai::types::{
    ChatCompletionRequest, ContentPart, ImageDetail, ImageSource, Message, MessageContent,
    MessageRole,
};
use planned_agent_core::ai::AiClient;
use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::{BuiltinToolProvider, ToolCategory, ToolExecutor};

use super::filesystem::core::FilesystemService;
use super::filesystem::support::{failure, tool_result};

/// 单次识别的墙钟上限。
///
/// **必须自带**：flexible 的 `llm_timeout` 只管「一次 `chat_completion` 调用」，
/// 工具内部的这次调用发生在两次调用**之间**，不在它的覆盖范围内
/// （`crates/planned-agent/src/flexible/exec/executor/config.rs:47-57`）。
const RECOGNIZE_TIMEOUT_SECS: u64 = 60;

/// 识别结果上限：验证码 / 二维码这类结果应当很短，超长说明模型跑偏。
const MAX_RESULT_CHARS: usize = 4_000;

/// 图片字节上限，与 ai-openai 的 `MAX_IMAGE_BYTES` 对齐
/// （`crates/ai-openai/src/client.rs:186`）—— 提前拦，错误码才可控。
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// 缺省识别指令。
const DEFAULT_INSTRUCTION: &str = "读取图片中的文字，只输出识别结果，不要解释。";

/// 允许发给模型的图片 MIME。按**内容嗅探**判定（不看扩展名）——
/// 与 ai-openai 的 `image_mime_of` 白名单等价（`client.rs:189`）。
const SUPPORTED_MIME: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

/// 视觉识别工具提供者。
pub struct VisionToolsProvider {
    ai: Arc<dyn AiClient>,
    service: Arc<FilesystemService>,
}

impl VisionToolsProvider {
    /// `allowed_root` = 允许读取的目录（宿主通常传 cache 产出区）。
    ///
    /// 目录不存在时自动创建 —— [`FilesystemService::try_new`] 会 `canonicalize`，
    /// 目录不存在会直接失败。
    pub fn new(ai: Arc<dyn AiClient>, allowed_root: &Path) -> Result<Self> {
        std::fs::create_dir_all(allowed_root).map_err(|error| {
            anyhow::anyhow!("视觉工具目录「{}」不可用：{error}", allowed_root.display())
        })?;
        let service = FilesystemService::try_new(&[allowed_root.to_path_buf()])
            .map_err(|error| anyhow::anyhow!("视觉工具初始化失败：{}", error.message()))?;
        Ok(Self {
            ai,
            service: Arc::new(service),
        })
    }
}

impl BuiltinToolProvider for VisionToolsProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![(
            Tool {
                name: "builtin_recognize_image".to_string(),
                description: "用 AI 识别一张本地图片并返回文本（读取验证码、二维码、截图中的文字等）。\
                    path 必须是本机已存在的图片文件路径，通常来自上一个工具的输出。"
                    .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "本地图片文件路径（png / jpg / jpeg / webp / gif）"
                        },
                        "instruction": {
                            "type": "string",
                            "description": "识别要求；缺省为「读取图片中的文字，只输出识别结果」"
                        }
                    },
                    "required": ["path"]
                }),
            },
            // 归 Utility 的理由与注意点见设计稿 §5.2。
            vec![ToolCategory::Utility],
        )]
    }

    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(VisionToolsExecutor {
            ai: self.ai.clone(),
            service: self.service.clone(),
        })
    }
}

/// 视觉识别执行器。
struct VisionToolsExecutor {
    ai: Arc<dyn AiClient>,
    service: Arc<FilesystemService>,
}

#[async_trait]
impl ToolExecutor for VisionToolsExecutor {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        // 与 filesystem / system 工具族同一约定：可预期失败返回
        // `Ok(ToolResult { is_error: true, content: {error, message} })`，
        // `Err` 只留给「未知工具名」这类编程错误。
        Ok(match tool_name {
            "builtin_recognize_image" => recognize_image(&self.ai, &self.service, &arguments).await,
            _ => return Err(anyhow::anyhow!("Unknown tool: {}", tool_name)),
        })
    }

    fn name(&self) -> &str {
        "builtin_vision_tools"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec!["builtin_recognize_image".to_string()]
    }
}

// ── builtin_recognize_image ─────────────────────────────────────────────────

async fn recognize_image(
    ai: &Arc<dyn AiClient>,
    service: &FilesystemService,
    arguments: &Value,
) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let path = Path::new(path_str);

    // ① 路径校验：越界即拒（安全边界见设计稿 §5.4）。
    let resolved = match service.resolve(path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };

    // ② 存在性 / 类型 / 大小。**读盘前**判，避免把 ai-openai 的 IO 错误
    //    拖成整次 chat 请求失败（`client.rs:216` 明确不降级）。
    let metadata = match resolved.dir.metadata(&resolved.rel) {
        Ok(metadata) => metadata,
        Err(_) => return failure("file_not_found", format!("图片不存在或不可读：{path_str}")),
    };
    if !metadata.is_file() {
        return failure("invalid_arguments", format!("不是文件：{path_str}"));
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return failure(
            "image_too_large",
            format!(
                "图片 {} 字节，超过上限 {} 字节",
                metadata.len(),
                MAX_IMAGE_BYTES
            ),
        );
    }

    // ③ 读字节：走 cap-std 句柄，不按字符串路径重新打开（防 TOCTOU 逃逸）。
    let bytes = match resolved.dir.read(&resolved.rel) {
        Ok(bytes) => bytes,
        Err(error) => return failure("read_failed", format!("读取图片失败：{error}")),
    };

    // ④ MIME 按内容嗅探（不看扩展名）。
    let Some(mime) = infer::get(&bytes).map(|kind| kind.mime_type()) else {
        return failure(
            "unsupported_image_type",
            format!("无法识别的图片格式（支持：{}）", SUPPORTED_MIME.join(" / ")),
        );
    };
    if !SUPPORTED_MIME.contains(&mime) {
        return failure(
            "unsupported_image_type",
            format!("不支持的图片格式「{mime}」（支持：{}）", SUPPORTED_MIME.join(" / ")),
        );
    }

    let instruction = arguments
        .get("instruction")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_INSTRUCTION);

    let request = ChatCompletionRequest {
        model: ai.model_name().to_string(),
        messages: vec![Message {
            role: MessageRole::User,
            content: Some(MessageContent::Parts {
                parts: vec![
                    ContentPart::Text {
                        text: instruction.to_string(),
                    },
                    ContentPart::Image {
                        image: ImageSource::Url {
                            url: to_data_url(mime, &bytes),
                            detail: Some(ImageDetail::High),
                        },
                    },
                ],
            }),
            ..Default::default()
        }],
        // 内部调用**不带 tools**：否则模型可能回 tool_calls 而不是文本。
        tools: None,
        temperature: None,
        max_tokens: None,
        stream: false,
        extra: Default::default(),
    };

    let response = match tokio::time::timeout(
        Duration::from_secs(RECOGNIZE_TIMEOUT_SECS),
        ai.chat_completion(request),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            tracing::warn!(
                target: "tool_audit",
                tool = "builtin_recognize_image",
                path = %path_str,
                error = %format!("{error:#}"),
                "图片识别调用 AI 失败"
            );
            return failure("ai_request_failed", format!("{error:#}"));
        }
        Err(_) => {
            return failure(
                "timeout",
                format!("识别超时（超过 {RECOGNIZE_TIMEOUT_SECS} 秒）"),
            )
        }
    };

    let text = response
        .choices
        .first()
        .map(|choice| message_text(&choice.message))
        .unwrap_or_default();
    let text = text.trim();
    if text.is_empty() {
        return failure("empty_result", "模型没有返回任何内容");
    }

    let text = truncate_chars(text, MAX_RESULT_CHARS);
    // 审计只记元信息：**绝不记图片字节 / data URL / 完整识别文本**。
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_recognize_image",
        path = %path_str,
        mime = mime,
        bytes = bytes.len(),
        model = %ai.model_name(),
        chars = text.chars().count(),
        duration_ms = started.elapsed().as_millis() as u64,
        "图片识别完毕"
    );

    tool_result(Value::String(text), false)
}

/// 字节 → `data:{mime};base64,…`（ai-openai 的 `ImageSource::Url` 原样透传）。
fn to_data_url(mime: &str, bytes: &[u8]) -> String {
    use base64::Engine as _;
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// 取 assistant 消息的正文。
fn message_text(message: &Message) -> String {
    match &message.content {
        Some(MessageContent::Text { text }) => text.clone(),
        _ => String::new(),
    }
}

/// 按**字符**（而非字节）截断，UTF-8 安全。
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use planned_agent_core::ai::ChatCompletionStream;

    /// PNG 魔数（`infer` 据此判定 `image/png`）。
    const PNG_HEADER: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    /// 只实现 `chat_completion` 的桩：记录收到的请求，回固定文本。
    struct FakeAiClient {
        seen: Mutex<Vec<ChatCompletionRequest>>,
        reply: String,
    }

    impl FakeAiClient {
        fn new(reply: &str) -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(Vec::new()),
                reply: reply.to_string(),
            })
        }

        fn calls(&self) -> usize {
            self.seen
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .len()
        }

        fn last_request(&self) -> ChatCompletionRequest {
            self.seen
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .last()
                .expect("应当收到过一次请求")
                .clone()
        }
    }

    #[async_trait]
    impl AiClient for FakeAiClient {
        async fn chat_completion(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<planned_agent_core::ai::types::ChatCompletionResponse> {
            self.seen
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(request);
            Ok(planned_agent_core::ai::types::ChatCompletionResponse {
                id: "fake".to_string(),
                object: "chat.completion".to_string(),
                created: 0,
                model: "fake-model".to_string(),
                choices: vec![planned_agent_core::ai::types::Choice {
                    index: 0,
                    message: Message {
                        role: MessageRole::Assistant,
                        content: Some(MessageContent::Text {
                            text: self.reply.clone(),
                        }),
                        ..Default::default()
                    },
                    finish_reason: None,
                    logprobs: None,
                }],
                usage: None,
                system_fingerprint: None,
            })
        }

        async fn chat_completion_stream(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<ChatCompletionStream> {
            unimplemented!("视觉识别工具不使用流式接口")
        }

        fn provider_name(&self) -> &str {
            "fake"
        }

        fn model_name(&self) -> &str {
            "fake-model"
        }

        fn default_config(&self) -> ChatCompletionRequest {
            unimplemented!("测试桩不提供默认配置")
        }
    }

    fn provider_in(dir: &Path, reply: &str) -> (VisionToolsProvider, Arc<FakeAiClient>) {
        let ai = FakeAiClient::new(reply);
        let provider = VisionToolsProvider::new(ai.clone(), dir).expect("provider 构造应当成功");
        (provider, ai)
    }

    async fn run(provider: &VisionToolsProvider, arguments: Value) -> ToolResult {
        provider
            .executor()
            .execute("builtin_recognize_image", arguments)
            .await
            .expect("工具执行不应返回 Err")
    }

    fn write_png(dir: &Path, name: &str) -> std::path::PathBuf {
        let target = dir.join(name);
        std::fs::write(&target, PNG_HEADER).expect("写文件");
        target
    }

    #[test]
    fn exposes_builtin_recognize_image_in_utility() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, _ai) = provider_in(dir.path(), "x");
        let tools = provider.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].0.name, "builtin_recognize_image");
        assert_eq!(tools[0].1, vec![ToolCategory::Utility]);
        assert_eq!(
            provider.executor().supported_tools(),
            vec!["builtin_recognize_image".to_string()]
        );
    }

    #[tokio::test]
    async fn missing_path_is_invalid_arguments() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, _ai) = provider_in(dir.path(), "x");
        let result = run(&provider, json!({})).await;
        assert!(result.is_error);
        assert_eq!(result.content["error"], "invalid_arguments");
    }

    #[tokio::test]
    async fn unsupported_content_is_rejected_without_ai_call() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, ai) = provider_in(dir.path(), "x");
        // 扩展名伪装成 png，内容却是文本 → 按**内容**判定为不支持。
        let target = dir.path().join("fake.png");
        std::fs::write(&target, b"not an image at all").expect("写文件");

        let result = run(&provider, json!({ "path": target })).await;
        assert!(result.is_error);
        assert_eq!(result.content["error"], "unsupported_image_type");
        assert_eq!(ai.calls(), 0, "格式不支持时不应发起 AI 调用");
    }

    #[tokio::test]
    async fn missing_file_is_file_not_found() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, ai) = provider_in(dir.path(), "x");
        let target = dir.path().join("ghost.png");

        let result = run(&provider, json!({ "path": target })).await;
        assert!(result.is_error);
        assert_eq!(result.content["error"], "file_not_found");
        assert_eq!(ai.calls(), 0);
    }

    #[tokio::test]
    async fn directory_is_rejected_as_invalid_arguments() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, ai) = provider_in(dir.path(), "x");
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).expect("建目录");

        let result = run(&provider, json!({ "path": nested })).await;
        assert!(result.is_error);
        assert_eq!(result.content["error"], "invalid_arguments");
        assert_eq!(ai.calls(), 0);
    }

    #[tokio::test]
    async fn path_outside_allowed_root_is_rejected() {
        let allowed = tempfile::tempdir().expect("允许目录");
        let outside = tempfile::tempdir().expect("越界目录");
        let (provider, ai) = provider_in(allowed.path(), "x");
        let target = outside.path().join("captcha.png");
        std::fs::write(&target, PNG_HEADER).expect("写文件");

        let result = run(&provider, json!({ "path": target })).await;
        assert!(result.is_error);
        assert_eq!(result.content["error"], "path_outside_allowed");
        assert_eq!(ai.calls(), 0, "越界路径不应发起 AI 调用");
    }

    #[tokio::test]
    async fn success_sends_data_url_image_on_user_message() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, ai) = provider_in(dir.path(), "  4F7K\n");
        let target = write_png(dir.path(), "captcha.png");

        let result = run(&provider, json!({ "path": target })).await;

        assert!(!result.is_error, "成功结果不应是 is_error：{result:?}");
        assert_eq!(result.content, Value::String("4F7K".to_string()));

        let request = ai.last_request();
        assert_eq!(request.model, "fake-model");
        assert!(request.tools.is_none(), "内部调用不得带 tools");
        assert_eq!(request.messages.len(), 1);
        let message = &request.messages[0];
        assert!(matches!(message.role, MessageRole::User));
        let MessageContent::Parts { parts } = message.content.as_ref().expect("content") else {
            panic!("图片必须走 Parts");
        };
        assert!(matches!(&parts[0], ContentPart::Text { .. }));
        match &parts[1] {
            ContentPart::Image { image } => match image {
                ImageSource::Url { url, detail } => {
                    assert!(
                        url.starts_with("data:image/png;base64,"),
                        "应当是 PNG 的 data URL，实际前缀：{}",
                        &url[..url.len().min(40)]
                    );
                    assert!(matches!(detail, Some(ImageDetail::High)));
                }
                other => panic!("期望 ImageSource::Url，实际 {other:?}"),
            },
            other => panic!("第二段应当是图片，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn custom_instruction_replaces_default() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, ai) = provider_in(dir.path(), "ok");
        let target = write_png(dir.path(), "code.png");

        let _ = run(
            &provider,
            json!({ "path": target, "instruction": "只读出二维码内容" }),
        )
        .await;

        let request = ai.last_request();
        let MessageContent::Parts { parts } = request.messages[0].content.as_ref().unwrap() else {
            panic!("应当走 Parts");
        };
        match &parts[0] {
            ContentPart::Text { text } => assert_eq!(text, "只读出二维码内容"),
            other => panic!("第一段应当是文本，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_reply_is_reported_as_error() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, _ai) = provider_in(dir.path(), "   ");
        let target = write_png(dir.path(), "blank.png");

        let result = run(&provider, json!({ "path": target })).await;
        assert!(result.is_error);
        assert_eq!(result.content["error"], "empty_result");
    }

    #[tokio::test]
    async fn oversized_result_is_truncated() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, _ai) = provider_in(dir.path(), &"识".repeat(MAX_RESULT_CHARS + 50));
        let target = write_png(dir.path(), "long.png");

        let result = run(&provider, json!({ "path": target })).await;
        assert!(!result.is_error);
        assert_eq!(
            result.content.as_str().expect("文本结果").chars().count(),
            MAX_RESULT_CHARS
        );
    }

    #[test]
    fn truncate_chars_counts_chars_not_bytes() {
        let text = "验证码".repeat(10);
        assert_eq!(truncate_chars(&text, 3), "验证码");
        assert_eq!(truncate_chars(&text, 100).chars().count(), 30);
    }

    #[test]
    fn to_data_url_encodes_bytes_as_base64() {
        assert_eq!(
            to_data_url("image/png", PNG_HEADER),
            "data:image/png;base64,iVBORw0KGgo="
        );
    }
}
