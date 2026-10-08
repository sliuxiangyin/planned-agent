//! 不触网的冒烟测试 —— `cargo test -p planned-agent-testkit` 默认跑这些，
//! 必须全绿且不要求密钥（设计稿 §7 验收 2）。

use planned_agent_testkit::{TestConfig, TestHarness};
use serde_json::json;

/// `config.toml.example` 必须能被解析 —— 模板格式与代码同步的回归锁。
#[test]
fn example_config_parses() {
    let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config.toml.example");
    let config = TestConfig::load(&example).expect("config.toml.example 应当可解析");
    assert!(
        !config.ai_providers.is_empty(),
        "模板里应当有一个示例 provider"
    );
}

/// 不注入 AI → 视觉类工具**不注册**（与 GUI 的降级路径同构）。
#[tokio::test]
async fn harness_without_ai_skips_vision_tools() {
    let harness = TestHarness::builder().build().expect("构造 harness");

    let error = harness
        .call("builtin_solve_captcha", json!({ "path": "unused.png" }))
        .await
        .expect_err("没有 AI 就不该注册验证码工具");

    // `TestHarness::call` 会给错误带上工具名上下文，故这里能断到工具名
    assert!(
        error.to_string().contains("builtin_solve_captcha"),
        "{error:#}"
    );
}

/// 非视觉的内置工具确实注册了 —— 用一个真文件走一次 `builtin_read_text_file`。
#[tokio::test]
async fn harness_registers_non_vision_tools() {
    let harness = TestHarness::builder().build().expect("构造 harness");
    let file = harness.sandbox_root().join("hello.txt");
    std::fs::write(&file, "hi\n").expect("写测试文件");

    let outcome = harness
        .call(
            "builtin_read_text_file",
            json!({ "path": file.to_string_lossy() }),
        )
        .await
        .expect("内置工具应当已注册");

    assert!(!outcome.result.is_error, "{:?}", outcome.result);
}
