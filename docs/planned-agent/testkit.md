# 测试库（testkit）设计

> **状态**：✅ **已实施**（2026-10-08）。实测见 §7。
> **目标**：提供「**实例化真实 AI + 注册真实工具 + 调一次工具**」的能力，让**单个工具**（第一个用例：`builtin_solve_captcha`）能在没有 GUI、没有手工点界面的情况下被测。

## 1. 结论先说

**新增一个独立 crate `crates/testkit`（`planned-agent-testkit`，`publish = false`），不要合并在 `agent-gui` 下。**（✅ 已落地）

理由（后两条是硬理由）：

| # | 理由 |
|---|---|
| 1 | **`agent-gui` 下的测试库只能被 `agent-gui` 自己用。** `agent-gui` 已经 `depend` `tool-manager`（`crates/agent-gui/Cargo.toml:24-30`）；若测试库放它下面，`tool-manager` 的测试想复用它就成了反向依赖（prod 环）。而「测验证码工具」这件事天然属于工具层。 |
| 2 | **`agent-gui` 会把 `dioxus` 拖进任何使用方的测试构建**，任何 crate 引它当 dev-dependency 都要先编译整个 GUI（分钟级）。 |
| 3 | 语义错位：这是「工具 / AI 的测试脚手架」，与桌面 GUI 无关；GUI 的 `--bins` 测试语义也会被搅浑。 |
| 4 | 全是 `pub` 的低层 API（见 §3）足以让一个最小 crate 独立装配，**不需要碰 GUI 的私有逻辑**。 |

## 2. 现状（已核实，方案的事实基础）

### 2.1 能直接复用的（全部 `pub`）

| 需要的能力 | API | 位置 |
|---|---|---|
| 真实 AI 管理器 | `AiManager::from_config(Vec<AiProviderConfig>)` / `.default()` | `crates/ai-manager/src/lib.rs:16,48` |
| 真实 OpenAI 客户端 | `OpenAiClient::new(OpenAiClientConfig)` | `crates/ai-openai/src/client.rs:342` |
| Provider 配置 | `core::ai::config::AiProviderConfig` | `crates/core/src/ai/config.rs:14` |
| 工具注册表 | `ToolRegistry::{new, register_builtin_provider, set_mcp_manager}` | `crates/tool-manager/src/core/registry.rs:73,258,89` |
| **调一次工具** | `ToolRegistry::call_tool(name, args) -> anyhow::Result<ToolOutcome>` | `.../registry.rs:598` |
| 内置 provider | `builtin::{Filesystem,TextTools,System,Data,Ai,Web,Doc,Vision,Captcha}Provider` | `crates/tool-manager/src/builtin/` |
| 验证码工具 | `CaptchaToolsProvider::{with_backend, with_local_vision}` | `crates/tool-manager/src/builtin/captcha_tools.rs:439,444` |
| 通用读图工具 | `VisionToolsProvider::new(ai, root)` | `crates/tool-manager/src/builtin/vision_tools.rs` |
| 结果类型 | `ToolOutcome` / `ToolResult` / `BuiltinToolProvider` / `ToolExecutor` | `core/src/types.rs:19`、`core/src/mcp/types.rs:14`、`core/src/tool_registry/traits.rs:11,35` |

→ 一个外部 crate 只需依赖 **core + ai-openai + ai-manager + tool-manager** 四个即可构造「真实 AI + 真实工具」，**无需任何 GUI 依赖**。

### 2.2 GUI 私有、**不可复用**（testkit 需自建一份等价装配）

- `ToolsContext::init` 的整套装配顺序与沙箱根策略（`crates/agent-gui/src/context/tools/mod.rs:81-142`）。
- `vision_ai` / `vision_root` 的取值（`crates/agent-gui/src/boot.rs:82-83`，依赖 `GuiConfig::flexible_output_dir()`）。
- `config.toml` 的加载与搜索路径（`crates/agent-gui/src/config/load.rs:40-61`）。
- `request_user_action` + `NoopExecutor`（`context/tools/mod.rs:186-272`）。

### 2.3 两个约束性事实

- **真实 AI 的密钥只来自 `config.toml` 的 `[[ai_providers]].api_key`**（`crates/ai-manager` 按 provider 分派，只认 `"openai"`；`base_url` 可指向 DeepSeek 等）。**没有 API-key 环境变量**；GUI Settings 的 Model 页是「即将上线」占位（`pages/settings/page.rs:106-110`），**不能**在界面配 provider。
  → **已拍板的做法**：testkit **自己复制一份 `config.toml`**（`crates/testkit/config.toml`，由 `.example` 模板派生），与仓库根的那份互不影响。根 `.gitignore` 的 `config.toml`（无前导斜杠）已匹配任意层级，密钥不会进库。
- **全仓没有任何 crate 把同仓 crate 当 `dev-dependencies` 的先例**，也没有 `[features]` 段。根 `Cargo.toml` 是纯 `[workspace]`（无 `[package]`），`examples/` 因此不被构建。

### 2.4 已有的「真实调用」例子（可参考，但不被 cargo 构建）

`examples/stream_chat.rs` 已演示了**从 `OPENAI_API_KEY` 环境变量**拼 `OpenAiClientConfig` 直接构造真实客户端 —— 这正是 testkit 想固化的路径。

## 3. 设计

### 3.1 层级与依赖

```
L0  core / util / rag
L1  ai-openai / tool-manager / mcp-rmcp / prompt-manager / script-lua
L2  ai-manager
L3  planned-agent
L4  agent-gui        ← 新增 testkit 放这一层附近
    testkit          ← 依赖 core + ai-openai + ai-manager + tool-manager
```

- **依赖**：`core`、`ai-manager`、`tool-manager`（`[dependencies]`，path 形式）+ `config` / `tempfile` / `serde` / `serde_json` / `anyhow` / `tokio`。
  （**不**直接依赖 `ai-openai` —— `AiManager::from_config` 内部已经用它。）
- **不依赖** `planned-agent` / `agent-gui` —— 保持最小；将来要测 flexible 流水线时再加 `planned-agent`。
- `publish = false`。
- **不反向被依赖**：`tool-manager` 的 `tests/` 保持现状（纯 mock）。真实集成测试统一写在 **testkit 自己的 `tests/`** 里 —— 这样**不需要 dev-dependency 环**（Cargo 对 dev 环的处理是灰色地带，直接绕开）。

### 3.2 对外 API（`src/lib.rs`，已落地）

```rust
// 配置：testkit 自己的一份 config.toml
pub struct TestConfig { pub ai_providers: Vec<AiProviderConfig> }
impl TestConfig {
    pub fn default_path() -> Option<PathBuf>;            // PLANNED_AGENT_TESTKIT_CONFIG → crates/testkit/config.toml
    pub fn load(path: &Path) -> anyhow::Result<Self>;    // 显式按 TOML 解析（模板是 .example）
}
pub fn real_ai_from_config(path: &Path) -> anyhow::Result<Arc<dyn AiClient>>;

// 测试台
pub struct TestHarness { /* registry, ai, sandbox_root, _sandbox */ }
impl TestHarness {
    pub fn from_env() -> anyhow::Result<Self>;              // 走 TestConfig::default_path()
    pub fn from_config_path(path: &Path) -> anyhow::Result<Self>;
    pub fn builder() -> TestHarnessBuilder;
    pub fn registry(&self) -> &ToolRegistry;
    pub fn ai(&self) -> Option<Arc<dyn AiClient>>;
    pub fn sandbox_root(&self) -> &Path;
    /// 把**外部文件**复制进沙箱，返回沙箱内路径（fixture 素材必须先「搬进沙箱」）。
    pub fn stage_file(&self, source: &Path) -> anyhow::Result<PathBuf>;
    pub async fn call(&self, tool: &str, arguments: Value) -> anyhow::Result<ToolOutcome>;
}

pub struct TestHarnessBuilder { /* ai, sandbox_root */ }
impl TestHarnessBuilder {
    pub fn with_ai(self, ai: Arc<dyn AiClient>) -> Self;                    // 不注入 → 视觉类工具不注册
    pub fn with_sandbox_root(self, root: impl Into<PathBuf>) -> Self;       // 默认 TempDir
    pub fn build(self) -> anyhow::Result<TestHarness>;
}
```

**内置工具清单**（`register_default_tools`，复刻 `context/tools/mod.rs:92-124`）：`Filesystem`、`TextTools`、`System`、`Data`、`Ai`、`Web`、`Doc`，以及有 `ai` 时的 `Vision` + `Captcha`（共用一份 `ai` / `root`）。

与 GUI 的**一处有意差异**：文件系统沙箱根**只有** harness 自己的临时目录，不含 cwd 与用户主目录（GUI 会带上）—— 避免测试误伤真实文件。

### 3.3 测试组织（已落地）

```
crates/testkit/
├── Cargo.toml              # publish = false
├── config.toml.example     # 进库的模板（密钥占位）
├── config.toml             # ← 你自己创建（.gitignore 已忽略）
├── src/lib.rs              # TestConfig / TestHarness / builder / register_default_tools
└── tests/
    ├── fixtures/README.md  # 素材说明（`captcha.png`，**一张即可**）
    ├── harness_smoke.rs    # 3 例：不触网，默认测试路径全绿
    ├── path_prefix_real_ai.rs  # content@/file@ 前缀：指纹 + 工具侧拒读（不触网）+ 2 个真实 AI 观察探针
    └── captcha_real_ai.rs  # 1 例：真实 AI，`#[ignore]`
```

**一张图就够**：工具内部先判题型再求解，所以不需要按「字符型 / 算式型」分文件。

**素材由使用者提供**：验证码图因人而异，testkit 不内置真图；缺图时对应 `--ignored` 测试打印提示后跳过。

**两条纪律**（已用 `#[ignore]` 与「只断不变量」落实）：

1. **真实 AI 的测试一律 `#[ignore]`** —— 绝不让 `cargo test` 默认依赖网络与密钥（`cargo test --workspace` 已有既存挂起问题，不能再加网络依赖）。
2. **只断言不变量**（形状 / `kind` / `is_error`），**不断言模型的具体答案** —— 真实模型给不出可稳定断言的字面结果。要断具体答案请用桩（`captcha_tools.rs` 里那套 `FakeBackend`，不动）。

### 3.4 需要改动的文件

| # | 文件 | 动作 |
|---|---|---|
| 1 | `Cargo.toml`（根） | `[workspace] members` 追加 `"crates/testkit"` ✅ |
| 2 | `crates/testkit/Cargo.toml` | 新建（`publish = false`）✅ |
| 3 | `crates/testkit/src/lib.rs` | 新建 ✅ |
| 4 | `crates/testkit/config.toml.example` | 新建（进库的模板）✅ |
| 5 | `crates/testkit/tests/{harness_smoke.rs, captcha_real_ai.rs, fixtures/README.md}` | 新建 ✅ |
| 6 | `workspace/AGENTS.md` §1 表格 + §4/§5 | 补一行 crate 与测试命令 ✅ |

**不动**：`agent-gui`、`tool-manager`、`planned-agent` 的任何 `src/`；根 `config.toml` 及其格式；`examples/`；`.gitignore`（`config.toml` 那条已覆盖新路径，无需改）。

## 4. 与 GUI 装配的漂移风险

testkit 复刻了 GUI 的「内置 provider 清单」（`context/tools/mod.rs:92-124`），两份清单会各自演进。三个可选处理，按代价排序：

- **(a) 不管，靠文档** ✅ **本次采用**：`register_default_tools` 的文档注释明确了清单来源（`context/tools/mod.rs:92-124`）与「GUI 新增时这里要同步」。
- **(b) testkit 只保「注册了哪些 provider」一份清单常量**。
- **(c) 治本**：把清单提到 `tool-manager`（如 `builtin::register_all(&registry, &BuiltinOptions)`），GUI 与 testkit 共用。**代价是动 `agent-gui`**，超出本次范围 —— **记为后续项**。

**采用 (a)**，把 (c) 记为后续项。

## 5. 风险与不变量

1. **不加网络依赖到默认测试路径** —— 真实测试全 `#[ignore]`。
2. **不改产品代码** —— testkit 只消费既有 `pub` API；密钥走**环境变量**（新路径），不碰 `config.toml` 与 GUI 设置。
3. **不反向依赖** —— 没有任何产品 crate 依赖 testkit，故 `cargo build` 的行为不变（只多编一个 `publish = false` 的 crate）。
4. **沙箱不外泄** —— harness 默认用 `tempfile::TempDir` 作文件系统沙箱根与视觉产出目录，测试结束即删。

## 6. 待确认点

| # | 问题 | 拍板 |
|---|---|---|
| P1 | 新 crate vs 合并在 agent-gui | **新 crate** |
| P2 | testkit 的定位范围 | **先只测工具**（core + ai-manager + tool-manager）；将来要测流水线再加 `planned-agent` |
| P3 | 真实 AI 密钥来源 | **testkit 单独复制一份 `config.toml`**（§2.3）—— 不用环境变量、不动根 `config.toml` / GUI |
| P4 | 真实测试的执行开关 | **`#[ignore]`** + `cargo test -p planned-agent-testkit -- --ignored` |
| P5 | provider 清单漂移 | **(a) 靠注释互指**；(c) 抽到 tool-manager 记为后续项 |

## 7. 验收

| # | 验收项 | 实测（2026-10-08） |
|---|---|---|
| 1 | 编译通过 | ✅ `cargo test -p planned-agent-testkit` 编译成功 |
| 2 | 默认（无密钥）全绿且不触网 | ✅ `harness_smoke` **3 passed**；`path_prefix_real_ai` **2 passed**（指纹 + `text_readers_reject_binary_file`）；真实探针 `captcha_real_ai` **1** + `path_prefix_real_ai` **2** = **3 ignored** |
| 3 | 真实 AI 能跑通 `builtin_solve_captcha` | ✅ **已实测**（MiniMax-M3 + `tests/fixtures/captcha.png`）：`{"kind":"text","readable":true,"text":"59"}`；`tool_audit` 记 `variant="calc"`、`duration_ms=3512` |
| 4 | 产品 crate 不反向依赖 | ✅ `crates/testkit` 不出现在任何产品 crate 的 `[dependencies]` |

第 3 项的命令：`cargo test -p planned-agent-testkit -- --ignored --nocapture`（要求 `crates/testkit/config.toml` 与 `tests/fixtures/captcha.png` 就位）。

**实施中发现的三个坑**：

1. `config` crate 默认按**扩展名**判格式，模板文件叫 `config.toml.example` 会报 `not of a registered file format` —— `TestConfig::load` 已显式指定 `FileFormat::Toml`。
2. **工具的文件系统沙箱挡住了 fixture**：`tests/fixtures/captcha.png` 在源码目录里，不在 harness 的临时沙箱内 → `builtin_solve_captcha` 报 `path_outside_allowed`。故加了 `TestHarness::stage_file`（先把素材复制进沙箱再调工具）。
3. **审计日志的 `target` 是 `tool_audit`**，不是 crate 名前缀 —— `tracing_subscriber` 的 `EnvFilter` 用 crate 名过滤会**看不到**工具审计（包括判定的题型 `variant`）。
