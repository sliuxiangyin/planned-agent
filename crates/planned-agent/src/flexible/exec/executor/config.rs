//! 执行器配置与默认值常量。

use std::path::PathBuf;
use std::time::Duration; 

/// 产出落盘根目录的默认值（相对**进程 cwd**；宿主可覆盖为含会话段的路径）。
pub const DEFAULT_CACHE_DIR: &str = "./data/cache";
/// 产出超过该字符数就落盘 —— 与记录侧 `OUTPUT_MAX_CHARS` 对齐。
pub const DEFAULT_SPILL_THRESHOLD_CHARS: usize = 8_000;
/// 落盘后写进 `prior` 的预览长度（字符）。
pub const DEFAULT_SPILL_PREVIEW_CHARS: usize = 800;
/// 日志里单条产出的上限：产出可能上万字符，不截断会把日志淹掉。
///
/// 完整内容总能从产出文件拿到（`StepRunRecord::output_file`）。
pub const LOG_OUTPUT_MAX_CHARS: usize = 2_000;

// ──────── LLM 请求的超时与重试 ────────

/// 单次 LLM 请求的默认超时（秒）。
pub const DEFAULT_LLM_TIMEOUT_SECS: u64 = 180;
/// 单次请求超时后的默认重试次数。
pub const DEFAULT_LLM_TIMEOUT_RETRIES: usize = 1;

/// 执行器配置。
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// 每步工具循环的轮数上限。
    pub max_rounds_per_step: usize,
    /// 采样温度（`None` 用 provider 默认）。
    pub temperature: Option<f32>,
    /// 最大生成 token（`None` 用 provider 默认）。
    pub max_tokens: Option<u32>,
    /// 工具白名单，语义与 `ChatConfig::allowed_tools` **完全一致**：
    /// `None` = 全部启用工具（含 Utility / SubAgent，不过滤）；
    /// `Some(tokens)` = 各 token 取并集（`"all"` / 分类名 / 精确工具名）。
    pub allowed_tools: Option<Vec<String>>,
    /// 步骤产出落盘的根目录。
    ///
    /// **必有值** —— 宿主不配置时用 `./data/cache`（相对**进程 cwd**，见设计稿 §13-1）。
    /// 执行器在其下自建 `run-<毫秒>-<序号>` 子目录隔离每次执行：它不认识「会话」概念，
    /// 会话段由宿主拼进这里（`run_service` 传 `./data/cache/<session_id>`）。
    pub cache_dir: PathBuf,
    /// 产出超过该字符数就落盘（下游 `prior` 只拿到「文件说明 + 预览」）。
    pub spill_threshold_chars: usize,
    /// 落盘后 `prior` 里保留的预览长度（字符）—— 给模型判断相关性的线索，不是截断兜底。
    pub spill_preview_chars: usize,
    /// 单次 LLM 请求的超时（`None` = 不限制）。
    ///
    /// ⚠️ 语义是「**一次 `AiClient::chat_completion` 调用**的墙钟上限」：该调用在
    /// `ai-openai` 内部本身有 3 次重试，所以超时**包住的是整次调用**（含内层重试）。
    /// 刻意**不是**「整步 / 整次执行」的超时 —— 工作流可能天然很长，固定总时长会误杀。
    pub llm_timeout: Option<Duration>,
    /// 单次请求**超时**后的重试次数（总尝试次数 = 1 + 这个值）。
    ///
    /// 只重试超时：其它失败（4xx / 5xx / 网络）在 `ai-openai` 内部已按自己的策略重试过，
    /// 这一层再叠加会变成**倍数放大**。
    pub llm_timeout_retries: usize,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            max_rounds_per_step: 50,
            temperature: None,
            max_tokens: None,
            allowed_tools: None,
            cache_dir: PathBuf::from(DEFAULT_CACHE_DIR),
            spill_threshold_chars: DEFAULT_SPILL_THRESHOLD_CHARS,
            spill_preview_chars: DEFAULT_SPILL_PREVIEW_CHARS,
            llm_timeout: Some(Duration::from_secs(DEFAULT_LLM_TIMEOUT_SECS)),
            llm_timeout_retries: DEFAULT_LLM_TIMEOUT_RETRIES,
        }
    }
}
