//! GUI 配置：**每类配置一个文件**。本目录只放「配置数据结构」，不含运行时逻辑。
//!
//! | 文件 | 内容 |
//! |---|---|
//! | [`ai`] | AI 提供商（`[[ai_providers]]`） |
//! | [`logging`] | 日志级别 / 格式 |
//! | [`gui`] | 窗口 / 主题 / 默认视图 |
//! | [`storage`] | 本地持久化（SQLite via SeaORM） |
//! | [`cache`] | 本地 KV 缓存（sled） |
//! | [`rag`] | 向量检索（embedder / store / retrieval） |
//! | [`flexible`] | 灵活计划执行期参数 |
//! | [`layout`] | 全局 `cache_root` 派生出的四个落盘路径（**拼接唯一落点**） |
//! | [`load`] | `config.toml` 的候选路径搜索与反序列化 |
//!
//! 顶层聚合结构是 [`GuiConfig`]；子模块类型一律 `pub use` 到本层，外部仍按
//! `crate::config::GuiStorageConfig` 之类引用。注意本 crate 另有同名但**不同路径**的模块
//! （`crate::cache`、`crate::storage`、`crate::context::rag`、`crate::paths`）——
//! 那些是运行时逻辑，与本目录的配置结构无关。

use planned_agent_prompt_manager::PromptManagerConfig;
use serde::{Deserialize, Serialize};

mod ai;
mod cache;
mod flexible;
mod gui;
mod layout;
mod load;
mod logging;
mod rag;
mod storage;

pub use ai::AiProviderConfig;
pub use cache::GuiCacheConfig;
pub use flexible::FlexibleConfig;
pub use gui::GuiSettings;
pub use logging::LoggingConfig;
pub use rag::{RagConfig, RagRetrievalConfig, RagStoreConfig};
pub use storage::GuiStorageConfig;

/// agent-gui 完整配置
///
/// 注意：MCP 服务器配置已迁移到 `data/mcp-config.json`，由 `McpConfigService` 管理。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiConfig {
    /// AI 提供商列表
    #[serde(default)]
    pub ai_providers: Vec<AiProviderConfig>,

    /// Prompt 管理器配置
    #[serde(default)]
    pub prompt_manager: PromptManagerConfig,

    /// 日志配置
    #[serde(default)]
    pub logging: LoggingConfig,

    /// GUI 专属配置
    #[serde(default)]
    pub gui: GuiSettings,

    /// RAG 向量检索配置
    #[serde(default)]
    pub rag: RagConfig,

    /// 本地持久化配置（SQLite via SeaORM）
    #[serde(default)]
    pub storage: GuiStorageConfig,

    /// 本地 KV 缓存配置（sled 后端）
    #[serde(default)]
    pub cache: GuiCacheConfig,

    /// 灵活计划执行配置
    #[serde(default)]
    pub flexible: FlexibleConfig,

    /// 全局缓存根目录：所有需要落盘的缓存 / 本地数据文件都拼在它下面
    /// （sled KV、SQLite 库、flexible 步骤产出、RAG 向量库）。
    ///
    /// 子项（`cache.path` / `storage.db_path` / `flexible.output_cache_dir` /
    /// `rag.store.path`）的解析规则见 `docs/planned-agent/gui-cache-root.md` §3.3：
    /// **单段名**（如 `kv_store`）→ 拼在本目录下；**绝对路径或含分隔符的相对路径** → 按原语义
    /// （相对 cwd）解释，不再拼根。**不含** GUI 日志（`logs/` 独立，见 `main.rs`）。
    #[serde(default = "default_cache_root")]
    pub cache_root: String,
}

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            ai_providers: Vec::new(),
            prompt_manager: PromptManagerConfig::default(),
            logging: LoggingConfig::default(),
            gui: GuiSettings::default(),
            rag: RagConfig::default(),
            storage: GuiStorageConfig::default(),
            cache: GuiCacheConfig::default(),
            flexible: FlexibleConfig::default(),
            cache_root: default_cache_root(),
        }
    }
}

/// 全局缓存根目录默认值（四项默认子项都在 `./data` 下，故默认落点不变）。
fn default_cache_root() -> String {
    "./data".to_string()
}
