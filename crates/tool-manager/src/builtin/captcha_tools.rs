//! 验证码求解工具族：`builtin_solve_captcha`。
//!
//! 设计依据：`docs/planned-agent/captcha-solver-tool.md`（已定稿）。
//!
//! 边界：
//! - v1 **只做字符型**（`kind = "text"`）。拖拽 / 点选按**策略表**扩展：加一种验证码只加一条
//!   [`CaptchaStrategy`] + 一个出参变体，主流程不动（设计稿 §4）。
//! - **后端可插拔**（设计稿 §5）：v1 唯一实现 [`LocalVisionBackend`]（复用主 provider 的视觉
//!   模型）；第三方 API / 本地 ddddocr 接在 [`CaptchaBackend`] 后面，工具契约不变。
//! - 读盘**必须经 filesystem 工具族的 cap-std 沙箱句柄**（[`FilesystemService`]）：只 `resolve`
//!   校验、再让适配层按字符串路径读盘会留下 TOCTOU 窗口（中间目录被换成指向沙箱外的链接即可逃逸）。
//! - 图片挂在 **user** 消息的 image part 上：OpenAI 协议里 tool 消息只装文本。
//! - **内部那次 LLM 调用不受 flexible 的 `llm_timeout` 保护**（它只管「一次 `chat_completion`
//!   调用」，工具内部这次发生在两次之间），所以本工具自带超时。

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

/// 单次求解的墙钟上限。
///
/// **公开**：自定义后端（将来的第三方 API）应当自行遵守同一时限；工具侧另有**同等时长的
/// 外层兜底**（见 [`solve_captcha`]）—— 后端没自带超时也不会拖死整个步骤。
pub const SOLVE_TIMEOUT_SECS: u64 = 60;

/// 字符型结果的长度上限。
///
/// 验证码不可能这么长 —— 超了就是模型跑偏，**判「没认出来」而不是截断**：
/// 截断会给出一个看似合理但**错误**的验证码（设计稿 §3.3）。
const MAX_TEXT_CHARS: usize = 64;

/// 图片字节上限，与 `builtin_recognize_image` / ai-openai 对齐（提前拦，错误码才可控）。
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// 允许发给模型的图片 MIME。按**内容嗅探**判定（不看扩展名）。
const SUPPORTED_MIME: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

/// 缺省的验证码类型。
///
/// 给默认值是合理的：本工具的**领域从名字起就收窄了**（验证码专用），在领域内给最常见形态
/// 一个默认，不等于「替调用方猜通用意图」（对比 `builtin_recognize_image` 的 `instruction`
/// 为什么必须由调用方给 —— 设计稿 §2.1）。
const DEFAULT_KIND: &str = "text";

/// 「认不出来」的哨兵：工具要求模型用它表达，**工具内部消费**，不出现在出参里
/// （出参用 `readable: false` + `text: null`，见 [`parse_text`]）。
const UNREADABLE_SENTINEL: &str = "UNREADABLE";

// ── 策略表：加一种验证码只加这里 ─────────────────────────────────────────────

/// 一种验证码的求解策略。
struct CaptchaStrategy {
    /// **固化**提示词 —— 不给调用方覆盖，这是本工具相对通用读图的收益之一。
    prompt: &'static str,
    /// 该类型需要的图片张数（字符型 1 张；将来的拖拽型是 2 张）。
    expected_images: usize,
    /// 模型原始输出 → 出参（含归一化与 `readable` 判定）。
    parse: fn(&str) -> Value,
}

/// 字符型（图形验证码）的提示词。
///
/// 纯机制描述，**不写任何站点/业务个案** —— 否则工具会过拟合到某一种验证码。
const TEXT_PROMPT: &str = "这是一张验证码图片。请只输出图片中的验证码字符本身：\n\
- 不要解释、不要标点、不要空格、不要引号；\n\
- 如果图中有干扰线、噪点或背景，请忽略它们；\n\
- 如果确实无法辨认，只输出 UNREADABLE。";

/// 当前支持的 `kind`（进错误信息用）。
const SUPPORTED_KINDS: &[&str] = &["text"];

/// 按 `kind` 取策略。**加类型只在这里加一行**（设计稿 §4.3 的 diff 清单）。
fn strategy(kind: &str) -> Option<CaptchaStrategy> {
    match kind {
        "text" => Some(CaptchaStrategy {
            prompt: TEXT_PROMPT,
            expected_images: 1,
            parse: parse_text,
        }),
        _ => None,
    }
}

// ── 出参：tagged union ──────────────────────────────────────────────────────

/// 字符型：归一化 → 判「能不能用」→ tagged union。
fn parse_text(raw: &str) -> Value {
    match normalize_text(raw) {
        Some(text) => json!({ "kind": "text", "text": text, "readable": true }),
        None => json!({ "kind": "text", "text": Value::Null, "readable": false }),
    }
}

/// 保守归一化。**认不出来**返回 `None`。
///
/// 只做明确安全的事：trim、去成对引号、去包裹的代码块围栏、去结尾句号。
///
/// **不做**的事（做了就可能给出错误答案）：
/// - 大小写转换 —— 验证码**可能区分大小写**；
/// - 删除内部空格 —— 有些验证码含空格，且「4 F 7 K」也可能就是答案的形态；
/// - 正则抽取「看起来像验证码的部分」—— 猜错就是错答案。
fn normalize_text(raw: &str) -> Option<String> {
    let text = strip_fence(raw.trim());
    let text = strip_wrapping_quotes(text.trim());
    let text = text.trim().trim_end_matches(['.', '。']).trim();
    // 大小写不敏感：模型偶尔回 `Unreadable` / `unreadable`，漏判会把这个词当答案交出去。
    // （验证码理论上可能恰是这个词，但字符长度与组成都不符，代价远小于漏判。）
    if text.is_empty() || text.eq_ignore_ascii_case(UNREADABLE_SENTINEL) {
        return None;
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return None;
    }
    Some(text.to_string())
}

/// 去掉包裹的代码块围栏（```` ``` ```` 或 `~~~`，允许首行带语言标注）。
///
/// **只在成对时去** —— 只有半个围栏说明模型输出本身不规范，去掉反而可能删掉真内容。
fn strip_fence(text: &str) -> &str {
    for fence in ["```", "~~~"] {
        let Some(rest) = text.strip_prefix(fence) else {
            continue;
        };
        // 首行可能带语言标注（```text），它属于围栏的一部分
        let Some(newline) = rest.find('\n') else {
            continue;
        };
        let body = rest[newline + 1..].trim_end();
        if let Some(inner) = body.strip_suffix(fence) {
            return inner.trim();
        }
    }
    text
}

/// 去掉成对的首尾引号（`"` / `'` / 反引号）。只去**成对**的，避免误删内容。
fn strip_wrapping_quotes(text: &str) -> &str {
    let bytes = text.as_bytes();
    if bytes.len() < 2 {
        return text;
    }
    let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
    if first == last && matches!(first, b'"' | b'\'' | b'`') {
        // 这三个引号都是单字节 ASCII，切片不会落在多字节字符中间
        return &text[1..text.len() - 1];
    }
    text
}

// ── 后端（可插拔；v1 只有本地视觉模型）─────────────────────────────────────

/// 一次求解请求 —— **自定义后端只需要这三样**（不需要知道策略表的存在）。
pub struct SolveRequest<'a> {
    /// 验证码类型（如 `"text"`）
    pub kind: &'a str,
    /// 固定提示词
    pub prompt: &'a str,
    /// 图片字节，按策略要求的顺序：`(mime, bytes)`
    pub images: &'a [(String, Vec<u8>)],
}

/// 后端失败原因 —— 决定对外错误码（`timeout` vs `backend_failed`）。
#[derive(Debug)]
pub enum BackendError {
    /// 超过 [`SOLVE_TIMEOUT_SECS`]
    Timeout,
    /// 其它失败（网络 / provider 报错 / 不支持图片…）
    Failed(anyhow::Error),
}

/// 求解后端。
///
/// v1 唯一实现是 [`LocalVisionBackend`]。第三方商业 API（CapSolver / 2Captcha 等）或本地
/// ddddocr 接在同一个入口后面即可 —— 它们的入出口与本工具的出参形状天然吻合
/// （字符型回文本、点选/拖拽回坐标或索引），**不需要改工具契约**（设计稿 §5.2、§9）。
#[async_trait]
pub trait CaptchaBackend: Send + Sync {
    /// 后端名（进审计日志与错误信息）。
    fn name(&self) -> &str;

    /// 本次实际使用的模型名（进审计日志）。
    ///
    /// 默认 `None` —— 不依赖具体模型的实现（如直接调第三方 API）不用管。
    fn model(&self) -> Option<String> {
        None
    }

    /// 求解。返回**原始模型输出**，不做归一化（归一化属于策略层）。
    ///
    /// **超时**：调用点会用 [`SOLVE_TIMEOUT_SECS`] 再做一层兜底 —— 超出时限的 future 会被直接
    /// cancel（无需自行返回 [`BackendError::Timeout`]，但实现应保证被 cancel 时不留半成品）。
    async fn solve<'a>(&self, request: SolveRequest<'a>) -> std::result::Result<String, BackendError>;
}

/// 本地视觉模型后端：复用主 provider 的 [`AiClient`]。
///
/// 图片不进调用方模型的上下文 —— 只在这一次内部调用里出现（与 `builtin_recognize_image`
/// 同一套纪律）。
struct LocalVisionBackend {
    ai: Arc<dyn AiClient>,
}

impl LocalVisionBackend {
    fn new(ai: Arc<dyn AiClient>) -> Self {
        Self { ai }
    }
}

#[async_trait]
impl CaptchaBackend for LocalVisionBackend {
    fn name(&self) -> &str {
        "local_vision"
    }

    fn model(&self) -> Option<String> {
        Some(self.ai.model_name().to_string())
    }

    async fn solve<'a>(
        &self,
        request: SolveRequest<'a>,
    ) -> std::result::Result<String, BackendError> {
        let chat = build_request(&self.ai.model_name().to_string(), request.prompt, request.images);

        let response = tokio::time::timeout(
            Duration::from_secs(SOLVE_TIMEOUT_SECS),
            self.ai.chat_completion(chat),
        )
        .await
        .map_err(|_| BackendError::Timeout)?
        .map_err(BackendError::Failed)?;

        Ok(response
            .choices
            .first()
            .map(|choice| message_text(&choice.message))
            .unwrap_or_default())
    }
}

/// 构造内部 LLM 请求。
///
/// **不带 `tools`** —— 免得模型返回 tool_calls 分支；提示词与图片都挂 user 消息
/// （协议上 tool 角色装不下图）。抽成纯函数是为了能直接断言请求形状（不必起桩客户端）。
fn build_request(
    model: &str,
    prompt: &str,
    images: &[(String, Vec<u8>)],
) -> ChatCompletionRequest {
    let mut parts = vec![ContentPart::Text {
        text: prompt.to_string(),
    }];
    for (mime, bytes) in images {
        parts.push(ContentPart::Image {
            image: ImageSource::Url {
                url: to_data_url(mime, bytes),
                detail: Some(ImageDetail::High),
            },
        });
    }

    ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: MessageRole::User,
            content: Some(MessageContent::Parts { parts }),
            ..Default::default()
        }],
        tools: None,
        temperature: None,
        max_tokens: None,
        stream: false,
        extra: Default::default(),
    }
}

/// 字节 → `data:{mime};base64,…`（ai-openai 的 `ImageSource::Url` 原样透传）。
///
/// 与 `vision_tools.rs` 同形，**故意各自持有一份**：两个工具族互不依赖，这 8 行不值当为它
/// 建一个共享模块。
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

// ── 工具注册 ────────────────────────────────────────────────────────────────

/// 验证码求解工具提供者。
pub struct CaptchaToolsProvider {
    backend: Arc<dyn CaptchaBackend>,
    service: Arc<FilesystemService>,
}

impl CaptchaToolsProvider {
    /// 用**本地视觉模型**后端（复用主 provider）。
    ///
    /// `allowed_root` = 允许读取的目录（宿主传 cache 产出区，与 `VisionToolsProvider` 同一个）。
    /// 目录不存在时自动创建 —— [`FilesystemService::try_new`] 会 `canonicalize`，目录不存在会直接失败。
    pub fn with_local_vision(ai: Arc<dyn AiClient>, allowed_root: &Path) -> Result<Self> {
        Self::with_backend(Arc::new(LocalVisionBackend::new(ai)), allowed_root)
    }

    /// 换**任意后端**（测试桩；将来接第三方 API / ddddocr 也走这里）。
    pub fn with_backend(backend: Arc<dyn CaptchaBackend>, allowed_root: &Path) -> Result<Self> {
        std::fs::create_dir_all(allowed_root).map_err(|error| {
            anyhow::anyhow!(
                "验证码工具目录「{}」不可用：{error}",
                allowed_root.display()
            )
        })?;
        let service = FilesystemService::try_new(&[allowed_root.to_path_buf()])
            .map_err(|error| anyhow::anyhow!("验证码工具初始化失败：{}", error.message()))?;
        Ok(Self {
            backend,
            service: Arc::new(service),
        })
    }
}

impl BuiltinToolProvider for CaptchaToolsProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![(
            Tool {
                name: "builtin_solve_captcha".to_string(),
                description: "求解一张验证码图片并返回结构化结果。类型由 `kind` 指定（当前仅字符型 \
                    \"text\"，可省略）。**用于验证码**；通用读图请用 builtin_recognize_image。\
                    图片必须是本机已存在的图片文件路径，通常来自上一个工具的输出。"
                    .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "本地图片文件路径（png / jpg / jpeg / webp / gif）"
                        },
                        "kind": {
                            "type": "string",
                            "description": "验证码类型，缺省 \"text\"；当前仅支持 \"text\"（字符型）"
                        }
                    },
                    "required": ["path"]
                }),
            },
            // 与 builtin_recognize_image 一致：Utility 在生产代码里不会被 allowed_tools 剔除。
            vec![ToolCategory::Utility],
        )]
    }

    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(CaptchaToolsExecutor {
            backend: self.backend.clone(),
            service: self.service.clone(),
        })
    }
}

/// 验证码求解执行器。
struct CaptchaToolsExecutor {
    backend: Arc<dyn CaptchaBackend>,
    service: Arc<FilesystemService>,
}

#[async_trait]
impl ToolExecutor for CaptchaToolsExecutor {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        // 与 filesystem / vision 工具族同一约定：可预期失败返回
        // `Ok(ToolResult { is_error: true, .. })`，`Err` 只留给「未知工具名」这类编程错误。
        Ok(match tool_name {
            "builtin_solve_captcha" => {
                solve_captcha(&*self.backend, &self.service, &arguments).await
            }
            _ => return Err(anyhow::anyhow!("Unknown tool: {}", tool_name)),
        })
    }

    fn name(&self) -> &str {
        "builtin_captcha_tools"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec!["builtin_solve_captcha".to_string()]
    }
}

// ── builtin_solve_captcha ───────────────────────────────────────────────────

async fn solve_captcha(
    backend: &dyn CaptchaBackend,
    service: &FilesystemService,
    arguments: &Value,
) -> ToolResult {
    let started = Instant::now();

    // ① 参数校验：**先于一切 IO**（缺参数不必先去读盘 / 嗅探格式）
    let Some(path_str) = arguments
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let kind = arguments
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_KIND);
    let Some(strategy) = strategy(kind) else {
        // **不静默回退到 text**：否则将来「加了 slide 但调用方拼错」会变成「静默按字符型解」，错误难查。
        return failure(
            "invalid_arguments",
            format!(
                "kind「{kind}」不支持（当前支持：{}）",
                SUPPORTED_KINDS.join(" / ")
            ),
        );
    };
    let path = Path::new(path_str);

    // ② 路径校验（越界即拒）+ 存在性 / 类型 / 大小（读盘前判，错误码才可控）
    let resolved = match service.resolve(path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };
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

    // ③ 读字节：走 cap-std 句柄，不按字符串路径重新打开（防 TOCTOU 逃逸）
    let bytes = match resolved.dir.read(&resolved.rel) {
        Ok(bytes) => bytes,
        Err(error) => return failure("read_failed", format!("读取图片失败：{error}")),
    };

    // ④ MIME 按内容嗅探（不看扩展名）
    let Some(mime) = infer::get(&bytes).map(|kind| kind.mime_type()) else {
        return failure(
            "unsupported_image_type",
            format!("无法识别的图片格式（支持：{}）", SUPPORTED_MIME.join(" / ")),
        );
    };
    if !SUPPORTED_MIME.contains(&mime) {
        return failure(
            "unsupported_image_type",
            format!(
                "不支持的图片格式「{mime}」（支持：{}）",
                SUPPORTED_MIME.join(" / ")
            ),
        );
    }

    // ⑤ 按策略组装图集并求解
    // 先把长度存下来：`bytes` 会被 move 进图集，而审计日志要用它。
    let image_len = bytes.len();
    let images = vec![(mime.to_string(), bytes)];
    // 开发期守卫：策略声明要几张图、图集就该有几张。**加类型时最容易漏的一处**。
    // v1 只有单图策略故恒真；真到多类型时这里要升级成运行时校验并返回 `invalid_arguments`。
    debug_assert_eq!(images.len(), strategy.expected_images);
    let request = SolveRequest {
        kind,
        prompt: strategy.prompt,
        images: &images,
    };
    // 外层时限是**兜底**：任何后端（含将来的第三方 API）都必须有时限，否则一次挂起会拖死
    // 整个步骤 —— flexible 的 `llm_timeout` 管不到工具内部这次调用。
    let raw = match tokio::time::timeout(
        Duration::from_secs(SOLVE_TIMEOUT_SECS),
        backend.solve(request),
    )
    .await
    {
        Err(_) | Ok(Err(BackendError::Timeout)) => {
            // 超时是最该被看见的一种失败（步骤会被它拖满整整一分钟），单独记一条
            tracing::warn!(
                target: "tool_audit",
                tool = "builtin_solve_captcha",
                backend = backend.name(),
                path = %path_str,
                duration_ms = started.elapsed().as_millis() as u64,
                "验证码求解超时"
            );
            return failure(
                "timeout",
                format!("求解超时（超过 {SOLVE_TIMEOUT_SECS} 秒）"),
            )
        }
        Ok(Err(BackendError::Failed(error))) => {
            tracing::warn!(
                target: "tool_audit",
                tool = "builtin_solve_captcha",
                backend = backend.name(),
                path = %path_str,
                error = %format!("{error:#}"),
                "验证码求解调用后端失败"
            );
            return failure("backend_failed", format!("{error:#}"));
        }
        Ok(Ok(raw)) => raw,
    };

    // ⑥ 归一化 + tagged union 出参
    //    「认不出来」是 `readable: false` 而**不是** `is_error`（设计稿 §3.2）：
    //    `is_error: true` 会让回灌链路当「工具失败」处理，模型容易就此放弃；
    //    而「这张图看不清」是正常结果，模型该据此决定重新截图还是判该步失败。
    let outcome = (strategy.parse)(&raw);
    let readable = outcome
        .get("readable")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // 审计：只记元信息 + **截断后的**原始输出。
    // 这里与 `vision_tools.rs` 的纪律（不记完整识别文本）刻意不同：验证码文本极短、不含页面
    // 内容，而它是排查「为什么读错」的唯一现场证据。绝不记图片字节 / data URL。
    let model = backend.model();
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_solve_captcha",
        backend = backend.name(),
        model = model.as_deref().unwrap_or("-"),
        path = %path_str,
        kind,
        mime,
        bytes = image_len,
        raw = %truncate_for_log(raw.trim()),
        raw_chars = raw.trim().chars().count(),
        readable,
        duration_ms = started.elapsed().as_millis() as u64,
        "验证码求解完毕"
    );

    tool_result(outcome, false)
}

/// 日志用的截断（按**字符**，UTF-8 安全）。
fn truncate_for_log(text: &str) -> String {
    if text.chars().count() <= MAX_TEXT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_TEXT_CHARS).collect();
    format!("{head}…（已截断）")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// PNG 魔数（`infer` 据此判定 `image/png`）。
    const PNG_HEADER: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    /// 假后端的固定答复。
    #[derive(Clone)]
    enum FakeReply {
        Ok(String),
        Timeout,
        Failed,
    }

    impl FakeReply {
        fn into_result(self) -> std::result::Result<String, BackendError> {
            match self {
                FakeReply::Ok(text) => Ok(text),
                FakeReply::Timeout => Err(BackendError::Timeout),
                FakeReply::Failed => {
                    Err(BackendError::Failed(anyhow::anyhow!("假后端失败")))
                }
            }
        }
    }

    /// 只回固定串 / 固定错误的桩，并记录收到的请求形状 `(kind, prompt, 图片张数)`。
    struct FakeBackend {
        reply: FakeReply,
        seen: Mutex<Vec<(String, String, usize)>>,
    }

    impl FakeBackend {
        fn calls(&self) -> usize {
            self.seen
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .len()
        }

        fn last(&self) -> (String, String, usize) {
            self.seen
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .last()
                .expect("应当收到过一次求解请求")
                .clone()
        }
    }

    #[async_trait]
    impl CaptchaBackend for FakeBackend {
        fn name(&self) -> &str {
            "fake"
        }

        async fn solve<'a>(
            &self,
            request: SolveRequest<'a>,
        ) -> std::result::Result<String, BackendError> {
            self.seen
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push((
                    request.kind.to_string(),
                    request.prompt.to_string(),
                    request.images.len(),
                ));
            self.reply.clone().into_result()
        }
    }

    fn provider_in(dir: &Path, reply: FakeReply) -> (CaptchaToolsProvider, Arc<FakeBackend>) {
        let backend = Arc::new(FakeBackend {
            reply,
            seen: Mutex::new(Vec::new()),
        });
        let as_dyn: Arc<dyn CaptchaBackend> = backend.clone();
        let provider =
            CaptchaToolsProvider::with_backend(as_dyn, dir).expect("provider 构造应当成功");
        (provider, backend)
    }

    fn provider_ok(dir: &Path, reply: &str) -> (CaptchaToolsProvider, Arc<FakeBackend>) {
        provider_in(dir, FakeReply::Ok(reply.to_string()))
    }

    async fn run(provider: &CaptchaToolsProvider, arguments: Value) -> ToolResult {
        provider
            .executor()
            .execute("builtin_solve_captcha", arguments)
            .await
            .expect("工具执行不应返回 Err")
    }

    fn write_png(dir: &Path, name: &str) -> PathBuf {
        let target = dir.join(name);
        std::fs::write(&target, PNG_HEADER).expect("写文件");
        target
    }

    #[test]
    fn exposes_builtin_solve_captcha_in_utility() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, _backend) = provider_ok(dir.path(), "x");
        let tools = provider.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].0.name, "builtin_solve_captcha");
        assert_eq!(tools[0].1, vec![ToolCategory::Utility]);
        assert_eq!(
            provider.executor().supported_tools(),
            vec!["builtin_solve_captcha".to_string()]
        );
        // `kind` 有缺省值，所以只有 `path` 是必填。
        assert_eq!(tools[0].0.input_schema["required"], json!(["path"]));
        // 边界要写进 description（否则模型会在两个读图工具间选错）
        assert!(tools[0].0.description.contains("builtin_recognize_image"));
    }

    // ── 参数校验 ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn missing_path_is_invalid_arguments() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, backend) = provider_ok(dir.path(), "x");

        let result = run(&provider, json!({})).await;

        assert!(result.is_error);
        assert_eq!(result.content["error"], "invalid_arguments");
        // path 与 kind 共用同一个错误码，只断码锁不住「path 优先」——必须看文案。
        assert!(
            result.content["message"]
                .as_str()
                .unwrap_or_default()
                .contains("path"),
            "path 缺失应当先于 kind 被报出来：{result:?}"
        );
        assert_eq!(backend.calls(), 0, "参数不全不应调用后端");
    }

    #[tokio::test]
    async fn unsupported_kind_is_invalid_arguments() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, backend) = provider_ok(dir.path(), "x");
        let target = write_png(dir.path(), "captcha.png");

        let result = run(&provider, json!({ "path": target, "kind": "slide" })).await;

        assert!(result.is_error);
        assert_eq!(result.content["error"], "invalid_arguments");
        assert!(
            result.content["message"]
                .as_str()
                .unwrap_or_default()
                .contains("slide"),
            "错误信息要报出非法值：{result:?}"
        );
        // **不静默回退到 text** —— 校验在读盘之前，故也不该碰后端
        assert_eq!(backend.calls(), 0);
    }

    // ── 图片校验 ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn path_outside_allowed_root_is_rejected() {
        let allowed = tempfile::tempdir().expect("允许目录");
        let outside = tempfile::tempdir().expect("越界目录");
        let (provider, backend) = provider_ok(allowed.path(), "x");
        let target = outside.path().join("captcha.png");
        std::fs::write(&target, PNG_HEADER).expect("写文件");

        let result = run(&provider, json!({ "path": target })).await;

        assert!(result.is_error);
        assert_eq!(result.content["error"], "path_outside_allowed");
        assert_eq!(backend.calls(), 0, "越界路径不应发起后端调用");
    }

    #[tokio::test]
    async fn missing_file_is_file_not_found() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, _backend) = provider_ok(dir.path(), "x");
        let target = dir.path().join("nope.png");

        let result = run(&provider, json!({ "path": target })).await;

        assert!(result.is_error);
        assert_eq!(result.content["error"], "file_not_found");
    }

    #[tokio::test]
    async fn directory_is_rejected_as_invalid_arguments() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, _backend) = provider_ok(dir.path(), "x");
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).expect("建目录");

        let result = run(&provider, json!({ "path": nested })).await;

        assert!(result.is_error);
        assert_eq!(result.content["error"], "invalid_arguments");
    }

    #[tokio::test]
    async fn unsupported_content_is_rejected_without_backend_call() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, backend) = provider_ok(dir.path(), "x");
        let target = dir.path().join("not-an-image.png");
        std::fs::write(&target, b"definitely not an image").expect("写文件");

        let result = run(&provider, json!({ "path": target })).await;

        assert!(result.is_error);
        assert_eq!(result.content["error"], "unsupported_image_type");
        assert_eq!(backend.calls(), 0);
    }

    #[tokio::test]
    async fn oversized_image_is_rejected() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, backend) = provider_ok(dir.path(), "x");
        let target = dir.path().join("huge.png");
        // 用稀疏文件：`set_len` 造出超限的**长度**但不真占磁盘
        let file = std::fs::File::create(&target).expect("建文件");
        file.set_len(MAX_IMAGE_BYTES + 1).expect("扩容");

        let result = run(&provider, json!({ "path": target })).await;

        assert!(result.is_error);
        assert_eq!(result.content["error"], "image_too_large");
        assert_eq!(backend.calls(), 0);
    }

    // ── 出参 ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn success_returns_tagged_union() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, backend) = provider_ok(dir.path(), "  4F7K\n");
        let target = write_png(dir.path(), "captcha.png");

        let result = run(&provider, json!({ "path": target })).await;

        assert!(!result.is_error, "求解成功不应是 is_error：{result:?}");
        assert_eq!(
            result.content,
            json!({ "kind": "text", "text": "4F7K", "readable": true })
        );

        // 桩收到的请求形状：缺省 kind 为 text、提示词是**固化**的那一份、单张图
        let (kind, prompt, images) = backend.last();
        assert_eq!(kind, "text");
        assert_eq!(prompt, TEXT_PROMPT);
        assert_eq!(images, 1);
    }

    #[tokio::test]
    async fn unreadable_sentinel_is_not_an_error() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, _backend) = provider_ok(dir.path(), "UNREADABLE");
        let target = write_png(dir.path(), "blur.png");

        let result = run(&provider, json!({ "path": target })).await;

        // 「看不清」是**正常结果**而不是工具失败 —— 用 readable 表达，别用 is_error
        assert!(!result.is_error, "{result:?}");
        assert_eq!(
            result.content,
            json!({ "kind": "text", "text": Value::Null, "readable": false })
        );
    }

    #[tokio::test]
    async fn empty_reply_is_readable_false() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, _backend) = provider_ok(dir.path(), "   ");
        let target = write_png(dir.path(), "blank.png");

        let result = run(&provider, json!({ "path": target })).await;

        assert!(!result.is_error);
        assert_eq!(result.content["readable"], false);
    }

    #[tokio::test]
    async fn overlong_reply_is_readable_false_not_truncated() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, _backend) = provider_ok(dir.path(), &"字".repeat(MAX_TEXT_CHARS + 1));
        let target = write_png(dir.path(), "long.png");

        let result = run(&provider, json!({ "path": target })).await;

        // 判「没认出来」而不是截断 —— 截断会给出一个**看似合理但错误**的验证码
        assert!(!result.is_error);
        assert_eq!(result.content["readable"], false);
        assert_eq!(result.content["text"], Value::Null);
    }

    // ── 归一化（纯函数）─────────────────────────────────────────────────────

    #[test]
    fn normalization_is_conservative() {
        // 该去的
        assert_eq!(normalize_text("  4F7K \n"), Some("4F7K".to_string()));
        assert_eq!(normalize_text("\"4F7K\""), Some("4F7K".to_string()));
        assert_eq!(normalize_text("'4F7K'"), Some("4F7K".to_string()));
        assert_eq!(normalize_text("`4F7K`"), Some("4F7K".to_string()));
        assert_eq!(normalize_text("4F7K."), Some("4F7K".to_string()));
        assert_eq!(normalize_text("4F7K。"), Some("4F7K".to_string()));
        assert_eq!(normalize_text("```\n4F7K\n```"), Some("4F7K".to_string()));
        assert_eq!(normalize_text("```text\n4F7K\n```"), Some("4F7K".to_string()));
        assert_eq!(normalize_text("~~~\n4F7K\n~~~"), Some("4F7K".to_string()));

        // 该保留的：**不做**大小写转换、**不删**内部空格
        assert_eq!(normalize_text("  ab12CD "), Some("ab12CD".to_string()));
        assert_eq!(normalize_text("4 F 7 K"), Some("4 F 7 K".to_string()));

        // 认不出来
        assert_eq!(normalize_text(""), None);
        assert_eq!(normalize_text("   "), None);
        assert_eq!(normalize_text("UNREADABLE"), None);
        // 大小写不敏感 —— 模型偶尔回 Unreadable / unreadable
        assert_eq!(normalize_text(" unreadable "), None);
        assert_eq!(normalize_text("Unreadable"), None);

        // 只有半个围栏 → 不当围栏处理（无法判断哪部分是内容）
        assert_eq!(
            normalize_text("```\n4F7K"),
            Some("```\n4F7K".to_string())
        );

        // 超长 → 判失败
        assert_eq!(normalize_text(&"字".repeat(MAX_TEXT_CHARS + 1)), None);
        assert_eq!(
            normalize_text(&"字".repeat(MAX_TEXT_CHARS)),
            Some("字".repeat(MAX_TEXT_CHARS))
        );
    }

    // ── 后端 ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn backend_timeout_is_reported_as_timeout() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, _backend) = provider_in(dir.path(), FakeReply::Timeout);
        let target = write_png(dir.path(), "captcha.png");

        let result = run(&provider, json!({ "path": target })).await;

        assert!(result.is_error);
        assert_eq!(result.content["error"], "timeout");
    }

    #[tokio::test]
    async fn backend_failure_is_reported_as_backend_failed() {
        let dir = tempfile::tempdir().expect("临时目录");
        let (provider, _backend) = provider_in(dir.path(), FakeReply::Failed);
        let target = write_png(dir.path(), "captcha.png");

        let result = run(&provider, json!({ "path": target })).await;

        assert!(result.is_error);
        assert_eq!(result.content["error"], "backend_failed");
    }

    #[test]
    fn request_shape_puts_prompt_first_and_image_second() {
        let images = vec![("image/png".to_string(), PNG_HEADER.to_vec())];

        let request = build_request("fake-model", TEXT_PROMPT, &images);

        assert_eq!(request.model, "fake-model");
        assert!(request.tools.is_none(), "内部调用不得带 tools");
        assert_eq!(request.messages.len(), 1);
        let message = &request.messages[0];
        assert!(matches!(message.role, MessageRole::User));
        let MessageContent::Parts { parts } = message.content.as_ref().expect("content") else {
            panic!("提示词与图片都必须走 Parts");
        };
        assert_eq!(parts.len(), 2);
        match &parts[0] {
            ContentPart::Text { text } => assert_eq!(text, TEXT_PROMPT),
            other => panic!("第一段应当是提示词，实际 {other:?}"),
        }
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

    #[test]
    fn log_truncation_is_utf8_safe() {
        let long = "字".repeat(MAX_TEXT_CHARS + 10);
        let logged = truncate_for_log(&long);
        assert!(logged.chars().count() < long.chars().count());
        assert!(logged.ends_with("（已截断）"));
        // 未超长时一字不改
        assert_eq!(truncate_for_log("4F7K"), "4F7K");
    }
}
