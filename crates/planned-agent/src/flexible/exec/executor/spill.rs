//! 步骤产出落盘与预览（见 docs/planned-agent/flexible-step-output-spill.md）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result}; 

/// `run-*` 子目录的进程内序号：与毫秒时间戳一起保证目录唯一。
pub(super) static RUN_SEQ: AtomicU64 = AtomicU64::new(0);

/// 本次执行的产出目录名（执行器不认识「会话」概念，会话段由宿主拼进 `cache_dir`）。
pub(super) fn new_run_dir_name() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|delta| delta.as_millis())
        .unwrap_or(0);
    let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("run-{millis}-{seq}")
}

/// 一条前序产出的存放形态。
///
/// 全文**始终**留在内存里 —— 本方案的目的不是省内存，而是不把全文塞进下游 `prompt`；
/// 超阈值时另存一份文件，下游以「文件说明 + 预览」引用、用 `builtin_read_file` 按需读取。
#[derive(Debug, Clone)]
pub(super) struct StoredOutput {
    pub(super) content: String,
    pub(super) spilled: Option<SpilledOutput>,
}

/// 落盘信息（供 `prior` 渲染与 `StepRunRecord::output_file` 使用）。
#[derive(Debug, Clone)]
pub(super) struct SpilledOutput {
    path: PathBuf,
    pub(super) lines: usize,
    pub(super) bytes: usize,
}

impl SpilledOutput {
    pub(super) fn path_string(&self) -> String {
        self.path.display().to_string()
    }
}

/// 产出超过阈值就写到 `<cache_dir>/<run_dir>/step-<index>.txt`。
///
/// - 返回 `Ok(None)` = 未超阈值（下游照旧内联全文）；
/// - 返回 `Ok(Some(_))` = 已落盘；
/// - IO 失败**向上抛**：由调用方把该步记为 `Failed`（不静默降级，见设计稿 §7）。
///
/// 文件名用**步骤序号**而非 `result_reference` —— 后者来自模板 / LLM，
/// 直接拼进路径有目录穿越风险。
pub(super) async fn spill_output(
    cache_dir: &Path,
    run_dir: &str,
    index: usize,
    threshold_chars: usize,
    output: &str,
) -> Result<Option<SpilledOutput>> {
    if output.chars().count() <= threshold_chars {
        return Ok(None);
    }
    let dir = cache_dir.join(run_dir);
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("创建产出缓存目录失败：{}", dir.display()))?;
    let path = dir.join(format!("step-{index}.txt"));
    tokio::fs::write(&path, output)
        .await
        .with_context(|| format!("写入产出缓存文件失败：{}", path.display()))?;
    Ok(Some(SpilledOutput {
        path,
        lines: output.lines().count(),
        bytes: output.len(),
    }))
}

/// 一条前序产出渲染进 `prompt` 的文本：未落盘给全文，落盘给「文件说明 + 预览」。
///
/// 预览不是截断兜底 —— 它是给模型判断「要不要读这个文件」的线索。
pub(super) fn render_prior_output(stored: &StoredOutput, preview_chars: usize) -> String {
    let Some(spilled) = &stored.spilled else {
        return stored.content.clone();
    };
    let preview: String = stored.content.chars().take(preview_chars).collect();
    format!(
        "⚠️ 产出较大（{} 行 / {} 字节），已存为临时文件，未全文注入。\n\
         文件：{}\n\
         读取方式：`builtin_read_file`（offset 从 1 开始，limit 默认 2000；\
         返回含 `next_offset` / `has_more`，可续读）。\n\
         ———— 开头预览（前 {} 字符）————\n{}",
        spilled.lines,
        spilled.bytes,
        spilled.path_string(),
        preview_chars,
        preview
    )
}
