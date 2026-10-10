//! **真实 AI** 的验证码工具测试。
//!
//! 必须显式开（默认不跑，绝不触网）：
//!
//! ```text
//! cargo test -p planned-agent-testkit -- --ignored
//! ```
//!
//! 前置条件：
//! 1. `crates/testkit/config.toml`（见 `config.toml.example`）；
//! 2. `tests/fixtures/captcha.png`（见该目录 README）。
//!
//! 断言纪律：只断**不变量**（`kind` / `is_error` / 字段类型），**不断言模型的具体答案**
//! —— 真实模型给不出可稳定断言的字面结果。要断具体答案请用桩（`captcha_tools.rs` 里的
//! `FakeBackend`）。
//!
//! 它同时是**耗时基准**：默认跑 3 轮并逐轮打印，因为真实调用的耗时波动很大（实测同一份
//! 配置出现过 3.5s / 4.4s / 8.9s）。用 `CAPTCHA_RUNS=5` 改轮数。
//!
//! 每轮的阶段耗时（`classify_ms` / `solve_ms`）打在 `tool_audit` 日志里 —— 测试会打开它。

use std::path::{Path, PathBuf};

use planned_agent_testkit::TestHarness;
use serde_json::json;

/// 默认重复轮数（真实调用耗时波动大，单点看不准）。
const DEFAULT_RUNS: usize = 3;

/// 取一张 fixture 图；不存在则返回 `None`（测试里打印提示后跳过）。
fn fixture(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    path.exists().then_some(path)
}

/// 打开日志（默认只看**工具审计**那一路；`RUST_LOG` 可覆盖）。
///
/// 审计日志的 `target` 固定是 `tool_audit`（**不是** crate 名前缀），所以必须单独放行 ——
/// 验证码题型与阶段耗时都记在那行。
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("tool_audit=info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

/// 环境变量 `CAPTCHA_RUNS` 指定的轮数（默认 [`DEFAULT_RUNS`]）。
fn runs() -> usize {
    std::env::var("CAPTCHA_RUNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(DEFAULT_RUNS)
}

/// 端到端跑若干轮 `builtin_solve_captcha`。
///
/// 工具内部是**两阶段**：先判题型（字符型 / 算式型）、再按题型求解；出参恒为
/// `{kind:"text", text, readable, variant}` —— v1.2 起 `variant` 把题型带给调用方（§14）。
#[tokio::test]
#[ignore = "需要真实 AI（crates/testkit/config.toml）与真图；用 `-- --ignored` 显式跑"]
async fn solve_captcha_with_real_ai() {
    init_tracing();

    let Some(image) = fixture("captcha.png") else {
        eprintln!("跳过：请把验证码图放到 tests/fixtures/captcha.png");
        return;
    };
    let harness = TestHarness::from_env().expect("构造真实 AI 测试台（见 config.toml.example）");
    if let Some(ai) = harness.ai() {
        println!("使用模型：{} / {}", ai.provider_name(), ai.model_name());
    }

    // 工具的**文件系统沙箱**只放行 harness 自己的临时目录，所以先把 fixture 搬进去
    // （否则报 `path_outside_allowed`）。
    let staged = harness.stage_file(&image).expect("把 fixture 图搬进沙箱");

    let runs = runs();
    let mut failures = 0usize;
    for round in 1..=runs {
        let started = std::time::Instant::now();
        let outcome = harness
            .call(
                "builtin_solve_captcha",
                json!({ "path": staged.to_string_lossy() }),
            )
            .await
            .expect("工具调用本身不应报错（读图 / 网络失败会是 Err）");
        let elapsed = started.elapsed();

        // 服务端**偶发**超时 / 失败是已知现象（实测同一份配置、同一张图，出现过一次
        // `classify` 卡满 60s 超时）：工具已正确把它降级成 `is_error`，所以这里只记录、
        // 不判失败 —— 但**全轮皆失败**时说明是配置或服务端的问题，那时才 FAIL。
        if outcome.result.is_error {
            eprintln!(
                "第 {round}/{runs} 轮：{elapsed:?} → 服务端失败：{}",
                outcome.result.content
            );
            failures += 1;
            continue;
        }

        // 只断不变量：形状恒定；`variant` 只在题型判出时有值（char / calc），判不出为 null
        assert_eq!(outcome.result.content["kind"], "text");
        assert!(outcome.result.content["readable"].is_boolean());
        let variant = &outcome.result.content["variant"];
        assert!(
            variant.is_null() || variant == "char" || variant == "calc",
            "variant 只允许 char / calc / null：{variant}"
        );
        println!(
            "第 {round}/{runs} 轮：{elapsed:?} → {}",
            outcome.result.content
        );
    }
    assert!(
        failures < runs,
        "全部 {runs} 轮都失败 —— 检查 crates/testkit/config.toml 与服务端状态"
    );
}
