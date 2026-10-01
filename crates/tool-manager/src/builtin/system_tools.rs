use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use sysinfo::System;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::{BuiltinToolProvider, ToolCategory, ToolExecutor};

// ── 契约常量 ────────────────────────────────────────────────────────────────
//
// schema 里的 `default` / `minimum` / `maximum` 在本仓库**运行时不生效**：
// `ToolValidator::validate_arguments`（core/validator.rs）只检查 `required` 与字段类型，
// 且类型不匹配也只 warn。所以这里的默认值与上下限必须在实现里自己兜。
// 设计依据：docs/planned-agent/system-tools-redesign.md §3.1。

/// `timeout_ms` 默认 30 秒。
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// `timeout_ms` 下限。
const MIN_TIMEOUT_MS: u64 = 100;
/// `timeout_ms` 上限（5 分钟）。
const MAX_TIMEOUT_MS: u64 = 300_000;

/// `max_output_bytes` 默认 256 KiB（stdout / stderr 各自）。
const DEFAULT_MAX_OUTPUT_BYTES: u64 = 262_144;
/// `max_output_bytes` 下限。
const MIN_MAX_OUTPUT_BYTES: u64 = 1_024;
/// `max_output_bytes` 上限（10 MiB）。
const MAX_MAX_OUTPUT_BYTES: u64 = 10 * 1_048_576;

/// 收割读取任务的兜底时长。
///
/// 子进程被杀后，若它 fork 出的孙进程仍持有管道写端，读取任务可能迟迟读不到 EOF ——
/// 没有这个上限，一次超时就会退化成「永久挂起」，而那正是本次要消灭的 bug。
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// `builtin_list_processes` 默认 / 最大返回条数。
const DEFAULT_PROCESS_LIMIT: u64 = 200;
const MAX_PROCESS_LIMIT: u64 = 2_000;

/// `builtin_list_env` 默认 / 最大返回条数。
const DEFAULT_ENV_LIMIT: u64 = 200;
const MAX_ENV_LIMIT: u64 = 2_000;

/// 敏感环境变量的遮蔽值。
const MASK: &str = "***";

/// 敏感环境变量名判定用的片段。
///
/// 刻意不包含裸 `AUTH`：那样会把 `GIT_AUTHOR_NAME` 这类无关变量一起打码。
/// 代价是误报只多不少 —— 打码一个不敏感的值无害，泄漏一个敏感值有害。
///
/// **边界（别高估它）**：脱敏只能阻止敏感值进入对话上下文 / trace / 落库，
/// **拦不住有命令执行权的模型主动去读** —— 子进程继承宿主完整环境，
/// `cmd /C set OPENAI_API_KEY` 一样能拿到明文。不在 spawn 前剔除敏感变量，
/// 是因为那会打断 `GITHUB_TOKEN` 拉私有依赖、`AWS_*` 部署这类正当用法
/// （属「tool 替调用方做决策」）。见 docs/planned-agent/system-tools-redesign.md §5。
const SENSITIVE_ENV_TOKENS: &[&str] = &[
    "_KEY",
    "APIKEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "PASS",
    "BEARER",
    "JWT",
    "CREDENTIAL",
    "SESSION",
    "COOKIE",
    "PRIVATE_",
    "AWS_",
    "OPENAI_",
    "ANTHROPIC_",
    "GITHUB_",
];

/// `taskkill` 杀进程树时的兜底超时（Windows）。
#[cfg(windows)]
const TASKKILL_TIMEOUT: Duration = Duration::from_secs(5);

/// Windows 下创建无控制台窗口的进程（GUI 进程里执行命令不再闪黑框）。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 内置系统工具提供者（跨平台）
pub struct SystemToolsProvider;

impl BuiltinToolProvider for SystemToolsProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![
            // 系统命令工具
            (
                Tool {
                    name: "builtin_execute_command".to_string(),
                    description:
                        "执行系统命令，返回 exit_code、stdout、stderr。
                        关键约定：
                        - 不经过 shell：command 是可执行文件名（如 \"git\"），args 是参数数组，逐项传参。
                          管道 / 重定向 / shell 内建命令都不可用；需要时把 shell 名填在 command、脚本放 args。
                        - exit_code 非 0 表示命令自身失败，工具调用本身仍算成功，请据 stdout / stderr 判断原因。
                        - 输出为 UTF-8（非法字节会损坏），换行统一为 \n。
                        - 默认 30 秒超时，长任务请显式提高 timeout_ms。"
                            .to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "command": {
                                "type": "string",
                                "minLength": 1,
                                "maxLength": 256,
                                "description": "可执行文件名（不是命令行字符串）：git、cargo、node。要套 shell 就把 shell 名填在这里、脚本放 args。"
                            },
                            "args": {
                                "type": "array",
                                "items": { "type": "string" },
                                "default": [],
                                "description": "参数列表，逐项传递，不做字符串拼接。"
                            },
                            "working_dir": {
                                "type": "string",
                                "description": "工作目录绝对路径，省略则用宿主当前目录。"
                            },
                            "timeout_ms": {
                                "type": "integer",
                                "minimum": 100,
                                "maximum": 300000,
                                "default": 30000,
                                "description": "超时毫秒数。默认 30000（30 秒）；编译、测试、安装依赖、下载等可能超过 30 秒的任务（如 cargo build、cargo test、npm install、pip install、make、docker build）请显式提高。"
                            },
                            "env": {
                                "type": "object",
                                "additionalProperties": { "type": "string" },
                                "description": "追加/覆盖的子进程环境变量，不清空继承的环境。"
                            },
                            "stdin": {
                                "type": "string",
                                "description": "写入子进程标准输入的内容（UTF-8）。省略则不写入。"
                            },
                            "max_output_bytes": {
                                "type": "integer",
                                "minimum": 1024,
                                "maximum": 10485760,
                                "default": 262144,
                                "description": "stdout / stderr 各自保留的最大字节数，超出截断并标记。默认 262144（256 KiB）。"
                            }
                        },
                        "required": ["command"],
                        "additionalProperties": false
                    }),
                },
                vec![ToolCategory::System],
            ),
            (
                Tool {
                    name: "builtin_command_exists".to_string(),
                    description: "检查命令是否存在，存在时回传解析到的绝对路径（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "command": { "type": "string", "description": "要检查的命令" }
                        },
                        "required": ["command"]
                    }),
                },
                vec![ToolCategory::System],
            ),
            // 进程管理工具
            (
                Tool {
                    name: "builtin_list_processes".to_string(),
                    description: "列出系统进程（跨平台）（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "filter": { "type": "string", "description": "按进程名过滤（不区分大小写，可选）" },
                            "limit": {
                                "type": "integer",
                                "minimum": 1,
                                "maximum": 2000,
                                "default": 200,
                                "description": "最多返回多少条（默认 200）。total 字段给出匹配总数。"
                            }
                        }
                    }),
                },
                vec![ToolCategory::System],
            ),
            (
                Tool {
                    name: "builtin_get_process_info".to_string(),
                    description: "获取进程详细信息（跨平台）（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "pid": { "type": "integer", "description": "进程ID" }
                        },
                        "required": ["pid"]
                    }),
                },
                vec![ToolCategory::System],
            ),
            (
                Tool {
                    name: "builtin_kill_process".to_string(),
                    description: "终止进程（跨平台）（内置工具）。拒绝终止本程序自身与系统进程（pid ≤ 1）。"
                        .to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "pid": { "type": "integer", "description": "进程ID" }
                        },
                        "required": ["pid"]
                    }),
                },
                vec![ToolCategory::System],
            ),
            // 环境变量工具
            (
                Tool {
                    name: "builtin_get_env".to_string(),
                    description: format!(
                        "获取环境变量（内置工具）。敏感变量（名字含 {:?} 之一，如 OPENAI_API_KEY）的值\
                         默认遮蔽为「{MASK}」；确实需要明文时显式传 allow_sensitive=true。",
                        SENSITIVE_ENV_TOKENS
                    ),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "description": "环境变量名" },
                            "allow_sensitive": {
                                "type": "boolean",
                                "default": false,
                                "description": "默认 false：敏感变量返回遮蔽值。仅在确需明文时设为 true。"
                            }
                        },
                        "required": ["name"]
                    }),
                },
                vec![ToolCategory::System],
            ),
            (
                Tool {
                    name: "builtin_list_env".to_string(),
                    description: format!(
                        "列出环境变量（内置工具）。敏感变量（名字含 {:?} 之一）的值默认遮蔽为「{MASK}」，\
                         masked_count 给出被遮蔽的条数；确实需要明文时显式传 allow_sensitive=true。",
                        SENSITIVE_ENV_TOKENS
                    ),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "filter": { "type": "string", "description": "按变量名过滤（区分大小写，可选）" },
                            "limit": {
                                "type": "integer",
                                "minimum": 1,
                                "maximum": 2000,
                                "default": 200,
                                "description": "最多返回多少条（默认 200）。total 字段给出匹配总数。"
                            },
                            "allow_sensitive": {
                                "type": "boolean",
                                "default": false,
                                "description": "默认 false：敏感变量的值返回遮蔽值。仅在确需明文时设为 true。"
                            }
                        }
                    }),
                },
                vec![ToolCategory::System],
            ),
        ]
    }

    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(SystemToolsExecutor)
    }
}

/// 系统工具执行器
struct SystemToolsExecutor;

#[async_trait]
impl ToolExecutor for SystemToolsExecutor {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        // 约定：可预期的失败一律返回 `Ok(ToolResult { is_error: true, content: {error, message} })`，
        // **不用 `Err`** —— 上层（chat/driver/round/handlers.rs、flexible/step.rs）会把 `Err` 拍平成
        // 一个字符串，错误码与结构全部丢失。`Err` 只留给「未知工具名」这种编程错误。
        Ok(match tool_name {
            "builtin_execute_command" => execute_command(&arguments).await,
            "builtin_command_exists" => command_exists(&arguments),
            "builtin_list_processes" => list_processes(&arguments),
            "builtin_get_process_info" => get_process_info(&arguments),
            "builtin_kill_process" => kill_process(&arguments),
            "builtin_get_env" => get_env(&arguments),
            "builtin_list_env" => list_env(&arguments),
            _ => return Err(anyhow::anyhow!("Unknown tool: {}", tool_name)),
        })
    }

    fn name(&self) -> &str {
        "builtin_system_tools"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec![
            "builtin_execute_command".to_string(),
            "builtin_command_exists".to_string(),
            "builtin_list_processes".to_string(),
            "builtin_get_process_info".to_string(),
            "builtin_kill_process".to_string(),
            "builtin_get_env".to_string(),
            "builtin_list_env".to_string(),
        ]
    }
}

// ── 结果构造 ────────────────────────────────────────────────────────────────

fn tool_result(content: Value, is_error: bool) -> ToolResult {
    ToolResult {
        call_id: uuid::Uuid::new_v4().to_string(),
        content,
        is_error,
    }
}

/// 构造带 **错误码** 的失败结果（错误码表见设计稿 §3.3）。
fn failure(code: &str, message: impl Into<String>) -> ToolResult {
    tool_result(json!({ "error": code, "message": message.into() }), true)
}

/// 取整数参数并按上下限钳制。schema 的 default/min/max 运行时不生效，这里自己兜。
fn clamped_u64(value: Option<&Value>, default: u64, min: u64, max: u64) -> u64 {
    value
        .and_then(Value::as_u64)
        .unwrap_or(default)
        .clamp(min, max)
}

// ── builtin_execute_command ─────────────────────────────────────────────────

/// 单条管道的有上限字节收集器。
///
/// 存**字节**而不是 `String`：逐块 `from_utf8_lossy` 会把跨块边界的多字节字符切坏。
#[derive(Default)]
struct CappedBytes {
    bytes: Vec<u8>,
    total: usize,
}

type SharedBytes = Arc<Mutex<CappedBytes>>;

/// 把 `reader` 读干：保留前 `cap` 字节，其余**继续读并丢弃**。
///
/// 「继续读」不是可选项 —— 读满上限就停会让子进程写满管道后永久阻塞，
/// 于是每次截断都退化成一次超时。
async fn pump_capped<R>(mut reader: R, cap: usize, sink: SharedBytes)
where
    R: AsyncRead + Unpin,
{
    let mut chunk = [0u8; 8 * 1024];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                let mut guard = sink.lock().unwrap_or_else(|poison| poison.into_inner());
                guard.total += n;
                let room = cap.saturating_sub(guard.bytes.len());
                if room > 0 {
                    guard.bytes.extend_from_slice(&chunk[..room.min(n)]);
                }
            }
            Err(_) => break,
        }
    }
}

/// 取得收集结果：转字符串 + 换行归一化，并报告是否被截断。
fn finalize(sink: &SharedBytes) -> (String, bool) {
    let guard = sink.lock().unwrap_or_else(|poison| poison.into_inner());
    let text = String::from_utf8_lossy(&guard.bytes).to_string();
    (normalize_newlines(text), guard.total > guard.bytes.len())
}

/// `\r\n` → `\n`。
///
/// 这是**改写调用方的原始输出**，因此必须写进 description（已写）—— 否则调用方会以为
/// 拿到的是原始字节。它只做平台差异归一化、对所有平台一致、不改变命令语义，
/// 与 `CREATE_NO_WINDOW` 同属「例外」。
fn normalize_newlines(text: String) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n")
    } else {
        text
    }
}

/// 解析要执行的程序：只在「名字不含路径分隔符」时介入。
///
/// 为什么需要：Windows 上 `Command::new("npm")` 只给无扩展名的名字补 `.exe`、**不做 PATHEXT
/// 展开**（`rust-lang/rust#37519`，至今 open），而 `which::which("npm")` 会解析到 `npm.cmd`
/// —— 不解析就会出现「`builtin_command_exists` 说存在、`builtin_execute_command` 却失败」的
/// 自相矛盾（`npm` / `pnpm` / `yarn` / `tsc` 整条 Node 工具链）。
///
/// 解析到 `.cmd` / `.bat` 后，std 会自动改用 `cmd.exe` 执行它 —— 那是 OS 层「按文件名找程序」，
/// 不是我们「套 shell」。含路径分隔符的名字原样返回：调用方的显式路径优先，且真实路径含空格也照旧放行。
///
/// 这不违反「tool 不做平台决策」：它不改变语义、不解释命令、不改写调用方输入，
/// 只是把「这个裸名在本机对应哪个文件」问了一次 OS（与 `CREATE_NO_WINDOW` 同类「例外」）。
fn resolve_command(command: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    if command.contains('/') || command.contains('\\') {
        return Cow::Borrowed(command);
    }
    match which::which(command) {
        Ok(path) => Cow::Owned(path.to_string_lossy().into_owned()),
        Err(_) => Cow::Borrowed(command),
    }
}

/// 超时后回收子进程 —— 尽力连带它的进程树。
///
/// `Child::kill`（`start_kill`）在 Windows 上只杀**直接**子进程；而 `npm` / `pnpm` / `yarn` 是
/// 「启动器 + 子进程」模型，只杀启动器会把 `node` 留成孤儿。Windows 上再补一次
/// `taskkill /F /T /PID <pid>`（`/T` = 连带子树），零新增依赖、无需 unsafe。
///
/// 非 Windows 平台只有 `kill_on_drop` + `start_kill`（只保证直接子进程）——
/// 完整的进程树回收需要 `process_group` + `killpg`（新依赖）或 Job Object（新依赖 + unsafe），
/// 见 docs/planned-agent/system-tools-redesign.md §7 Q4。
async fn kill_process_tree(child: &mut tokio::process::Child) {
    // 先杀树、后回收：taskkill 需要 pid 仍然存在才有意义。
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let pid_text = pid.to_string();
        let _ = tokio::time::timeout(
            TASKKILL_TIMEOUT,
            tokio::process::Command::new("taskkill")
                .args(["/F", "/T", "/PID", pid_text.as_str()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        )
        .await;
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

fn spawn_failure(command: &str, error: &std::io::Error) -> ToolResult {
    use std::io::ErrorKind;
    let (code, hint) = match error.kind() {
        ErrorKind::NotFound => (
            "executable_not_found",
            "（本机找不到这个可执行文件，请换命令，或改用运行环境段里已探测到可用的命令）",
        ),
        ErrorKind::PermissionDenied => ("permission_denied", "（没有执行权限）"),
        _ => ("spawn_failed", ""),
    };
    failure(code, format!("启动「{command}」失败：{error}{hint}"))
}

async fn execute_command(arguments: &Value) -> ToolResult {
    // ── 入参（含默认值与钳制；schema 的 default/min/max 运行时不生效）──
    let command = match arguments.get("command").and_then(Value::as_str) {
        Some(command) if !command.trim().is_empty() => command.to_string(),
        _ => {
            return failure(
                "invalid_arguments",
                "缺少 command：请传可执行文件名（非空字符串），例如 \"git\"",
            )
        }
    };

    let mut args: Vec<String> = Vec::new();
    match arguments.get("args") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for item in items {
                match item.as_str() {
                    Some(arg) => args.push(arg.to_string()),
                    None => {
                        return failure(
                            "invalid_arguments",
                            "args 的每一项都必须是字符串（不要把整个参数列表拼成一条字符串）",
                        )
                    }
                }
            }
        }
        Some(_) => return failure("invalid_arguments", "args 必须是字符串数组"),
    }

    let working_dir = match arguments.get("working_dir") {
        None | Some(Value::Null) => None,
        Some(Value::String(dir)) if !dir.trim().is_empty() => Some(dir.clone()),
        Some(_) => return failure("invalid_arguments", "working_dir 必须是字符串"),
    };
    if let Some(dir) = &working_dir {
        if !Path::new(dir).is_dir() {
            // 回显路径：`os error 3` 这类错误码没有任何定位价值（`filesystem/support.rs::path_error` 同款做法）。
            return failure(
                "working_dir_invalid",
                format!("工作目录「{dir}」不存在或不是目录（请核对拼写）"),
            );
        }
    }

    let timeout = Duration::from_millis(clamped_u64(
        arguments.get("timeout_ms"),
        DEFAULT_TIMEOUT_MS,
        MIN_TIMEOUT_MS,
        MAX_TIMEOUT_MS,
    ));

    let mut envs: Vec<(String, String)> = Vec::new();
    match arguments.get("env") {
        None | Some(Value::Null) => {}
        Some(Value::Object(map)) => {
            for (key, value) in map {
                match value.as_str() {
                    Some(value) => envs.push((key.clone(), value.to_string())),
                    None => {
                        return failure(
                            "invalid_arguments",
                            format!("env 的「{key}」值必须是字符串"),
                        )
                    }
                }
            }
        }
        Some(_) => return failure("invalid_arguments", "env 必须是对象"),
    }

    let stdin_text = match arguments.get("stdin") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => return failure("invalid_arguments", "stdin 必须是字符串"),
    };

    let cap = clamped_u64(
        arguments.get("max_output_bytes"),
        DEFAULT_MAX_OUTPUT_BYTES,
        MIN_MAX_OUTPUT_BYTES,
        MAX_MAX_OUTPUT_BYTES,
    ) as usize;

    // ── command 语义护栏 ──
    //
    // `command` 会被直接交给 `CreateProcess` / `execve` 当**文件名**用，不可能是一条命令行。
    // 模型把整条命令塞进来时会拿到「系统找不到指定的路径 (os error 3)」——信息量太低，
    // 只能靠试错（历史上白烧两轮）。这里换成人能看懂的指导性错误。
    // 判定加 `exists()` 兜底：带空格的**真实绝对路径**（"C:\Program Files\...\node.exe"）必须放行。
    if command.chars().any(char::is_whitespace) && !Path::new(&command).exists() {
        return failure(
            "invalid_arguments",
            format!(
                "command「{command}」只能是可执行文件名，不能是整条命令行 —— 它会被当作文件名直接查找。\
                 要执行带管道 / 重定向 / shell 内建命令的脚本，请拆开传：command = 本机 shell 名、\
                 args = [\"<脚本>\"]（本机是哪个 shell 见运行环境段）。"
            ),
        );
    }

    // ── 构造进程：默认不经 shell，参数逐项传递 ──
    //
    // 裸名先按本机 PATHEXT 解析：Windows 上 `Command::new("npm")` 只补 `.exe`、不做 PATHEXT 展开，
    // 而 `which` 会解析到 `npm.cmd` —— 不解析就会出现「`builtin_command_exists` 说存在、
    // 本工具却失败」的自相矛盾。含路径分隔符的名字原样交给 OS（调用方的显式路径优先）。
    let resolved = resolve_command(&command);
    let mut cmd = tokio::process::Command::new(resolved.as_ref());
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 不给 stdin 时置 null：piped 但不写会让读 stdin 的子进程永久阻塞。
        .stdin(if stdin_text.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        // 超时被 drop / 提前返回时回收子进程，避免留下孤儿。
        .kill_on_drop(true);
    if let Some(dir) = &working_dir {
        cmd.current_dir(dir);
    }
    for (key, value) in &envs {
        cmd.env(key, value);
    }
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => return spawn_failure(&command, &error),
    };

    // stdin 写入放到独立任务：子进程不读 stdin 时，同步写完会双向死锁。
    if let Some(text) = stdin_text {
        if let Some(mut sink) = child.stdin.take() {
            tokio::spawn(async move {
                let _ = sink.write_all(text.as_bytes()).await;
                let _ = sink.shutdown().await;
            });
        }
    }

    let out_sink: SharedBytes = Arc::new(Mutex::new(CappedBytes::default()));
    let err_sink: SharedBytes = Arc::new(Mutex::new(CappedBytes::default()));
    let out_task = child
        .stdout
        .take()
        .map(|reader| tokio::spawn(pump_capped(reader, cap, out_sink.clone())));
    let err_task = child
        .stderr
        .take()
        .map(|reader| tokio::spawn(pump_capped(reader, cap, err_sink.clone())));

    // ── 等待 + 超时 ──
    //
    // 注意不能用 `child.wait_with_output()`：它会吃掉 `Child` 的所有权，超时后拿不回句柄、
    // 无法 kill。所以先手动接管两条管道（上面），这里只对 `child.wait()` 加超时。
    let started = Instant::now();
    let (exit_code, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => (status.code().unwrap_or(-1), false),
        Ok(Err(error)) => {
            return failure(
                "internal_error",
                format!("等待「{command}」退出时出错：{error}"),
            )
        }
        Err(_) => {
            kill_process_tree(&mut child).await;
            (-1, true)
        }
    };
    let duration_ms = started.elapsed().as_millis() as u64;

    // 收割读取任务（带兜底上限，见 PIPE_DRAIN_TIMEOUT）。
    let drain = async {
        if let Some(task) = out_task {
            let _ = task.await;
        }
        if let Some(task) = err_task {
            let _ = task.await;
        }
    };
    let _ = tokio::time::timeout(PIPE_DRAIN_TIMEOUT, drain).await;

    let (stdout, stdout_truncated) = finalize(&out_sink);
    let (stderr, stderr_truncated) = finalize(&err_sink);
    // `command_line` 记**实际执行**的那个（即解析后的路径）—— 审计要看的是真跑了什么。
    let executed = resolved.as_ref();
    let command_line = if args.is_empty() {
        executed.to_string()
    } else {
        format!("{executed} {}", args.join(" "))
    };

    // 审计：落进既有 tracing 设施（不自建日志文件）。含 command/args/working_dir/exit_code/耗时，
    // 出问题可复现。注意这是**本机日志**——送进 LLM 上下文的环境段仍不写工作目录。
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_execute_command",
        command = %command,
        resolved = %executed,
        args = ?args,
        working_dir = ?working_dir,
        exit_code,
        duration_ms,
        timed_out,
        stdout_truncated,
        stderr_truncated,
        "命令执行完毕"
    );

    // `is_error`：超时才算工具层失败；`exit_code` 非 0 表示命令**自身**失败（grep 无匹配、
    // cargo test 有失败用例都是这种），工具调用本身成功，交给调用方据 stdout/stderr 判断。
    //
    // 超时**同时**给错误码与完整输出：错误码让上层能机械判断（与其它失败一致），
    // 输出则是排查「为什么这么慢」的唯一现场证据（必须回灌给 LLM）。
    let mut content = json!({
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "stdout_truncated": stdout_truncated,
        "stderr_truncated": stderr_truncated,
        "duration_ms": duration_ms,
        "timed_out": timed_out,
        "command_line": command_line,
    });
    if timed_out {
        content["error"] = json!("timeout");
        content["message"] = json!(format!(
            "命令在 {} 毫秒内没有结束，已被终止（子进程已回收，退出码记为 -1）",
            timeout.as_millis()
        ));
    }

    tool_result(content, timed_out)
}

// ── 其余系统工具 ────────────────────────────────────────────────────────────

fn command_exists(arguments: &Value) -> ToolResult {
    let command = match arguments.get("command").and_then(Value::as_str) {
        Some(command) if !command.trim().is_empty() => command,
        _ => return failure("invalid_arguments", "缺少 command"),
    };

    match which::which(command) {
        Ok(path) => tool_result(
            json!({ "exists": true, "path": path.to_string_lossy() }),
            false,
        ),
        Err(_) => tool_result(json!({ "exists": false, "path": Value::Null }), false),
    }
}

fn list_processes(arguments: &Value) -> ToolResult {
    let filter = arguments
        .get("filter")
        .and_then(Value::as_str)
        .unwrap_or("");
    let limit = clamped_u64(
        arguments.get("limit"),
        DEFAULT_PROCESS_LIMIT,
        1,
        MAX_PROCESS_LIMIT,
    ) as usize;

    let mut sys = System::new();
    sys.refresh_all();

    let matched: Vec<Value> = sys
        .processes()
        .iter()
        .filter(|(_, process)| {
            filter.is_empty()
                || process
                    .name()
                    .to_lowercase()
                    .contains(&filter.to_lowercase())
        })
        .map(|(pid, process)| {
            json!({
                "pid": pid.as_u32(),
                "name": process.name(),
                "status": format!("{:?}", process.status()),
                "cpu_usage": process.cpu_usage(),
                "memory": process.memory(),
            })
        })
        .collect();

    let total = matched.len();
    let processes: Vec<Value> = matched.into_iter().take(limit).collect();

    tool_result(
        json!({
            "processes": processes,
            "count": processes.len(),
            "total": total,
            "limit": limit,
        }),
        false,
    )
}

fn get_process_info(arguments: &Value) -> ToolResult {
    let pid = match arguments.get("pid").and_then(Value::as_i64) {
        Some(pid) => pid,
        None => return failure("invalid_arguments", "缺少 pid（整数）"),
    };

    let mut sys = System::new();
    sys.refresh_all();

    let target = sysinfo::Pid::from_u32(pid as u32);
    match sys.process(target) {
        Some(process) => tool_result(
            json!({
                "pid": pid,
                "name": process.name(),
                "exe": process.exe().map(|p| p.to_string_lossy().to_string()),
                "cwd": process.cwd().map(|p| p.to_string_lossy().to_string()),
                "status": format!("{:?}", process.status()),
                "cpu_usage": process.cpu_usage(),
                "memory": process.memory(),
                "parent_pid": process.parent().map(|p| p.as_u32()),
            }),
            false,
        ),
        None => failure("process_not_found", format!("找不到 pid={pid} 的进程")),
    }
}

fn kill_process(arguments: &Value) -> ToolResult {
    let pid = match arguments.get("pid").and_then(Value::as_i64) {
        Some(pid) => pid,
        None => return failure("invalid_arguments", "缺少 pid（整数）"),
    };

    // 护栏：不需要全局一致性，属于「局部也能做对」的那类防护。
    if pid <= 1 {
        return failure(
            "operation_refused",
            format!("拒绝终止 pid={pid}：系统进程（pid ≤ 1）不允许通过本工具终止"),
        );
    }
    if pid as u32 == std::process::id() {
        return failure(
            "operation_refused",
            format!("拒绝终止 pid={pid}：那是本程序自己的进程"),
        );
    }

    let mut sys = System::new();
    sys.refresh_all();

    let target = sysinfo::Pid::from_u32(pid as u32);
    match sys.process(target) {
        Some(process) => {
            if process.kill() {
                tool_result(
                    json!({ "pid": pid, "success": true, "message": "Process terminated" }),
                    false,
                )
            } else {
                failure(
                    "kill_failed",
                    format!("终止 pid={pid} 失败（可能权限不足）"),
                )
            }
        }
        None => failure("process_not_found", format!("找不到 pid={pid} 的进程")),
    }
}

/// 环境变量名是否敏感（大小写不敏感）。
fn is_sensitive_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SENSITIVE_ENV_TOKENS
        .iter()
        .any(|token| upper.contains(token))
}

/// 按策略遮蔽敏感值；命中返回遮蔽串，否则 `None`（调用方回落原值）。
fn masked_value(name: &str, allow_sensitive: bool) -> Option<String> {
    (!allow_sensitive && is_sensitive_env(name)).then(|| MASK.to_string())
}

fn get_env(arguments: &Value) -> ToolResult {
    let name = match arguments.get("name").and_then(Value::as_str) {
        Some(name) if !name.is_empty() => name,
        _ => return failure("invalid_arguments", "缺少 name"),
    };
    let allow_sensitive = arguments
        .get("allow_sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    match std::env::var(name) {
        Ok(value) => {
            let masked = masked_value(name, allow_sensitive);
            let is_masked = masked.is_some();
            tool_result(
                json!({
                    "name": name,
                    "value": masked.unwrap_or(value),
                    "exists": true,
                    "masked": is_masked,
                }),
                false,
            )
        }
        Err(_) => tool_result(
            json!({ "name": name, "value": Value::Null, "exists": false, "masked": false }),
            false,
        ),
    }
}

fn list_env(arguments: &Value) -> ToolResult {
    let filter = arguments
        .get("filter")
        .and_then(Value::as_str)
        .unwrap_or("");
    let limit = clamped_u64(arguments.get("limit"), DEFAULT_ENV_LIMIT, 1, MAX_ENV_LIMIT) as usize;
    let allow_sensitive = arguments
        .get("allow_sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut vars: Vec<(String, String)> = std::env::vars()
        .filter(|(name, _)| filter.is_empty() || name.contains(filter))
        .collect();
    // 环境变量顺序不定（HashMap 语义），排序后再截断，结果才可预期。
    vars.sort_by(|left, right| left.0.cmp(&right.0));

    let total = vars.len();
    vars.truncate(limit);

    let mut masked_count = 0usize;
    let variables: Vec<Value> = vars
        .iter()
        .map(|(name, value)| {
            let masked = masked_value(name, allow_sensitive);
            if masked.is_some() {
                masked_count += 1;
            }
            json!({ "name": name, "value": masked.unwrap_or_else(|| value.clone()) })
        })
        .collect();

    tool_result(
        json!({
            "variables": variables,
            "count": variables.len(),
            "total": total,
            "limit": limit,
            "masked_count": masked_count,
        }),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试里走真实进程的命令一律从这三个 helper 取，避免平台分支散落在每个用例里。
    /// 它们显式把 shell 名填在 `command`、脚本放 `args` —— 正是「不套 shell」约定允许的用法。
    #[cfg(windows)]
    fn failing_command() -> (&'static str, Vec<&'static str>) {
        ("cmd", vec!["/C", "exit 3"])
    }
    #[cfg(unix)]
    fn failing_command() -> (&'static str, Vec<&'static str>) {
        ("sh", vec!["-c", "exit 3"])
    }

    #[cfg(windows)]
    fn slow_command() -> (&'static str, Vec<&'static str>) {
        ("ping", vec!["-n", "10", "127.0.0.1"])
    }
    #[cfg(unix)]
    fn slow_command() -> (&'static str, Vec<&'static str>) {
        ("sleep", vec!["10"])
    }

    #[cfg(windows)]
    fn flood_command() -> (&'static str, Vec<&'static str>) {
        (
            "cmd",
            vec![
                "/C",
                "for /L %i in (1,1,30000) do @echo 0123456789012345678901234567890123456789",
            ],
        )
    }
    #[cfg(unix)]
    fn flood_command() -> (&'static str, Vec<&'static str>) {
        (
            "sh",
            vec![
                "-c",
                "i=0; while [ $i -lt 30000 ]; do echo 0123456789012345678901234567890123456789; i=$((i+1)); done",
            ],
        )
    }

    #[cfg(windows)]
    fn stdin_echo_command() -> (&'static str, Vec<&'static str>) {
        ("cmd", vec!["/C", "more"])
    }
    #[cfg(unix)]
    fn stdin_echo_command() -> (&'static str, Vec<&'static str>) {
        ("sh", vec!["-c", "cat"])
    }

    async fn exec(tool: &str, arguments: Value) -> ToolResult {
        SystemToolsExecutor
            .execute(tool, arguments)
            .await
            .expect("系统工具不该返回 Err（可预期的失败一律走 is_error + error 码）")
    }

    fn message_of(result: &ToolResult) -> String {
        result.content["message"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    // ── 输出收集 ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn pump_caps_output_and_marks_truncation() {
        let sink: SharedBytes = Arc::new(Mutex::new(CappedBytes::default()));
        let payload = vec![b'x'; 5_000];
        pump_capped(&payload[..], 1_024, sink.clone()).await;

        let (text, truncated) = finalize(&sink);
        assert_eq!(text.len(), 1_024);
        assert!(truncated, "超出上限必须标记截断");
    }

    #[tokio::test]
    async fn pump_keeps_short_output_intact() {
        let sink: SharedBytes = Arc::new(Mutex::new(CappedBytes::default()));
        pump_capped(&b"hello"[..], 1_024, sink.clone()).await;

        let (text, truncated) = finalize(&sink);
        assert_eq!(text, "hello");
        assert!(!truncated);
    }

    #[test]
    fn crlf_is_normalized_to_lf() {
        assert_eq!(normalize_newlines("a\r\nb\r\n".to_string()), "a\nb\n");
        assert_eq!(normalize_newlines("a\nb".to_string()), "a\nb");
    }

    // ── 敏感环境变量 ────────────────────────────────────────────────────────

    #[test]
    fn sensitive_env_names_are_detected() {
        assert!(is_sensitive_env("OPENAI_API_KEY"));
        assert!(is_sensitive_env("AWS_SECRET_ACCESS_KEY"));
        assert!(is_sensitive_env("github_token"));
        assert!(!is_sensitive_env("PATH"));
        assert!(
            !is_sensitive_env("GIT_AUTHOR_NAME"),
            "裸 AUTH 不该把 GIT_AUTHOR_NAME 一起误伤"
        );
    }

    #[test]
    fn masking_follows_the_allow_sensitive_switch() {
        assert_eq!(masked_value("OPENAI_API_KEY", false).as_deref(), Some(MASK));
        assert_eq!(masked_value("OPENAI_API_KEY", true), None);
        assert_eq!(masked_value("PATH", false), None);
    }

    #[tokio::test]
    async fn list_env_masks_sensitive_values_by_default() {
        // 造一个当前进程可见的敏感变量，避免依赖宿主环境里恰好存在某个 key。
        std::env::set_var("PLANNED_AGENT_TEST_API_KEY", "super-secret");
        let masked = exec(
            "builtin_list_env",
            json!({ "filter": "PLANNED_AGENT_TEST_" }),
        )
        .await;
        assert_eq!(masked.content["masked_count"], 1);
        assert_eq!(masked.content["variables"][0]["value"], MASK);

        let plain = exec(
            "builtin_list_env",
            json!({ "filter": "PLANNED_AGENT_TEST_", "allow_sensitive": true }),
        )
        .await;
        assert_eq!(plain.content["masked_count"], 0);
        assert_eq!(plain.content["variables"][0]["value"], "super-secret");
    }

    #[tokio::test]
    async fn list_env_reports_total_and_applies_limit() {
        let result = exec("builtin_list_env", json!({ "limit": 1 })).await;
        assert_eq!(result.content["count"], 1);
        assert!(result.content["total"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn set_env_tool_is_gone() {
        assert!(
            !SystemToolsExecutor
                .supported_tools()
                .iter()
                .any(|name| name == "builtin_set_env"),
            "builtin_set_env 改的是宿主进程全局状态，已按设计稿 §5 删除"
        );
        assert!(
            SystemToolsExecutor
                .execute("builtin_set_env", json!({ "name": "X", "value": "Y" }))
                .await
                .is_err(),
            "已删除的工具必须走 Err（未知工具名），而不是悄悄成功"
        );
    }

    // ── execute_command：入参护栏 ───────────────────────────────────────────

    #[tokio::test]
    async fn command_with_whitespace_is_rejected_with_guidance() {
        let result = exec(
            "builtin_execute_command",
            json!({ "command": "git status" }),
        )
        .await;
        assert!(result.is_error);
        assert_eq!(result.content["error"], "invalid_arguments");
        assert!(
            message_of(&result).contains("只能是可执行文件名"),
            "错误必须教调用方怎么改，而不是丢一个 os error 3：{}",
            message_of(&result)
        );
    }

    #[tokio::test]
    async fn missing_working_dir_is_rejected_and_echoes_the_path() {
        let missing = "C:/no-such-dir-planned-agent/definitely-missing";
        let result = exec(
            "builtin_execute_command",
            json!({ "command": "cargo", "working_dir": missing }),
        )
        .await;
        assert!(result.is_error);
        assert_eq!(result.content["error"], "working_dir_invalid");
        assert!(
            message_of(&result).contains(missing),
            "错误必须回显路径，否则无从定位：{}",
            message_of(&result)
        );
    }

    #[tokio::test]
    async fn missing_command_argument_is_reported() {
        let result = exec("builtin_execute_command", json!({})).await;
        assert_eq!(result.content["error"], "invalid_arguments");
    }

    #[tokio::test]
    async fn non_string_args_are_rejected() {
        let result = exec(
            "builtin_execute_command",
            json!({ "command": "cargo", "args": ["--version", 42] }),
        )
        .await;
        assert_eq!(result.content["error"], "invalid_arguments");
    }

    // ── execute_command：真实进程 ───────────────────────────────────────────

    #[tokio::test]
    async fn real_command_reports_zero_exit_code_and_output() {
        // 测试本身就是 cargo 跑起来的 → cargo 必然在 PATH 里（与 core 的探测测试同一前提）。
        let result = exec(
            "builtin_execute_command",
            json!({ "command": "cargo", "args": ["--version"], "timeout_ms": 60_000 }),
        )
        .await;
        assert!(
            !result.is_error,
            "cargo --version 应当成功：{}",
            result.content
        );
        assert_eq!(result.content["exit_code"], 0);
        assert!(!result.content["timed_out"].as_bool().unwrap());
        assert!(
            result.content["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("cargo"),
            "stdout 应含有版本信息：{}",
            result.content["stdout"]
        );
    }

    #[tokio::test]
    async fn non_zero_exit_is_not_a_tool_error() {
        let (command, args) = failing_command();
        let result = exec(
            "builtin_execute_command",
            json!({ "command": command, "args": args, "timeout_ms": 60_000 }),
        )
        .await;

        assert_eq!(result.content["exit_code"], 3);
        assert!(
            !result.is_error,
            "命令自身返回非 0 不等于工具调用失败（设计稿 P2）：{}",
            result.content
        );
    }

    #[tokio::test]
    async fn unknown_executable_reports_executable_not_found() {
        let result = exec(
            "builtin_execute_command",
            json!({ "command": "planned-agent-no-such-binary-xyz" }),
        )
        .await;
        assert!(result.is_error);
        assert_eq!(result.content["error"], "executable_not_found");
        assert!(
            message_of(&result).contains("planned-agent-no-such-binary-xyz"),
            "错误必须回显命令名：{}",
            message_of(&result)
        );
    }

    #[tokio::test]
    async fn timeout_is_flagged_and_reclaims_the_child() {
        let (command, args) = slow_command();
        let result = exec(
            "builtin_execute_command",
            json!({ "command": command, "args": args, "timeout_ms": 300 }),
        )
        .await;

        assert!(result.is_error, "超时属于工具层失败：{}", result.content);
        assert_eq!(result.content["error"], "timeout");
        assert_eq!(result.content["timed_out"], true);
        assert_eq!(result.content["exit_code"], -1);
        assert!(
            result.content["stdout_truncated"].is_boolean() && result.content["stdout"].is_string(),
            "超时也要带回已收到的输出，那才是排查现场：{}",
            result.content
        );
        assert!(
            result.content["duration_ms"].as_u64().unwrap() < 5_000,
            "超时后应当立刻回收，而不是等命令自然结束：{}",
            result.content["duration_ms"]
        );
    }

    #[tokio::test]
    async fn large_output_is_truncated_without_deadlocking_the_child() {
        let (command, args) = flood_command();
        let result = exec(
            "builtin_execute_command",
            json!({
                "command": command,
                "args": args,
                "max_output_bytes": 1024,
                "timeout_ms": 60_000
            }),
        )
        .await;

        assert_eq!(
            result.content["stdout_truncated"], true,
            "超过 max_output_bytes 必须标记截断：{}",
            result.content
        );
        assert!(
            result.content["stdout"].as_str().unwrap().len() <= 1024,
            "保留的字节数不得超过上限"
        );
        assert_eq!(
            result.content["timed_out"], false,
            "读满上限后必须继续 drain 管道，否则子进程会写满管道卡死并退化成超时：{}",
            result.content
        );
    }

    #[tokio::test]
    async fn stdin_is_piped_to_the_child() {
        let (command, args) = stdin_echo_command();
        let result = exec(
            "builtin_execute_command",
            json!({
                "command": command,
                "args": args,
                "stdin": "piped-payload\n",
                "timeout_ms": 30_000
            }),
        )
        .await;

        assert!(!result.is_error, "{}", result.content);
        assert!(
            result.content["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("piped-payload"),
            "stdin 的内容应当被透传：{}",
            result.content
        );
    }

    #[tokio::test]
    async fn env_is_passed_to_the_child() {
        // 用 shell 内建回显环境变量：`echo` 的展开是 shell 的活，不是工具的。
        #[cfg(windows)]
        let (command, args) = ("cmd", vec!["/C", "echo %PLANNED_AGENT_TEST_ENV%"]);
        #[cfg(unix)]
        let (command, args) = ("sh", vec!["-c", "echo $PLANNED_AGENT_TEST_ENV"]);

        let result = exec(
            "builtin_execute_command",
            json!({
                "command": command,
                "args": args,
                "env": { "PLANNED_AGENT_TEST_ENV": "env-payload" },
                "timeout_ms": 30_000
            }),
        )
        .await;

        assert!(!result.is_error, "{}", result.content);
        assert!(
            result.content["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("env-payload"),
            "env 参数应当写进子进程环境：{}",
            result.content
        );
    }

    // ── 其余工具 ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn command_exists_reports_resolved_path() {
        let result = exec("builtin_command_exists", json!({ "command": "cargo" })).await;
        assert_eq!(result.content["exists"], true);
        assert!(
            result.content["path"].is_string(),
            "命中时应当回传解析到的绝对路径：{}",
            result.content
        );
    }

    #[tokio::test]
    async fn list_processes_respects_limit() {
        let result = exec("builtin_list_processes", json!({ "limit": 1 })).await;
        assert!(!result.is_error);
        assert!(result.content["processes"].as_array().unwrap().len() <= 1);
        assert!(result.content["total"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn kill_own_process_is_refused() {
        let result = exec(
            "builtin_kill_process",
            json!({ "pid": std::process::id() as i64 }),
        )
        .await;
        assert!(result.is_error);
        assert_eq!(result.content["error"], "operation_refused");
    }

    #[tokio::test]
    async fn kill_pid_one_is_refused() {
        let result = exec("builtin_kill_process", json!({ "pid": 1 })).await;
        assert_eq!(result.content["error"], "operation_refused");
    }

    #[tokio::test]
    async fn get_process_info_reports_missing_pid_with_a_code() {
        // 取一个几乎不可能存在的 pid。
        let result = exec(
            "builtin_get_process_info",
            json!({ "pid": 4_000_000_000_i64 }),
        )
        .await;
        assert!(result.is_error);
        assert_eq!(result.content["error"], "process_not_found");
    }

    // ── 可执行文件名解析（Q2：Windows .cmd / .bat）────────────────────────────

    #[test]
    fn resolve_command_keeps_explicit_paths_untouched() {
        for raw in [
            "C:\\tools\\git.exe",
            "..\\bin\\x",
            "./local-script",
            "/usr/bin/git",
        ] {
            assert_eq!(
                resolve_command(raw),
                raw,
                "含路径分隔符的名字是调用方的显式意图，绝不改写"
            );
        }
    }

    #[test]
    fn resolve_command_resolves_bare_names_on_this_host() {
        // cargo 必然可用：测试本身就是 cargo 跑起来的（与 core 的探测测试同一前提）。
        let resolved = resolve_command("cargo");
        assert_ne!(resolved.as_ref(), "cargo", "本机有 cargo，应解析成完整路径");
        assert!(
            Path::new(resolved.as_ref()).is_absolute(),
            "解析结果应是绝对路径：{resolved}"
        );
    }

    #[test]
    fn resolve_command_falls_back_to_the_original_name() {
        let raw = "planned-agent-no-such-binary-xyz";
        assert_eq!(
            resolve_command(raw),
            raw,
            "解析不到时按原名交给 OS，由 spawn 报 executable_not_found"
        );
    }

    #[test]
    fn resolve_command_handles_windows_cmd_shims_when_present() {
        // 本机装了 Node 时，npm 只有 `.cmd`（`Get-Command npm` → npm.ps1 / npm.cmd）——
        // 这正是 `Command::new("npm")` 会失败、而 `resolve_command` 必须救回来的那个案例。
        // 本机没装就跳过，不假装通过。
        match which::which("npm") {
            Ok(_) => {
                let resolved = resolve_command("npm");
                let path = resolved.as_ref().to_ascii_lowercase();
                assert!(
                    path.ends_with(".cmd") || path.ends_with(".exe") || path.ends_with(".bat"),
                    "npm 应解析到真实可执行文件（.cmd/.exe/.bat）：{resolved}"
                );
            }
            Err(_) => eprintln!("本机没有 npm，跳过 .cmd 解析断言"),
        }
    }

    // ── description 保持静态，不注入平台 ─────────────────────────────────────

    #[test]
    fn description_stays_static_without_platform_injection() {
        let tools = SystemToolsProvider.tools();
        let description = &tools[0].0.description;

        assert!(
            !description.contains(std::env::consts::OS),
            "平台事实由执行期运行环境段提供，不该写进工具静态契约：{description}"
        );
        for token in ["tasklist", "findstr", "{RECOMMENDED}", "{PLATFORM}"] {
            assert!(
                !description.contains(token),
                "平台专属命令与占位符都不该出现在静态契约里（{token}）：{description}"
            );
        }
        assert!(
            description.contains("不经过 shell"),
            "不套 shell 是硬约定，必须留在契约里：{description}"
        );
    }

    // ── 进程树回收（Q4，Windows）─────────────────────────────────────────────

    async fn count_processes_matching(filter: &str) -> usize {
        let result = exec(
            "builtin_list_processes",
            json!({ "filter": filter, "limit": 2_000 }),
        )
        .await;
        result.content["total"].as_u64().unwrap_or(0) as usize
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn timeout_reclaims_grandchildren_on_windows() {
        // `cmd /C ping` 是「直接子进程 + 孙进程」结构：只 start_kill 会把 ping 留成孤儿。
        let before = count_processes_matching("ping").await;

        let result = exec(
            "builtin_execute_command",
            json!({
                "command": "cmd",
                "args": ["/C", "ping -n 10 127.0.0.1"],
                "timeout_ms": 300
            }),
        )
        .await;
        assert_eq!(result.content["timed_out"], true, "{}", result.content);

        // taskkill /T 之后，cmd 拉起的 ping 也该消失。
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            count_processes_matching("ping").await <= before,
            "超时后不该留下孙进程（taskkill /F /T）"
        );
    }
}
