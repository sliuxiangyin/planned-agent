//! 执行器的内置提示词常量。
//!
//! 刻意不依赖 `prompt-manager` 的文件目录：宿主无需为执行器加载任何 prompt 文件。

use super::output_schema::{OutputKind, OutputSchema};

/// 单步执行的 system prompt。
pub(crate) const STEP_SYSTEM_PROMPT: &str = "\
你是一个计划执行助手，本次只负责完成一个子目标。

规则：
- 需要外部数据或产生副作用时，调用提供的工具完成任务；不要凭空编造工具输出。
- 工具参数必须来自「本次子目标」「期望产出」或「前序步骤结果」，禁止臆造路径、URL、关键词。
- 若已有信息足够，直接给出本次产出的结论作回答，不要再调用工具。
- 回答不要包 JSON 外壳，直接写产出内容本身。
";

/// 输出整理步的 system prompt。
///
/// 只在模板带 `output_schema`、且模板里的步骤全部成功时使用：把交付步的输出，
/// 按契约整理成「要交付的东西」。不带工具 —— 它只做分析，不需要外部数据。
pub(crate) const OUTPUT_RESOLVE_SYSTEM_PROMPT: &str = "\
你是一个结果整理助手，本次只负责按给定的输出契约，把执行产出整理成要交付的东西。

规则：
- 只依据给出的执行产出整理，不得补充、推测或编造其中没有的数据。
- 契约要求的字段在执行产出里找不到时，如实在结果中注明「未获得」，绝不许编造内容。
- 不要复述执行过程与步骤编号，只交付结果本身。
- 契约要求 JSON / CSV 时，直接给出数据本身，不包解释文字、不加 Markdown 代码块标记。
";

/// 组装单步任务的 user 文本。
///
/// `prior` 是依赖项（`#En`）的实际输出文本，按依赖顺序排列；无依赖时为空。
pub(crate) fn build_step_task(
    intent: &str,
    expected_output: &str,
    prior: &[(String, String)],
) -> String {
    let mut text = String::new();
    text.push_str("## 本次子目标\n");
    text.push_str(intent);
    text.push_str("\n\n## 期望产出（满足什么算完成）\n");
    text.push_str(expected_output);

    if !prior.is_empty() {
        text.push_str("\n\n## 前序步骤结果\n");
        for (reference, output) in prior {
            text.push_str("### ");
            text.push_str(reference);
            text.push('\n');
            text.push_str(output);
            text.push('\n');
        }
    }
    text
}

/// 把输出契约写成给 LLM 的说明文本（`${name}` 原样保留，由调用方渲染成实际值）。
pub(crate) fn build_output_contract_text(schema: &OutputSchema) -> String {
    let mut text = String::new();
    text.push_str("结果形态：");
    text.push_str(kind_label(schema.kind));
    text.push('\n');
    if let Some(goal) = &schema.goal {
        text.push_str("要交付什么：");
        text.push_str(goal);
        text.push('\n');
    }
    if let Some(success) = &schema.success {
        text.push_str("什么算成功：");
        text.push_str(success);
        text.push('\n');
    }
    if let Some(format) = &schema.format {
        text.push_str("形态要求：");
        text.push_str(format);
        text.push('\n');
    }
    if !schema.required.is_empty() {
        text.push_str("必须包含的字段（缺一即视为未达成）：");
        text.push_str(&schema.required.join(", "));
        text.push('\n');
    }
    if !schema.wanted.is_empty() {
        text.push_str("尽力去找的字段（找不到不算错，但要在结果里说明未获得）：");
        text.push_str(&schema.wanted.join(", "));
        text.push('\n');
    }
    text.push_str(match schema.kind {
        OutputKind::Bool => "交付要求：只给一句判断（成功 / 失败）与理由，不要其它内容。",
        OutputKind::Text => "交付要求：只给结果正文，不要复述执行过程。",
        OutputKind::Markdown => "交付要求：只给 Markdown 正文（可直接用标题与列表）。",
        OutputKind::Json => "交付要求：只给一个 JSON 对象，不加解释文字、不加代码块标记。",
        OutputKind::Csv => "交付要求：只给 CSV 文本（首行表头），不加解释文字、不加代码块标记。",
        OutputKind::File => "交付要求：给出落盘文件的完整路径与一句内容说明。",
    });
    text
}

/// 结果形态的中文名（给 LLM 看）。
fn kind_label(kind: OutputKind) -> &'static str {
    match kind {
        OutputKind::Bool => "只要成功 / 失败判断（无交付内容）",
        OutputKind::Text => "一段文本",
        OutputKind::Markdown => "一份 Markdown 文档",
        OutputKind::Json => "一个 JSON 对象",
        OutputKind::Csv => "一份 CSV 表格",
        OutputKind::File => "一个落盘文件",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn task_without_prior_omits_section() {
        let task = build_step_task("读取文件", "得到内容", &[]);
        assert!(task.contains("## 本次子目标\n读取文件"));
        assert!(task.contains("## 期望产出（满足什么算完成）\n得到内容"));
        assert!(!task.contains("前序步骤结果"), "无依赖时不应出现前序段");
    }

    #[test]
    fn task_with_prior_lists_references() {
        let prior = vec![
            ("#E1".to_string(), "索引=7".to_string()),
            ("#E2".to_string(), "已写入".to_string()),
        ];
        let task = build_step_task("基于 #E1 写入", "新增一行", &prior);
        assert!(task.contains("## 前序步骤结果"));
        assert!(task.contains("### #E1\n索引=7"));
        assert!(task.contains("### #E2\n已写入"));
    }

    #[test]
    fn contract_text_covers_bool_and_structured() {
        let bool_schema = OutputSchema::parse(&json!({
            "kind": "bool",
            "goal": "向 ${file_path} 追加一行",
            "success": "文件末尾新增一行即视为成功"
        }))
        .unwrap();
        let text = build_output_contract_text(&bool_schema);
        assert!(text.contains("什么算成功：文件末尾新增一行即视为成功"));
        assert!(text.contains("只给一句判断"), "bool 型要有专门的交付要求: {text}");
        assert!(!text.contains("必须包含的字段"), "bool 不谈字段: {text}");
        // 占位符原样保留，等调用方渲染
        assert!(text.contains("向 ${file_path} 追加一行"));
    }

    #[test]
    fn contract_text_lists_field_requirements() {
        let schema = OutputSchema::parse(&json!({
            "kind": "csv",
            "goal": "导出 ${dir} 下的商品清单",
            "format": "UTF-8 CSV，首行表头",
            "required": ["title", "price"],
            "wanted": ["stock"]
        }))
        .unwrap();
        let text = build_output_contract_text(&schema);
        assert!(text.contains("一份 CSV 表格"));
        assert!(text.contains("必须包含的字段（缺一即视为未达成）：title, price"));
        assert!(text.contains("尽力去找的字段"));
        assert!(text.contains("stock"));
        assert!(text.contains("UTF-8 CSV，首行表头"));
    }
}
