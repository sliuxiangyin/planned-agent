//! 大文本落盘与「文件 + 预览」引用渲染。
//!
//! 两个消费者：
//! - **跨步产出**：`executor` 在拿到某步产出后判断是否落盘，下游步骤只看到引用
//!   （见 `docs/planned-agent/flexible-step-output-spill.md`）；
//! - **单步内的工具输出**：`step` 在执行工具后判断是否落盘，回灌给 LLM 的只留引用
//!   （见 `docs/planned-agent/flexible-step-tool-output-spill.md`）。
//!
//! 落盘**不省内存** —— 全文始终留在调用方手里；目的是不把全文塞进上下文。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

/// `run-*` 子目录的进程内序号：与毫秒时间戳一起保证目录唯一。
pub(in crate::flexible::exec) static RUN_SEQ: AtomicU64 = AtomicU64::new(0);

/// 本次执行的产出目录名（执行器不认识「会话」概念，会话段由宿主拼进 `cache_dir`）。
pub(in crate::flexible::exec) fn new_run_dir_name() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|delta| delta.as_millis())
        .unwrap_or(0);
    let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("run-{millis}-{seq}")
}

/// 落盘引用描述的是哪一类内容 —— 只影响文案措辞，模型据此区分「步骤产出」与「工具输出」。
#[derive(Debug, Clone, Copy)]
pub(in crate::flexible::exec) enum SpillKind {
    /// 前序步骤的产出
    StepOutput,
    /// 单步内的工具输出
    ToolOutput,
}

/// 一条前序产出的存放形态。
///
/// 全文**始终**留在内存里 —— 本方案的目的不是省内存，而是不把全文塞进下游 `prompt`；
/// 超阈值时另存一份文件，下游以「文件说明 + 预览」引用、用 `builtin_read_file_lines` 按需读取。
#[derive(Debug, Clone)]
pub(in crate::flexible::exec) struct StoredOutput {
    pub(in crate::flexible::exec) content: String,
    pub(in crate::flexible::exec) spilled: Option<SpilledOutput>,
}

/// 落盘信息（供 `prior` 渲染与 `StepRunRecord::output_file` 使用）。
#[derive(Debug, Clone)]
pub(in crate::flexible::exec) struct SpilledOutput {
    path: PathBuf,
    pub(in crate::flexible::exec) lines: usize,
    pub(in crate::flexible::exec) bytes: usize,
}

impl SpilledOutput {
    pub(in crate::flexible::exec) fn path_string(&self) -> String {
        self.path.display().to_string()
    }
}

/// 通用落盘：`content` 超过阈值就写到 `<cache_dir>/<run_dir>/<file_name>`。
///
/// - 返回 `Ok(None)` = 未超阈值（调用方照旧内联全文）；
/// - 返回 `Ok(Some(_))` = 已落盘；
/// - IO 失败**向上抛** —— 由调用方决定语义。两个调用方的后果不对称：
///   跨步产出落盘失败 → 下游拿不到数据，该步记 `Failed`；
///   步内工具输出落盘失败 → 只是多占上下文，告警并回退内联全文。
pub(in crate::flexible::exec) async fn spill_text(
    cache_dir: &Path,
    run_dir: &str,
    file_name: &str,
    threshold_chars: usize,
    content: &str,
) -> Result<Option<SpilledOutput>> {
    if content.chars().count() <= threshold_chars {
        return Ok(None);
    }
    let dir = cache_dir.join(run_dir);
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("创建产出缓存目录失败：{}", dir.display()))?;
    let path = dir.join(file_name);
    tokio::fs::write(&path, content)
        .await
        .with_context(|| format!("写入产出缓存文件失败：{}", path.display()))?;
    Ok(Some(SpilledOutput {
        path,
        lines: content.lines().count(),
        bytes: content.len(),
    }))
}

/// 跨步产出落盘。
///
/// 文件名用**步骤序号**而非 `result_reference` —— 后者来自模板 / LLM，
/// 直接拼进路径有目录穿越风险。
pub(in crate::flexible::exec) async fn spill_output(
    cache_dir: &Path,
    run_dir: &str,
    index: usize,
    threshold_chars: usize,
    output: &str,
) -> Result<Option<SpilledOutput>> {
    spill_text(
        cache_dir,
        run_dir,
        &format!("step-{index}.txt"),
        threshold_chars,
        output,
    )
    .await
}

/// 落盘内容渲染进上下文的文本：`{说明} + 文件路径 + 读回方式 + 开头预览`。
///
/// 预览不是截断兜底 —— 它是给模型判断「要不要读这个文件」的线索。
pub(in crate::flexible::exec) fn render_spill_reference(
    content: &str,
    spilled: &SpilledOutput,
    preview_chars: usize,
    kind: SpillKind,
) -> String {
    let what = match kind {
        SpillKind::StepOutput => "产出较大",
        SpillKind::ToolOutput => "工具输出较大",
    };
    let preview: String = content.chars().take(preview_chars).collect();
    format!(
        "⚠️ {what}（{} 行 / {} 字节），已存为临时文件，未全文注入。\n\
         文件：{}\n\
         读取方式：按需要选择其中一个即可（两者不是必须配合的两步）——\n\
         已知要读哪一段、或想直接看全文：`builtin_read_file_lines`（`offset` 从 0 开始；\
         **不传 `limit` 会一次读到文件末尾**，大文件请显式传 `limit`（如 2000）；本文件共 {} 行，\
         续读时把 `offset` 加上已读到的行数）；\n\
         只知道要找什么、不知道在第几行：`builtin_grep_file`（返回 1-based 行号；命中多时按 `match_offset` 续读），\
         若还要多看上下文再按行号读（`offset` = 行号 - 1）。\n\
         ———— 开头预览（前 {} 字符）————\n{}",
        spilled.lines,
        spilled.bytes,
        spilled.path_string(),
        spilled.lines,
        preview_chars,
        preview
    )
}

/// 一条前序产出渲染进 `prompt` 的文本：未落盘给全文，落盘给「文件说明 + 预览」。
pub(in crate::flexible::exec) fn render_prior_output(
    stored: &StoredOutput,
    preview_chars: usize,
) -> String {
    let Some(spilled) = &stored.spilled else {
        return stored.content.clone();
    };
    render_spill_reference(&stored.content, spilled, preview_chars, SpillKind::StepOutput)
}
