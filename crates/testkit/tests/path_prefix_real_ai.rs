//! `content@<path>` / `file@<path>` 前缀的**真实 AI 行为探针**。
//!
//! 背景：`crates/planned-agent/src/flexible` 用这两个前缀区分「未全文注入的内容」与
//! 「落盘文件」，目的是**不让模型把文件当成未注入的正文去读回**。本文件在**真实模型**
//! 面前验证这个约定还成立。
//!
//! 三条纪律（与 `captcha_real_ai.rs` 一致）：
//! 1. **真实 AI 的探针一律 `#[ignore]`** —— 默认测试路径绝不触网、不要密钥。
//! 2. **只断言不变量**：两个真实探针都只**观察**（模型首次选择是概率性的，不判对错）；
//!    需要被保证的不变量是「端到端拿不到错误内容」—— 它由**工具侧**兜底，见
//!    `text_readers_reject_binary_file`（不触网）。
//! 3. **不依赖 `planned-agent`**（见 `docs/planned-agent/testkit.md` §3.1）：prompt 片段在此
//!    **手抄**，并用 `include_str!` 对 planned-agent 的源文件做「指纹」断言防漂移
//!    （该断言**不触网**，默认测试就会跑）。
//!
//! ## 实测结论（2026-10-10，MiniMax-M3）
//!
//! ⚠️ **单次采样不能当结论**。多轮（`PATH_PREFIX_RUNS`，默认 3）看到的是「概率性」：
//!
//! | 场景 | 观察 | 结论 |
//! |---|---|---|
//! | `content@` | 3/3 轮都用了读回工具 | 判据有效 |
//! | `file@`，只写「别当未注入的正文读回」 | 单次采样即误选 `builtin_read_text_file` 读 png | **不足以选对工具** |
//! | `file@`，再加「用与该文件类型相配的工具」 | 首次仍误选 **1/3 轮** | **只降低概率，不是保证** |
//!
//! 而且**误选不是错结果**：三个文本读回工具对二进制一律拒读（`binary_file` + 明确文案），
//! 端到端会被纠正（代价多一轮）——确定性在那里，不在 prompt。见下方 `text_readers_reject_binary_file`。
//!
//! 故两个真实探针都**只观察、不判对错**；本文件的镜像**逐条抄** `STEP_SYSTEM_PROMPT`（指纹兜漂移）。
//!
//! 跑真实探针：
//!
//! ```text
//! cargo test -p planned-agent-testkit --test path_prefix_real_ai -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use planned_agent_core::ai::traits::AiClient;
use planned_agent_core::ai::types::{
    ChatCompletionRequest, ChatCompletionResponse, FunctionDefinition, Message, MessageContent,
    MessageRole, ToolDefinition, ToolType,
};
use planned_agent_testkit::TestHarness;
use serde_json::json;

/// 文本读回工具 —— `file@` 场景里模型**不得**用它们去读那个文件。
const TEXT_READERS: &[&str] = &[
    "builtin_read_file_lines",
    "builtin_grep_file",
    "builtin_read_text_file",
];

/// 手抄**镜像**：来源 `crates/planned-agent/src/flexible/exec/prompt/system.rs` 的
/// `STEP_SYSTEM_PROMPT`，**逐条**抄（含「怎么读回」的细节）——
/// 探针必须贴近真实单步的 system，否则测的是我自己发明的规则。
/// 唯一省略的是与「运行环境」段耦合的「落盘产出必须写完整路径」那条（探针无环境段）。
/// 与源文件的漂移由 `copied_prefix_contract_matches_planned_agent_source` 兜住。
/// `file@` 那条的「用与该文件类型相配的工具」已**落入** planned-agent（实测驱动，见文件头），
/// 故这里是真正的逐条镜像。
const PREFIX_RULES: &str = "\
你是一个计划执行助手，本次只负责完成一个子目标。

规则：
- 需要外部数据或产生副作用时，调用提供的工具完成任务；不要凭空编造工具输出。
- 工具参数必须来自「本次子目标」「期望产出」或「前序步骤结果」，禁止臆造路径、URL、关键词。
- 给你的内容不是正文、而是**文件路径**时，看前缀区分：
  - `content@<path>` 是**未全文注入的内容**（「前序步骤结果」段与工具返回都可能是这样）——
    **只有带 `content@` 前缀的路径才是这种内容**。按你的需要**选用其中一个**读回工具即可，**两者不是必须配合的两步**：
    - 已经知道要读哪一段、或想直接看全文（包括「还没想好要找什么」）→ 用 `builtin_read_file_lines`
      （`offset` 从 0 开始，**显式传 `limit`**（如 2000）——不传 `limit` 会一次读到文件末尾，大文件可能撑爆上下文；
      续读时把 `offset` 加上上一次读到的行数）；
    - 只知道要找什么、不知道在第几行 → 用 `builtin_grep_file`（给出 1-based 行号与上下文；命中多时按输出末尾的
      `match_offset` 续读）；**若还想读它给的那几行**再多看上下文，才接着用 `builtin_read_file_lines`
      （`offset` = 行号 - 1）。
    **不要仅凭预览臆断**。
  - `file@<path>`：这是一个**文件**（落盘产出 / 工具产物），**不要**把它当成「未全文注入的正文」去读回；
    需要它的内容时，用与该文件**类型相配**的工具（图片 → 读图工具），而不是文本读回工具。
- 若已有信息足够，直接给出本次产出的结论作回答，不要再调用工具。
- 回答不要包 JSON 外壳，直接写产出内容本身。
";

// ── 防漂移指纹：编译期直接读 planned-agent 的源文件，那边改了标记这里就红。
const SYSTEM_SRC: &str = include_str!("../../planned-agent/src/flexible/exec/prompt/system.rs");
const SPILL_SRC: &str = include_str!("../../planned-agent/src/flexible/exec/spill.rs");
const IMAGE_SRC: &str = include_str!("../../planned-agent/src/flexible/exec/step/image.rs");

/// 手抄的约定必须仍与 planned-agent 的实现一致（**不触网**，默认测试跑）。
///
/// 这是「手抄文案」的代价控制：planned-agent 改了标记，这里立刻失败并提示同步，
/// 而不是让探针悄悄测一个已经过时的约定。
#[test]
fn copied_prefix_contract_matches_planned_agent_source() {
    assert!(
        SYSTEM_SRC.contains("content@<path>") && SYSTEM_SRC.contains("file@<path>"),
        "planned-agent 的 STEP_SYSTEM_PROMPT 不再声明这两个前缀 —— 同步本文件的手抄片段"
    );
    assert!(
        SPILL_SRC.contains("content@{}"),
        "spill.rs 不再产出 `content@` 前缀（落盘引用）"
    );
    assert!(
        IMAGE_SRC.contains("file@"),
        "image.rs 不再给图片说明加 `file@` 前缀"
    );
    // file@ 条的「按类型选相配工具」判据也必须在两边一致 —— 它才是实测有效的关键一句，
    // 只锁两个符号不足以发现「那句被删」。
    assert!(
        SYSTEM_SRC.contains("用与该文件") && PREFIX_RULES.contains("用与该文件"),
        "file@ 条的「按类型选相配工具」判据在手抄镜像与 planned-agent 之间不一致 —— 同步两边"
    );
    // 手抄片段自身也要含这两个符号，否则探针测的压根不是设想的约定
    assert!(
        PREFIX_RULES.contains("content@<path>") && PREFIX_RULES.contains("file@<path>"),
        "手抄片段缺前缀符号"
    );
}

/// 工具侧兜底（**不触网**）：文本读回工具遇到二进制/图片会**明确拒读**。
///
/// 这决定了 `file@` 探针那条 FAIL 的严重性：模型即使误选文本读回工具，也**拿不到乱码**，
/// 而是收到 `binary_file` 错误 + 明确文案，下一轮可改用读图工具（代价是多一轮，不是错结果）。
#[tokio::test]
async fn text_readers_reject_binary_file() {
    let harness = TestHarness::builder().build().expect("构造 harness");

    // 优先用真实 fixture 图；缺则写一个含 NUL 的占位（`looks_binary` 按「开头 8 KiB 含 NUL」判定）
    let target = match fixture("captcha.png") {
        Some(source) => harness.stage_file(&source).expect("把 fixture 搬进沙箱"),
        None => {
            let path = harness.sandbox_root().join("shot.png");
            let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
            bytes.extend_from_slice(&[0x00, 0x01, 0x02, 0x00]);
            std::fs::write(&path, bytes).expect("写占位文件");
            path
        }
    };
    let path = target.to_string_lossy().to_string();

    for tool in TEXT_READERS {
        // `builtin_grep_file` 的 `query` 必填；`builtin_read_file_lines` 的 `offset` 必填；
        // `builtin_read_text_file` 只要 `path`（契约见 `filesystem/contract.rs`）
        let arguments = match *tool {
            "builtin_grep_file" => json!({ "path": path, "query": "x" }),
            "builtin_read_file_lines" => json!({ "path": path, "offset": 0 }),
            _ => json!({ "path": path }),
        };
        let outcome = harness
            .call(tool, arguments)
            .await
            .unwrap_or_else(|error| panic!("{tool} 调用失败：{error:#}"));
        assert!(
            outcome.result.is_error,
            "{tool} 应当拒读二进制，而不是返回内容：{:?}",
            outcome.result
        );
        println!(
            "[{tool}] is_error={}：{}",
            outcome.result.is_error, outcome.result.content
        );
    }
}

/// 真实调用的默认重复轮数（模型有波动，单点看不准 —— 同 `captcha_real_ai.rs` 的做法）。
const DEFAULT_RUNS: usize = 3;

/// `PATH_PREFIX_RUNS` 指定的轮数（默认 [`DEFAULT_RUNS`]）。
fn runs() -> usize {
    std::env::var("PATH_PREFIX_RUNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count| *count > 0)
        .unwrap_or(DEFAULT_RUNS)
}

/// 取一张 fixture 图；不存在返回 `None`（探针不依赖素材）。
fn fixture(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    path.exists().then_some(path)
}

fn text_message(role: MessageRole, text: String) -> Message {
    Message {
        role,
        content: Some(MessageContent::Text { text }),
        ..Default::default()
    }
}

/// 从 harness 的真实注册表里按名取「LLM 侧工具定义」（等价于 flexible 的 `to_tool_definition`）。
fn tool_definitions(harness: &TestHarness, names: &[&str]) -> Vec<ToolDefinition> {
    harness
        .registry()
        .get_enabled_tools_with_categories()
        .into_iter()
        .filter(|(tool, _)| names.contains(&tool.name.as_str()))
        .map(|(tool, _)| ToolDefinition {
            r#type: ToolType::Function,
            function: FunctionDefinition {
                name: tool.name,
                description: Some(tool.description),
                parameters: Some(tool.input_schema),
                strict: None,
            },
        })
        .collect()
}

/// 发一次真实请求（`tools` 为空则不带工具）。
async fn ask(
    ai: &Arc<dyn AiClient>,
    messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
) -> ChatCompletionResponse {
    let mut request: ChatCompletionRequest = ai.default_config();
    request.messages = messages;
    request.tools = (!tools.is_empty()).then_some(tools);
    request.stream = false;
    ai.chat_completion(request)
        .await
        .expect("真实 AI 调用不应 Err（检查 crates/testkit/config.toml 与网络）")
}

/// 取首选的工具调用 `(工具名, 原始 arguments)`。
fn tool_calls_of(response: &ChatCompletionResponse) -> Vec<(String, String)> {
    response
        .choices
        .first()
        .and_then(|choice| choice.message.tool_calls.as_ref())
        .map(|calls| {
            calls
                .iter()
                .map(|call| (call.function.name.clone(), call.function.arguments.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn real_harness() -> (TestHarness, Arc<dyn AiClient>) {
    let harness = TestHarness::from_env().expect("构造真实 AI 测试台（见 config.toml.example）");
    let ai = harness.ai().expect("from_env 应已注入真实 AI");
    (harness, ai)
}

/// `content@<path>` 场景：只**观察**模型是否用读回工具把它读回来。
///
/// 不判对错 —— 真实模型可能直接回答、也可能先读回；两者都合理。真正被锁住的契约在
/// `copied_prefix_contract_matches_planned_agent_source`（不触网）。
#[tokio::test]
#[ignore = "需要真实 AI（crates/testkit/config.toml）；用 `-- --ignored --nocapture` 显式跑"]
async fn content_prefix_invites_read_back() {
    let (harness, ai) = real_harness();

    // `content@` 指向一个**真实落盘**的文本文件（未全文注入的替身）
    let file = harness.sandbox_root().join("step-1.txt");
    std::fs::write(&file, "第一行：内容\n第二行：更多内容\n".repeat(20)).expect("写测试文件");

    let prompt = format!(
        "### #E1\n\
         ⚠️ 工具输出较大（40 行 / 800 字节），已存为临时文件，未全文注入。\n\
         content@{}\n\
         ———— 开头预览（前 20 字符）————\n第一行：内容\n第二行：更多内容\n\n\
         本步意图：汇报这个文件里一共有几行。",
        file.display()
    );

    let tools = tool_definitions(&harness, TEXT_READERS);
    let runs = runs();
    for round in 1..=runs {
        let response = ask(
            &ai,
            vec![
                text_message(MessageRole::System, PREFIX_RULES.to_string()),
                text_message(MessageRole::User, prompt.clone()),
            ],
            tools.clone(),
        )
        .await;

        let calls = tool_calls_of(&response);
        let answer = response.choices.first().and_then(|choice| match &choice.message.content {
            Some(MessageContent::Text { text }) => Some(text.as_str()),
            _ => None,
        });
        // 只观察，不判对错：读回、或直接作答，都合理
        println!("[content@] 第 {round}/{runs} 轮 → 工具 {calls:?} / 回答 {answer:?}");
        assert!(!response.choices.is_empty(), "应当有 choices");
    }
}

/// 跑一轮 `file@` 场景，返回**误读回**的工具调用（空 = 这一轮正确）。
async fn observe_file_prefix(
    harness: &TestHarness,
    ai: &Arc<dyn AiClient>,
    tool_names: &[&str],
    intent: &str,
) -> Vec<(String, String)> {
    // `file@` 指向一个**非文本**落盘文件；优先用真实 fixture 图，缺则写一个 PNG 头占位
    let target = match fixture("captcha.png") {
        Some(source) => harness.stage_file(&source).expect("把 fixture 搬进沙箱"),
        None => {
            let path = harness.sandbox_root().join("shot.png");
            std::fs::write(&path, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
                .expect("写占位文件");
            path
        }
    };

    let prompt = format!(
        "### #E1\n\
         工具返回了 1 张图片，已保存为本地文件：\n\
         - file@{}（image/png，8 字节）\n\n\
         本步意图：{intent}",
        target.display()
    );

    let tools = tool_definitions(harness, tool_names);
    let response = ask(
        ai,
        vec![
            text_message(MessageRole::System, PREFIX_RULES.to_string()),
            text_message(MessageRole::User, prompt),
        ],
        tools,
    )
    .await;

    let calls = tool_calls_of(&response);
    println!("[file@] 模型选择了：{calls:?}");
    calls
        .into_iter()
        .filter(|(name, _)| TEXT_READERS.contains(&name.as_str()))
        .collect()
}

/// `file@` 场景的**观察**（不是门禁）：跑 `PATH_PREFIX_RUNS`（默认 3，可调大）轮，
/// 打印「模型首次就选对（没用文本读回工具）」的比例。
///
/// **不断言** —— 实测它是**概率性**的（3 轮里出现过 1 轮误选），而模型的**首次选择不是**
/// 需要保证的不变量：真正的不变量是「端到端拿不到错误内容」，那由**工具侧**兜底，见
/// `text_readers_reject_binary_file`。把首次选择写成红灯会把「已知的、已被兜住的概率」
/// 误报成故障。
#[tokio::test]
#[ignore = "需要真实 AI（crates/testkit/config.toml）；用 `-- --ignored --nocapture` 显式跑"]
async fn file_prefix_misread_rate() {
    let (harness, ai) = real_harness();
    let mut names = TEXT_READERS.to_vec();
    names.push("builtin_recognize_image");

    let runs = runs();
    let mut misread = 0usize;
    for round in 1..=runs {
        let offenders =
            observe_file_prefix(&harness, &ai, &names, "识别图中验证码，给出其中的字符。").await;
        if offenders.is_empty() {
            println!("[file@] 第 {round}/{runs} 轮 → 首次选对（没用文本读回工具）");
        } else {
            misread += 1;
            println!("[file@] 第 {round}/{runs} 轮 → 首次误选：{offenders:?}");
        }
    }
    println!(
        "[file@] 首次误选 {misread}/{runs} 轮 —— 误选**不会**导致错结果：\
         文本读回工具会以 binary_file 拒读（见 text_readers_reject_binary_file）"
    );
}
