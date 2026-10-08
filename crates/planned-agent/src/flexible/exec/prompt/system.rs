//! 两个 system prompt 基准，以及单步 system prompt 的组装。
//!
//! 组装只做「尾部追加」：先追加环境段（同目录 `environment`），再追加技能规范段（同目录 `skills`），
//! 顺序固定、内容只取决于 `env + category` —— 同一次执行内每步拿到**完全相同**的字符串，
//! 从而整段成为可命中的 provider 前缀缓存。

use std::borrow::Cow;

use planned_agent_core::host::RuntimeEnvironment;

use super::category_rule_section;
use super::environment::render_environment_into;
use super::super::super::plan::category::PlanCategory;

/// 单步执行的 system prompt。
pub(crate) const STEP_SYSTEM_PROMPT: &str = "\
你是一个计划执行助手，本次只负责完成一个子目标。

规则：
- 需要外部数据或产生副作用时，调用提供的工具完成任务；不要凭空编造工具输出。
- 工具参数必须来自「本次子目标」「期望产出」或「前序步骤结果」，禁止臆造路径、URL、关键词。
- 给你的内容不是正文、而是**文件路径**时（会注明「未全文注入」——「前序步骤结果」段与工具返回都可能是这样），
  按你的需要**选用其中一个**读回工具即可，**两者不是必须配合的两步**：
  - 已经知道要读哪一段、或想直接看全文（包括「还没想好要找什么」）→ 用 `builtin_read_file_lines`
    （`offset` 从 0 开始，**显式传 `limit`**（如 2000）——不传 `limit` 会一次读到文件末尾，大文件可能撑爆上下文；
    续读时把 `offset` 加上上一次读到的行数）；
  - 只知道要找什么、不知道在第几行 → 用 `builtin_grep_file`（给出 1-based 行号与上下文；命中多时按输出末尾的
    `match_offset` 续读）；**若还想读它给的那几行**再多看上下文，才接着用 `builtin_read_file_lines`
    （`offset` = 行号 - 1）。
  **不要仅凭预览臆断**。
- 若已有信息足够，直接给出本次产出的结论作回答，不要再调用工具。
- 回答不要包 JSON 外壳，直接写产出内容本身。
";

/// 单步执行的 system prompt：`env` 与 `category` 都为 `None` 时**逐字**返回
/// [`STEP_SYSTEM_PROMPT`]（`Cow::Borrowed`，零分配）。
///
/// 有值时在基准提示词**尾部**依次追加两段（顺序固定）：
/// 1. 「运行环境」段（`env`）—— 陈述宿主事实，消灭「环境不确定 → 反复试探」这类弯路
///    （先跑 `uname` 再跑 `ver`、反复试 `python3` 与 `python`）；
/// 2. 「技能作业规范」段（`category`）—— 该计划所属技能（见 [`PlanCategory`]）的作业纪律。
///
/// 追加位置固定在尾部、内容只取决于 `env + category` —— 同一次执行内每步拿到**完全相同**的
/// 字符串，从而整段成为可命中的 provider 前缀缓存。
///
/// 两段都**只陈述事实 / 通用判据**；「怎么调用某个工具」属于工具契约（按需加载，不占常驻 prompt）。
/// 环境事实由 `core::host` 提供、作业规范由 [`category_rule_section`] 提供，本函数只负责组织成文本。
pub(crate) fn step_system_prompt(
    env: Option<&RuntimeEnvironment>,
    category: Option<PlanCategory>,
) -> Cow<'static, str> {
    // 无环境、无分类：逐字返回基准（零分配）。
    if env.is_none() && category.is_none() {
        return Cow::Borrowed(STEP_SYSTEM_PROMPT);
    }
    let mut text = String::with_capacity(STEP_SYSTEM_PROMPT.len() + 640);
    text.push_str(STEP_SYSTEM_PROMPT);
    if let Some(env) = env {
        render_environment_into(env, &mut text);
    }
    if let Some(section) = category.and_then(category_rule_section) {
        text.push('\n');
        text.push_str(section);
    }
    Cow::Owned(text)
}

/// 输出整理步的 system prompt。
///
/// 只在模板带 `output_schema`、且模板里的步骤全部成功时使用：把交付步的输出，
/// 按契约整理成「要交付的东西」。
///
/// **带 `builtin_read_file_lines` 与 `builtin_grep_file`**（`executor/mod.rs` 的 `resolve_tools`）：
/// 交付步的产出超过落盘阈值时，`prior` 里只给「文件路径 + 800 字符预览」，整理步得能把没内联的那份读回来
/// （读法与 `STEP_SYSTEM_PROMPT` 第 3 条同：按需二选一，不是必须两步）。
/// 除它们以外不带任何工具 —— 整理本身不需要外部数据源。
pub(crate) const OUTPUT_RESOLVE_SYSTEM_PROMPT: &str = "\
你是一个结果整理助手，本次只负责按给定的输出契约，把执行产出整理成要交付的东西。

规则：
- 只依据给出的执行产出整理，不得补充、推测或编造其中没有的数据。
- 契约要求的字段在执行产出里找不到时，如实在结果中注明「未获得」，绝不许编造内容。
- 不要复述执行过程与步骤编号，只交付结果本身。
- 契约要求 JSON / CSV 时，直接给出数据本身，不包解释文字、不加 Markdown 代码块标记。
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_prompt_without_env_borrows_verbatim() {
        let prompt = step_system_prompt(None, None);
        assert!(matches!(prompt, Cow::Borrowed(_)), "无环境时应零分配借用");
        assert_eq!(prompt.as_ref(), STEP_SYSTEM_PROMPT);
    }

    #[test]
    fn step_prompt_appends_environment_facts() {
        use planned_agent_core::host::ExecutableProbe;

        let env = RuntimeEnvironment {
            os: "windows".to_string(),
            arch: "x86_64".to_string(),
            path_separator: '\\',
            line_ending: "CRLF".to_string(),
            shell: Some("powershell".to_string()),
            console_encoding: None,
            executables: ExecutableProbe {
                available: vec![
                    ("node".to_string(), Some("v24.15.0".to_string())),
                    ("python".to_string(), None),
                ],
                missing: vec!["go".to_string()],
            },
            working_dir: Some("C:\\secret-dir".to_string()),
            notes: None,
            probed_at: Some("2026-01-01T00:00:00Z".to_string()),
        };
        let prompt = step_system_prompt(Some(&env), None);

        // 基准提示词原样保留在前（尾部追加，不动原有规则）
        assert!(prompt.starts_with(STEP_SYSTEM_PROMPT));
        // 事实
        assert!(prompt.contains("操作系统：windows (x86_64)"), "{prompt}");
        assert!(prompt.contains("执行命令的 shell：powershell"), "{prompt}");
        assert!(prompt.contains("node v24.15.0、python"), "{prompt}");
        // 可用项带「已探测」范围；缺失项不再单列（没列出来的自然没有）
        assert!(
            prompt.contains("已探测 node/python/python3/php/go/cargo/rustc"),
            "{prompt}"
        );
        assert!(!prompt.contains("不可用"), "不该再列缺失命令: {prompt}");
        // 编码风险：powershell 5.1 按控制台代码页写 stdout，中文会乱
        assert!(prompt.contains("命令输出按 UTF-8 解码"), "{prompt}");
        // 「怎么调用工具」不占常驻 prompt（属于工具 description）
        assert!(!prompt.contains("builtin_execute_command"), "{prompt}");
        // 隐私：工作目录、探测时刻不得外发
        assert!(!prompt.contains("secret-dir"), "working_dir 不应进 prompt");
        assert!(!prompt.contains("2026-01-01"), "probed_at 不应进 prompt");
    }

    #[test]
    fn step_prompt_is_identical_for_same_env() {
        // 前缀缓存的前提：同一次执行内每步拿到完全相同的字符串
        let env = RuntimeEnvironment {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            path_separator: '/',
            line_ending: "LF".to_string(),
            shell: None,
            console_encoding: None,
            executables: Default::default(),
            working_dir: None,
            notes: None,
            probed_at: None,
        };
        let first = step_system_prompt(Some(&env), None);
        assert_eq!(first, step_system_prompt(Some(&env), None));
        // shell 为 None：不写 shell 名，也不输出编码风险行（无从判断是哪家的编码行为）
        assert!(!first.contains("执行命令的 shell"), "{first}");
        assert!(!first.contains("命令输出按 UTF-8 解码"), "{first}");
        assert!(!first.contains("cmd"), "无 shell 时不该出现平台专属名: {first}");
        assert!(!first.contains("本机可用命令"), "无探测结果时省略该行");
    }

    #[test]
    fn linux_env_has_no_encoding_hint() {
        // 判据是 shell 而不是 os：Unix shell 默认写 UTF-8，不该出现任何编码提醒
        // （这也是「不要针对 Windows 硬编码」的回归锁 —— 提示跟着运行时事实走）
        let env = RuntimeEnvironment {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            path_separator: '/',
            line_ending: "LF".to_string(),
            shell: Some("bash".to_string()),
            console_encoding: None,
            executables: Default::default(),
            working_dir: None,
            notes: None,
            probed_at: None,
        };
        let prompt = step_system_prompt(Some(&env), None);
        assert!(prompt.contains("执行命令的 shell：bash"), "{prompt}");
        assert!(!prompt.contains("命令输出按 UTF-8 解码"), "{prompt}");
        assert!(!prompt.contains("chcp"), "{prompt}");
    }

    /// 无分类 ⇒ 不加规范段（行为不变）。
    #[test]
    fn step_prompt_without_category_has_no_rules() {
        let prompt = step_system_prompt(None, None);
        assert_eq!(prompt.as_ref(), STEP_SYSTEM_PROMPT);
    }

    /// 有分类 ⇒ 尾部追加该技能的规范段；`Other` 不追加。
    #[test]
    fn step_prompt_appends_category_rules() {
        let prompt = step_system_prompt(None, Some(PlanCategory::File));
        assert!(prompt.starts_with(STEP_SYSTEM_PROMPT));
        assert!(prompt.contains("## 计划类型：文件与数据处理"), "{prompt}");
        assert!(prompt.contains("读 / 改之前先看现状"), "{prompt}");

        let other = step_system_prompt(None, Some(PlanCategory::Other));
        assert_eq!(other.as_ref(), STEP_SYSTEM_PROMPT, "Other 不该加规范段");
    }

    /// 前缀缓存前提：同 `env + category` ⇒ 逐字一致；换分类才不同。
    #[test]
    fn step_prompt_is_identical_for_same_env_and_category() {
        let first = step_system_prompt(None, Some(PlanCategory::Browser));
        assert_eq!(step_system_prompt(None, Some(PlanCategory::Browser)), first);
        assert_ne!(step_system_prompt(None, Some(PlanCategory::Dev)), first);
    }
}
