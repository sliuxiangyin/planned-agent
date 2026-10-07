//! `config.toml` 的候选路径搜索与反序列化。
//!
//! 注意 `use ::config::Config;` 里的 `::`：外部 crate 名与本 crate 的 `crate::config`
//! 模块同名，显式写 `::` 才不会歧义。

use anyhow::Result;
use ::config::Config;

use super::ai::AiProviderConfig;
use super::GuiConfig;

impl GuiConfig {
    /// 从 `config.toml` 加载配置，失败时返回默认配置
    ///
    /// 加载失败时同时通过 `eprintln!` 输出，因为这是致命错误——日志只写文件，
    /// 用户在 CLI 上看不到 warn，所以必须 stderr 提示一次。
    pub fn load() -> Self {
        match Self::try_load() {
            Ok(cfg) => {
                tracing::info!(
                    "配置加载成功: {} 个 AI provider, RAG api_key={}, cache_root={}, KV cache path={}",
                    cfg.ai_providers.len(),
                    if cfg.rag.embedding_api_key.is_empty() { "未配置" } else { "已配置" },
                    cfg.cache_root,
                    cfg.cache.path
                );
                cfg
            }
            Err(e) => {
                let msg = format!("配置加载失败，使用默认配置: {}", e);
                eprintln!("[planned-agent-gui] {}", msg);
                tracing::warn!("{}", msg);
                Self::default()
            }
        }
    }

    fn try_load() -> Result<Self> {
        // 尝试多个候选路径（Dioxus desktop 运行时 CWD 可能不是 workspace 根）
        let candidates: Vec<std::path::PathBuf> = {
            let mut v = Vec::new();

            // 1. 环境变量覆盖（最高优先级）
            if let Ok(p) = std::env::var("PLANNED_AGENT_CONFIG") {
                v.push(std::path::PathBuf::from(p));
            }

            // 2. 当前工作目录
            if let Ok(cwd) = std::env::current_dir() {
                v.push(cwd.join("config.toml"));
            }

            // 3. 可执行文件所在目录的祖父（target/debug/planned-agent-gui → ../../）
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
                    v.push(dir.join("config.toml"));
                }
            }

            v
        };

        let mut last_err: Option<String> = None;
        for path in &candidates {
            if !path.exists() {
                continue;
            }
            match Self::try_load_from(path) {
                Ok(cfg) => {
                    eprintln!("[planned-agent-gui] 配置加载自: {}", path.display());
                    return Ok(cfg);
                }
                Err(e) => {
                    last_err = Some(format!("{}: {}", path.display(), e));
                }
            }
        }

        // 所有候选路径都失败
        let searched: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
        anyhow::bail!(
            "未找到 config.toml（已尝试 {} 个候选路径: {}），最后一次错误: {}",
            searched.len(),
            searched.join(", "),
            last_err.as_deref().unwrap_or("无")
        )
    }

    fn try_load_from(path: &std::path::Path) -> Result<Self> {
        let cfg = Config::builder()
            .add_source(::config::File::from(path))
            .build()?;
        let gui_config: GuiConfig = cfg.try_deserialize()?;
        Ok(gui_config)
    }

    /// 获取默认 AI 提供商
    pub fn default_ai_provider(&self) -> Option<&AiProviderConfig> {
        self.ai_providers
            .iter()
            .find(|p| p.is_default)
            .or_else(|| self.ai_providers.first())
    }
}
