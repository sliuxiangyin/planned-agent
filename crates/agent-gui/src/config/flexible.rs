//! 灵活计划（`flexible`）执行期配置（`[flexible]`）。

use serde::{Deserialize, Serialize};

/// 灵活计划（`flexible`）执行期的配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlexibleConfig {
    /// 步骤产出落盘的根目录；**执行时会在其下再拼 `/<session_id>`**
    /// （执行器不认识「会话」概念，会话段由宿主拼 —— 见 `flexible-step-output-spill.md` §5.2）。
    ///
    /// 默认是 `cache_root` 下的**单段名** `cache`（解析见 `gui-cache-root.md` §3.3）。
    #[serde(default = "default_flexible_output_cache_dir")]
    pub output_cache_dir: String,

    /// 步骤产出超过该字符数就落盘，下游只拿到「文件说明 + 预览」。
    #[serde(default = "default_flexible_spill_threshold_chars")]
    pub spill_threshold_chars: usize,

    /// 落盘后写进下游 prompt 的预览长度（字符）。
    #[serde(default = "default_flexible_spill_preview_chars")]
    pub spill_preview_chars: usize,

    /// 单次 LLM 请求超时（秒）；**0 = 不限制**。
    ///
    /// 注意是「单次 `chat_completion` 调用」而非「整步 / 整次执行」—— 工作流可能天然很长。
    #[serde(default = "default_flexible_llm_timeout_secs")]
    pub llm_timeout_secs: u64,

    /// 单次请求**超时**后的重试次数（只重试超时，不叠加 `ai-openai` 内部已有的重试）。
    #[serde(default = "default_flexible_llm_timeout_retries")]
    pub llm_timeout_retries: usize,
}

fn default_flexible_output_cache_dir() -> String {
    // 故意不引用内核的 `flexible::DEFAULT_CACHE_DIR`：**根目录是宿主概念**，
    // 内核不知道它；这里只给一个「根下的子目录名」，由 `paths::resolve_under` 拼根。
    "cache".to_string()
}

fn default_flexible_spill_threshold_chars() -> usize {
    planned_agent::flexible::DEFAULT_SPILL_THRESHOLD_CHARS
}

fn default_flexible_spill_preview_chars() -> usize {
    planned_agent::flexible::DEFAULT_SPILL_PREVIEW_CHARS
}

fn default_flexible_llm_timeout_secs() -> u64 {
    planned_agent::flexible::DEFAULT_LLM_TIMEOUT_SECS
}

fn default_flexible_llm_timeout_retries() -> usize {
    planned_agent::flexible::DEFAULT_LLM_TIMEOUT_RETRIES
}

impl Default for FlexibleConfig {
    fn default() -> Self {
        Self {
            output_cache_dir: default_flexible_output_cache_dir(),
            spill_threshold_chars: default_flexible_spill_threshold_chars(),
            spill_preview_chars: default_flexible_spill_preview_chars(),
            llm_timeout_secs: default_flexible_llm_timeout_secs(),
            llm_timeout_retries: default_flexible_llm_timeout_retries(),
        }
    }
}
