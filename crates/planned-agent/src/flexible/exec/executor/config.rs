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
/// 「空回答」的默认**重发**次数（每轮各自计算，见 `ExecutorConfig::llm_empty_retries`）。
pub const DEFAULT_LLM_EMPTY_RETRIES: usize = 1;
/// 「收敛核对」的默认次数上限（见 `ExecutorConfig::verify_attempts`）。
///
/// 给 `2` 而不是 `1`：模型可能在**单独一轮**只回「核对未通过」（不带工具调用），
/// 下一次核对才是它动手修完后的复述。上限为 1 时这种分两步的做法没有修复机会，
/// 「不满足就继续修」的意图会落空。
pub const DEFAULT_VERIFY_ATTEMPTS: usize = 2;

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
    /// 「**空回答**」（既无正文也无思考、且无工具调用）的**重发**次数，**每轮各自计算**。
    ///
    /// 与 `llm_timeout_retries` **正交**：那条管「没拿到响应」，这条管「拿到了但没有内容」。
    /// 空回答是 **HTTP 200 的成功响应**，`ai-openai` 内层重试覆盖不到，只能在这一层兜。
    ///
    /// 重发的是**同一条请求**（messages 一字不动）—— 工具已经执行、结果已回灌，
    /// 所以重发**不会重复执行工具**（对比「整步重跑」会，见 hardening 稿 A6）。
    /// 代价只是一次全量 prompt 请求。
    ///
    /// **不占** `max_rounds_per_step` 的轮数：另记 `StepRunRecord::llm_retries`
    /// （`call_usages.len() == rounds + llm_retries`）。
    pub llm_empty_retries: usize,
    /// 每步「**收敛核对**」的次数上限（`0` = 关闭）。
    ///
    /// 模型自称完成（该轮**无工具调用**）时不再立即收工，而是往**同一条对话**追加一条
    /// user 消息，要求它对照「期望产出」逐条自检：不满足就继续调用工具去修，满足才收工。
    ///
    /// 与 `llm_empty_retries` 的区别：那条是「**重发同一条请求**」（messages 一字不动、
    /// 不占 `rounds`）；这条是「**追加新消息**」—— 模型因此可以接着调工具修，所以它
    /// **占** `rounds`，`call_usages.len() == rounds + llm_retries` 的不变式仍然成立。
    ///
    /// 用尽仍自称不满足 ⇒ 该步 `Failed`（`output` 保持 `None`），后续步骤按现有语义
    /// `Skipped`。**不新增 `StepStatus` 取值**，故对外的 `StepPhase` / 报告 / UI 全不受影响。
    pub verify_attempts: usize,
    /// 只对「**调用过至少一次工具**」的步骤做收敛核对（默认 `true`）。
    ///
    /// 纯文本产出的步骤没有外部副作用、误收敛概率低，而核对固定要多花一次**全上下文**
    /// 请求；默认跳过这一档，把预算留给真正动过文件 / 命令 / 网络的步骤。
    pub verify_only_after_tools: bool,
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
            llm_empty_retries: DEFAULT_LLM_EMPTY_RETRIES,
            verify_attempts: DEFAULT_VERIFY_ATTEMPTS,
            verify_only_after_tools: true,
        }
    }
}
