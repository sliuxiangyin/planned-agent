//! 集成测试：MCP 客户端连接失败的错误分类
//!
//! 覆盖场景：
//! 1. **Timeout**：使用 `sleep` 作为假 server，配短超时，验证 spawn 成功但握手超时
//!    （即 `npx` 首次拉包慢的等价场景）
//! 2. **Spawn**：使用不存在的命令，验证 spawn 失败被分类
//! 3. **Handshake + stderr**：使用 bash 子进程向 stderr 写错误后立即退出，
//!    验证子进程真实报错能被捕获并显示给用户（解决"UI 显示 connection closed 但终端显示 MODULE_NOT_FOUND"的问题）
//! 4. **status 暴露**：失败后 `connection_status()` 能读到对应 `ConnectionError`

use planned_agent_core::mcp::types::{ConnectionError, McpServerConfig};
use planned_agent_mcp_rmcp::McpClientImpl;
use planned_agent_core::mcp::McpClient;

fn sleep_config(timeout_secs: u64) -> McpServerConfig {
    // `sleep 999` 会成功 spawn，但永远不会发出 MCP initialize 响应
    // —— 这正是 "npx 拉包卡住 / 慢" 的等价模拟场景
    McpServerConfig {
        name: "fake-sleep".into(),
        server_command: "sleep".into(),
        server_args: vec!["999".into()],
        transport: "stdio".into(),
        timeout_secs: Some(timeout_secs),
        handshake_timeout_secs: None,
        max_retries: None,
        is_default: false,
        tools_filter: None,
        categories: None,
    }
}

fn nonexistent_config() -> McpServerConfig {
    McpServerConfig {
        name: "ghost".into(),
        server_command: "/this/command/definitely/does/not/exist__xyz_42".into(),
        server_args: vec![],
        transport: "stdio".into(),
        timeout_secs: Some(2),
        handshake_timeout_secs: None,
        max_retries: None,
        is_default: false,
        tools_filter: None,
        categories: None,
    }
}

/// 静默进程（stderr 无任何输出，如 sleep）：用于验证"无输出按握手线提前失败"
fn silent_sleep_config(timeout_secs: u64, handshake_secs: u64) -> McpServerConfig {
    McpServerConfig {
        name: "silent-sleep".into(),
        server_command: "sleep".into(),
        server_args: vec!["999".into()],
        transport: "stdio".into(),
        timeout_secs: Some(timeout_secs),
        handshake_timeout_secs: Some(handshake_secs),
        max_retries: None,
        is_default: false,
        tools_filter: None,
        categories: None,
    }
}

/// 有输出的进程（stderr 写一行后长睡）：用于验证"有输出按总上限等待"
fn noisy_sleep_config(timeout_secs: u64, handshake_secs: u64) -> McpServerConfig {
    McpServerConfig {
        name: "noisy-sleep".into(),
        server_command: "bash".into(),
        server_args: vec!["-c".into(), "echo alive 1>&2; sleep 5".into()],
        transport: "stdio".into(),
        timeout_secs: Some(timeout_secs),
        handshake_timeout_secs: Some(handshake_secs),
        max_retries: None,
        is_default: false,
        tools_filter: None,
        categories: None,
    }
}

/// 模拟"npx 拉包失败"的典型场景：
/// bash 启动 → 写入 fake MODULE_NOT_FOUND 到 stderr → 立即 exit(1)
/// rmcp 这边会看到 stdout 关闭 → 返回 "connection closed: initialize response"
fn crashing_stderr_config() -> McpServerConfig {
    McpServerConfig {
        name: "crash-with-stderr".into(),
        // bash -c 子进程成功 spawn，向 stderr 写错误后退出，
        // 完全不会回应 MCP initialize — 等价于 npx 拉包后 node 抛 MODULE_NOT_FOUND 退出
        server_command: "bash".into(),
        server_args: vec![
            "-c".into(),
            r#"echo "Error: Cannot find module './api/index'" 1>&2
echo "Require stack:" 1>&2
echo "- /tmp/pdf-lib/cjs/index.js" 1>&2
echo "    code: 'MODULE_NOT_FOUND'" 1>&2
exit 1"#
                .into(),
        ],
        transport: "stdio".into(),
        timeout_secs: Some(3),
        handshake_timeout_secs: None,
        max_retries: None,
        is_default: false,
        tools_filter: None,
        categories: None,
    }
}

#[tokio::test]
async fn connect_times_out_and_records_timeout_error() {
    let mut client = McpClientImpl::new();

    // 1 秒超时；sleep 999 永远不会响应 MCP 握手
    let result = client.connect(sleep_config(1)).await;

    assert!(result.is_err(), "应当因超时返回 Err");

    // 连接状态：未连接 + 上次错误为 Timeout
    let status = client.connection_status().await;
    assert!(!status.connected, "超时后不应处于 connected 状态");

    match &status.last_error {
        Some(ConnectionError::Timeout {
            elapsed_secs,
            timeout_secs,
            stderr_tail,
        }) => {
            assert_eq!(*timeout_secs, 1, "记录的超时上限应等于配置值");
            // elapsed 应该至少接近 1s（受 tokio 调度影响，允许一定偏差）
            assert!(
                *elapsed_secs >= 1 && *elapsed_secs < 10,
                "elapsed_secs 应在合理范围，实际 {}",
                elapsed_secs
            );
            // sleep 999 不会写 stderr，这里应该是 None
            assert!(
                stderr_tail.is_none(),
                "sleep 999 不应产生 stderr，得到: {:?}",
                stderr_tail
            );
        }
        other => panic!("期望 Timeout 错误，实际: {:?}", other),
    }

    // 错误消息应包含给用户的可读提示
    let msg = status.last_error.unwrap().message();
    assert!(msg.contains("timed out"), "错误消息应说明超时: {}", msg);

    // 给 rmcp 在 Drop 时 spawn 的 kill 子进程任务留出执行窗口，
    // 避免 cargo test 退出阶段等待 sleep 孤儿进程
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let _ = client.disconnect().await;
}

#[tokio::test]
async fn connect_records_spawn_error_for_missing_command() {
    let mut client = McpClientImpl::new();
    let result = client.connect(nonexistent_config()).await;

    assert!(result.is_err(), "不存在的命令应当返回 Err");

    let status = client.connection_status().await;
    assert!(!status.connected);

    match &status.last_error {
        Some(ConnectionError::Spawn { reason }) => {
            assert!(!reason.is_empty(), "Spawn 错误应附带 reason");
        }
        other => panic!("期望 Spawn 错误，实际: {:?}", other),
    }
}

#[tokio::test]
async fn handshake_failure_captures_subprocess_stderr() {
    // ─── 核心场景：npx 拉包时内部模块崩溃 ───
    // 子进程成功 spawn → 写真实错误到 stderr → 立即退出
    // 我们应当捕获 stderr 并通过 ConnectionError::Handshake.stderr_tail 透出
    let mut client = McpClientImpl::new();
    let result = client.connect(crashing_stderr_config()).await;

    assert!(result.is_err(), "崩溃子进程应触发 Err");

    let status = client.connection_status().await;
    assert!(!status.connected);

    let err = status.last_error.expect("应记录到 last_error");
    match &err {
        ConnectionError::Handshake {
            reason,
            stderr_tail,
        } => {
            // reason 是 rmcp 给的二手描述（connection closed）
            assert!(
                reason.contains("connection closed")
                    || reason.contains("initialize"),
                "reason 应来自 rmcp 的握手失败消息，实际: {}",
                reason
            );

            // 关键断言：stderr_tail 必须包含子进程真实错误
            let stderr = stderr_tail
                .as_deref()
                .expect("崩溃子进程应产生 stderr_tail");
            assert!(
                stderr.contains("MODULE_NOT_FOUND"),
                "stderr_tail 应包含子进程真实错误，实际: {}",
                stderr
            );
            assert!(
                stderr.contains("Cannot find module"),
                "stderr_tail 应包含模块缺失消息，实际: {}",
                stderr
            );

            // 错误消息组合（base + stderr 段落）
            let msg = err.message();
            assert!(
                msg.contains("MCP handshake failed"),
                "消息应包含 handshake 描述，实际: {}",
                msg
            );
            assert!(
                msg.contains("subprocess stderr"),
                "消息应包含 stderr 段落标题，实际: {}",
                msg
            );
            assert!(
                msg.contains("MODULE_NOT_FOUND"),
                "消息应透传真实错误给用户，实际: {}",
                msg
            );
        }
        other => panic!("期望 Handshake 错误，实际: {:?}", other),
    }
}

#[tokio::test]
async fn connection_error_serializes_to_json() {
    // 给未来 UI / IPC 消费方一个明确的契约样例
    let err = ConnectionError::Timeout {
        elapsed_secs: 120,
        timeout_secs: 60,
        stderr_tail: Some("Module not found: foo".into()),
    };
    let json = serde_json::to_string(&err).unwrap();
    assert!(json.contains("\"kind\":\"timeout\""));
    assert!(json.contains("\"elapsed_secs\":120"));
    assert!(json.contains("\"timeout_secs\":60"));
    assert!(
        json.contains("\"stderr_tail\""),
        "stderr_tail 应参与序列化"
    );

    // 反向：不带 stderr_tail 的 Handshake 不应输出 None 字段
    let err2 = ConnectionError::Handshake {
        reason: "bad".into(),
        stderr_tail: None,
    };
    let json2 = serde_json::to_string(&err2).unwrap();
    assert!(
        !json2.contains("stderr_tail"),
        "stderr_tail=None 应被 skip_serializing_if 跳过"
    );
}

#[tokio::test]
async fn silent_process_times_out_early_on_handshake_limit() {
    // 静默进程（sleep，stderr 无输出）：应走"握手提前失败线"，≈1s 快速失败，
    // 而不是干等总上限 10s
    let mut client = McpClientImpl::new();
    let result = client.connect(silent_sleep_config(10, 1)).await;

    assert!(result.is_err(), "静默进程应超时");

    let status = client.connection_status().await;
    match &status.last_error {
        Some(ConnectionError::Timeout {
            elapsed_secs,
            timeout_secs,
            ..
        }) => {
            assert!(
                *elapsed_secs >= 1 && *elapsed_secs < 10,
                "无输出时应约 1s 提前失败（握手线），实际 {}s",
                elapsed_secs
            );
            assert_eq!(
                *timeout_secs, 1,
                "无输出时应报握手线 1s，实际 {}",
                timeout_secs
            );
        }
        other => panic!("期望 Timeout，实际: {:?}", other),
    }
}

#[tokio::test]
async fn noisy_process_waits_full_startup_timeout() {
    // 有输出的进程（bash 向 stderr 写一行后长睡）：stderr 有数据 → 确认进程激活，
    // 应切回总上限 3s 等待（而非握手线 1s 提前失败）
    let mut client = McpClientImpl::new();
    let result = client.connect(noisy_sleep_config(3, 1)).await;

    assert!(result.is_err(), "有输出但未握手应超时");

    let status = client.connection_status().await;
    match &status.last_error {
        Some(ConnectionError::Timeout {
            elapsed_secs,
            timeout_secs,
            stderr_tail,
        }) => {
            assert!(
                *elapsed_secs >= 3 && *elapsed_secs < 10,
                "有输出时应等总上限约 3s，实际 {}s",
                elapsed_secs
            );
            assert_eq!(
                *timeout_secs, 3,
                "有输出时应报总上限 3s，实际 {}",
                timeout_secs
            );
            // 关键：stderr_tail 应完整包含探测前缀 + 剩余内容（首字符不丢）
            let stderr = stderr_tail
                .as_deref()
                .expect("有输出的进程应产生 stderr_tail");
            assert!(
                stderr.contains("alive"),
                "stderr_tail 应包含完整内容（含探测读走的首字节），实际: {}",
                stderr
            );
        }
        other => panic!("期望 Timeout，实际: {:?}", other),
    }
}