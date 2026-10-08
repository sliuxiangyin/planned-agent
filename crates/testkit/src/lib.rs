//! 真实集成测试脚手架：**真实 AI + 真实工具**，用于单独测一个工具。
//!
//! 首个用例是 `builtin_solve_captcha`（见 `tests/captcha_real_ai.rs`）。
//! 设计稿：`docs/planned-agent/testkit.md`。
//!
//! 三条纪律：
//! 1. **真实 AI 的测试一律 `#[ignore]`** —— 默认测试路径绝不触网、不要密钥。
//! 2. 只断言**不变量**（形状 / `kind` / `is_error`），**不断言模型的具体答案** ——
//!    真实模型给不出可稳定断言的字面结果。要断具体答案请用桩（如
//!    `captcha_tools.rs` 里那套 `FakeBackend`）。
//! 3. 依赖只到 L1/L2（core + ai-manager + tool-manager），**不碰** `planned-agent`
//!    / `agent-gui` —— 这样产品 crate 永远不必反向依赖本 crate。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use planned_agent_ai_manager::AiManager;
use planned_agent_core::ai::config::AiProviderConfig;
use planned_agent_core::ai::traits::AiClient;
use planned_agent_tool_manager::builtin::ai_tools::AiToolsProvider;
use planned_agent_tool_manager::builtin::captcha_tools::CaptchaToolsProvider;
use planned_agent_tool_manager::builtin::data_tools::DataToolsProvider;
use planned_agent_tool_manager::builtin::doc_tools::DocToolsProvider;
use planned_agent_tool_manager::builtin::filesystem::FilesystemProvider;
use planned_agent_tool_manager::builtin::system_tools::SystemToolsProvider;
use planned_agent_tool_manager::builtin::text_tools::TextToolsProvider;
use planned_agent_tool_manager::builtin::vision_tools::VisionToolsProvider;
use planned_agent_tool_manager::builtin::web_tools::WebToolsProvider;
use planned_agent_tool_manager::{ToolOutcome, ToolRegistry};
use serde_json::Value;

/// testkit **自己**的一份 `config.toml`（与仓库根 / GUI 的那份互不影响）。
///
/// 用法：把 `config.toml.example` 复制为同目录的 `config.toml` 并填入真实密钥。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct TestConfig {
    /// 与 GUI 的 `config.toml` 同格式（`[[ai_providers]]`），字段同 [`AiProviderConfig`]。
    ///
    /// 其余段落（cache / rag …）**不需要写** —— testkit 只读这一项。
    #[serde(default)]
    pub ai_providers: Vec<AiProviderConfig>,
}

impl TestConfig {
    /// 配置文件路径。
    ///
    /// 优先 `PLANNED_AGENT_TESTKIT_CONFIG`；否则用 `crates/testkit/config.toml`（存在才算）。
    /// 用 [`env!("CARGO_MANIFEST_DIR")`] 锚定，**与 cwd 无关** —— 测试的 cwd 不稳定。
    pub fn default_path() -> Option<PathBuf> {
        if let Ok(path) = std::env::var("PLANNED_AGENT_TESTKIT_CONFIG") {
            return Some(PathBuf::from(path));
        }
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.toml");
        path.exists().then_some(path)
    }

    /// 从 TOML 文件加载（解析路径与 GUI 的 `GuiConfig::try_load_from` 同源）。
    pub fn load(path: &Path) -> Result<Self> {
        let raw = ::config::Config::builder()
            // 显式指定格式：默认按扩展名判断，而模板是 `config.toml.example`，
            // 且 `PLANNED_AGENT_TESTKIT_CONFIG` 可能指向无扩展名的文件。
            .add_source(::config::File::from(path).format(::config::FileFormat::Toml))
            .build()
            .with_context(|| format!("读取 testkit 配置失败：{}", path.display()))?;
        raw.try_deserialize()
            .with_context(|| format!("解析 testkit 配置失败：{}", path.display()))
    }
}

/// 用 testkit 的配置构造**真实** AI 客户端（取默认 provider）。
pub fn real_ai_from_config(path: &Path) -> Result<Arc<dyn AiClient>> {
    let config = TestConfig::load(path)?;
    anyhow::ensure!(
        !config.ai_providers.is_empty(),
        "{} 里没有配置任何 [[ai_providers]]",
        path.display()
    );
    let manager = AiManager::from_config(config.ai_providers)?;
    manager.default()
}

/// 测试台：一个工具注册表 + 一份（可选的）真实 AI + 一个临时沙箱根。
pub struct TestHarness {
    registry: ToolRegistry,
    ai: Option<Arc<dyn AiClient>>,
    sandbox_root: PathBuf,
    /// 保活所有权：harness drop 时临时目录才被删除（`with_sandbox_root` 时不建）。
    _sandbox: Option<tempfile::TempDir>,
}

impl TestHarness {
    /// 用 testkit 的 `config.toml`（见 [`TestConfig::default_path`]）构造**真实 AI** 测试台。
    pub fn from_env() -> Result<Self> {
        let path = TestConfig::default_path().ok_or_else(|| {
            anyhow::anyhow!(
                "找不到 testkit 的 config.toml —— 把 crates/testkit/config.toml.example 复制为 \
                 crates/testkit/config.toml 并填入密钥（或用 PLANNED_AGENT_TESTKIT_CONFIG 指定）"
            )
        })?;
        Self::from_config_path(&path)
    }

    /// 从指定配置文件构造（真实 AI）。
    pub fn from_config_path(path: &Path) -> Result<Self> {
        let ai = real_ai_from_config(path)?;
        Self::builder().with_ai(ai).build()
    }

    pub fn builder() -> TestHarnessBuilder {
        TestHarnessBuilder::default()
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    #[must_use]
    pub fn ai(&self) -> Option<Arc<dyn AiClient>> {
        self.ai.clone()
    }

    /// 文件系统沙箱根（同时也是视觉类工具的图片根目录）。
    pub fn sandbox_root(&self) -> &Path {
        &self.sandbox_root
    }

    /// 把一个**外部文件**复制进沙箱，返回沙箱内的路径。
    ///
    /// 工具的文件系统沙箱只放行 harness 自己的临时目录，所以 `tests/fixtures/` 之类的
    /// 素材必须先「搬进沙箱」才能被工具读到（否则报 `path_outside_allowed`）。
    pub fn stage_file(&self, source: &Path) -> Result<PathBuf> {
        let name = source
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("源路径没有文件名：{}", source.display()))?;
        let target = self.sandbox_root.join(name);
        std::fs::copy(source, &target).with_context(|| {
            format!("复制素材失败：{} → {}", source.display(), target.display())
        })?;
        Ok(target)
    }

    /// 调一次工具，并附上工具名做错误上下文。
    pub async fn call(&self, tool: &str, arguments: Value) -> Result<ToolOutcome> {
        self.registry
            .call_tool(tool, arguments)
            .await
            .with_context(|| format!("调用工具 {tool} 失败"))
    }
}

/// [`TestHarness`] 的构造器。
#[derive(Default)]
pub struct TestHarnessBuilder {
    ai: Option<Arc<dyn AiClient>>,
    sandbox_root: Option<PathBuf>,
}

impl TestHarnessBuilder {
    /// 注入 AI 客户端（真实或桩）。不注入 → 视觉类工具**不注册**（与 GUI 的降级路径同构）。
    #[must_use]
    pub fn with_ai(mut self, ai: Arc<dyn AiClient>) -> Self {
        self.ai = Some(ai);
        self
    }

    /// 指定沙箱根。默认新建一个临时目录，生命周期跟随 harness。
    #[must_use]
    pub fn with_sandbox_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.sandbox_root = Some(root.into());
        self
    }

    pub fn build(self) -> Result<TestHarness> {
        let registry = ToolRegistry::new();
        let (sandbox_root, sandbox) = match self.sandbox_root {
            Some(root) => (root, None),
            None => {
                let dir = tempfile::tempdir().context("创建临时沙箱目录失败")?;
                let root = dir.path().to_path_buf();
                (root, Some(dir))
            }
        };
        register_default_tools(&registry, self.ai.clone(), &sandbox_root)?;
        Ok(TestHarness {
            registry,
            ai: self.ai,
            sandbox_root,
            _sandbox: sandbox,
        })
    }
}

/// 注册与 GUI 同款的**内置 provider 清单**。
///
/// ⚠️ **清单来源**：`crates/agent-gui/src/context/tools/mod.rs:92-124`。GUI 那边新增内置
/// provider 时这里要同步 —— 这是设计稿 §4 记录的已知漂移风险（治本方案是把清单提到
/// `tool-manager`，属后续项）。
///
/// 与 GUI 的一处**有意差异**：文件系统沙箱根只有测试自己的临时目录，**不含** cwd 与用户
/// 主目录（GUI 会带上），避免测试误伤真实文件。
fn register_default_tools(
    registry: &ToolRegistry,
    ai: Option<Arc<dyn AiClient>>,
    root: &Path,
) -> Result<()> {
    let sandbox_roots = vec![root.to_path_buf()];
    registry.register_builtin_provider(&FilesystemProvider::new(&sandbox_roots)?);

    registry.register_builtin_provider(&TextToolsProvider);
    registry.register_builtin_provider(&SystemToolsProvider);
    registry.register_builtin_provider(&DataToolsProvider);
    registry.register_builtin_provider(&AiToolsProvider);
    registry.register_builtin_provider(&WebToolsProvider);

    let docs_dir = root.join("docs");
    std::fs::create_dir_all(&docs_dir)
        .with_context(|| format!("创建 docs 目录失败：{}", docs_dir.display()))?;
    registry.register_builtin_provider(&DocToolsProvider::new(docs_dir));

    // 视觉类工具（都要 AI 客户端 + 同一个图片根目录）。没有 AI 就**都不注册** ——
    // 与 GUI 一致：留一个必然报错的工具不如不注册。
    if let Some(ai) = ai {
        registry.register_builtin_provider(&VisionToolsProvider::new(ai.clone(), root)?);
        registry.register_builtin_provider(&CaptchaToolsProvider::with_local_vision(ai, root)?);
    }
    Ok(())
}
