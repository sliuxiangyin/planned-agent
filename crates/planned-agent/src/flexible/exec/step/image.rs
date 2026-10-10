//! 工具结果里的图片：落盘成文件，只把路径交给模型。
//!
//! 依据 `docs/planned-agent/image-recognition-tool.md` §4.3。
//!
//! **契约**：base64 绝不进入 messages / 日志 / 事件 / 报告 —— 落盘后立即丢弃，
//! 进上下文的只有一句短路径提示。图片由**任何接受路径的看图工具**按需读取；回灌文案
//! 刻意不点名具体工具（没注册 / 改名时点名即幻觉源）。**执行器的主模型不需要视觉能力**。
//!
//! **职责**：本模块只做一件事 —— 把图片块落盘、返回说明。它**不**决定
//! 「要不要走这条路」（那是 `mod.rs` 回灌点的 `match`），也**不**渲染文本块
//! （那是 `render::tool_content` 与回灌点的活）。

use std::path::Path;

use base64::Engine as _;
use planned_agent_core::mcp::types::ContentBlock;

use super::super::spill;

/// 单张图片的字节上限，与 `builtin_recognize_image` / ai-openai 对齐
/// （`crates/ai-openai/src/client.rs:186`）。
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// 一次工具调用最多落盘几张图片。
///
/// 一个 MCP server 返回 N 张图就会写 N 个文件 —— 不设上限能把磁盘写满。
const MAX_IMAGES_PER_RESULT: usize = 8;

/// 一次工具调用落盘的图片**总**字节上限（单张上限之外的第二道闸）。
const MAX_TOTAL_BYTES_PER_RESULT: u64 = 64 * 1024 * 1024;

/// MIME → 落盘扩展名。
///
/// 只认 ai-openai `image_mime_of` 的白名单（`crates/ai-openai/src/client.rs:189`）：
/// 扩展名写错会让读盘侧推断失败，整个 `chat_completion` 直接 `Err`。
fn extension_for_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

/// 把结果里的图片块落盘，返回给模型看的说明清单（绝对路径 + 类型 + 字节数）。
///
/// **只认图片块**：非图片块 `continue` 掉，不看、不管 —— 文本渲染与「要不要走这条路」
/// 都由调用方（`mod.rs` 回灌点）负责。
///
/// 清单长度 = 实际处理的图片张数（含被上限 / 校验拒掉的，它们也各占一条说明），
/// 调用方用它拼提示。路径必须绝对：看图工具的沙箱根是宿主的 cache 产出区
/// （绝对路径），相对路径过不了校验。
///
/// 落盘失败**不**判该步失败（对比跨步产出落盘的严格语义）：工具已经执行完了，
/// 失败原因会写进说明，模型可以重新调用工具取一张新图。
pub(crate) async fn spill_images(
    blocks: &[ContentBlock<'_>],
    cache_dir: &Path,
    run_dir: &str,
    tag: &str,
) -> Vec<String> {
    let mut notes: Vec<String> = Vec::new();
    let mut nth = 0usize;
    let mut budget = MAX_TOTAL_BYTES_PER_RESULT;

    for block in blocks.iter().copied() {
        let ContentBlock::Image { mime_type, data } = block else {
            continue;
        };
        let ordinal = nth + 1;
        if nth >= MAX_IMAGES_PER_RESULT {
            notes.push(format!(
                "- 第 {ordinal} 张图片：超过单次 {MAX_IMAGES_PER_RESULT} 张上限，未保存"
            ));
        } else {
            let (note, written) =
                materialize(mime_type, data, cache_dir, run_dir, tag, nth, budget).await;
            budget = budget.saturating_sub(written);
            notes.push(note);
        }
        nth += 1;
    }

    notes
}

/// 单张图片落盘：`<cache_dir>/<run_dir>/img-<tag>-<nth>.<ext>`。
///
/// 返回 `(写进上下文的说明, 实际落盘字节数)`；字节数供外层扣预算。
async fn materialize(
    mime: &str,
    data: &str,
    cache_dir: &Path,
    run_dir: &str,
    tag: &str,
    nth: usize,
    budget: u64,
) -> (String, u64) {
    let ordinal = nth + 1;
    let Some(ext) = extension_for_mime(mime) else {
        return (
            format!(
                "- 第 {ordinal} 张图片：格式「{}」不支持，未保存",
                if mime.is_empty() { "未知" } else { mime }
            ),
            0,
        );
    };
    if data.is_empty() {
        return (format!("- 第 {ordinal} 张图片：缺少 base64 数据，未保存"), 0);
    }

    // 先按 base64 长度粗筛，避免把超大 payload 整份解码进内存后再被拒
    // （base64 是 3 字节原文 → 4 字符，+8 覆盖 padding 与换行）。
    let allowed_bytes = budget.min(MAX_IMAGE_BYTES);
    if data.len() as u64 > allowed_bytes / 3 * 4 + 8 {
        return (
            format!(
                "- 第 {ordinal} 张图片：{} 字符超过本次可落盘额度（单张上限 {MAX_IMAGE_BYTES} 字节），未保存",
                data.len()
            ),
            0,
        );
    }

    // base64 只在这里出现一次：解码后随本作用域丢弃，绝不外传。
    let bytes = match base64::engine::general_purpose::STANDARD.decode(data) {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                format!("- 第 {ordinal} 张图片：base64 解码失败（{error}），未保存"),
                0,
            )
        }
    };
    if bytes.len() as u64 > allowed_bytes {
        return (
            format!(
                "- 第 {ordinal} 张图片：{} 字节超过本次可落盘额度（单张上限 {MAX_IMAGE_BYTES} 字节），未保存",
                bytes.len()
            ),
            0,
        );
    }

    // 文件名只含步骤 / 轮次 / 序号，不含 LLM 给的任何字符串 → 无目录穿越风险。
    let file_name = format!("img-{tag}-{nth}.{ext}");
    match spill::spill_bytes(cache_dir, run_dir, &file_name, &bytes).await {
        Ok(path) => {
            let absolute = std::path::absolute(&path).unwrap_or(path);
            (
                format!("- file@{}（{mime}，{} 字节）", absolute.display(), bytes.len()),
                bytes.len() as u64,
            )
        }
        Err(error) => (
            format!("- 第 {ordinal} 张图片：保存失败（{error:#}），未保存"),
            0,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use planned_agent_core::mcp::types::{parse_content_blocks, ContentBlock};
    use serde_json::{json, Value};

    /// 复刻 `mod.rs` 回灌点的解析步骤：把测试数据变成视图。
    fn view(content: &Value) -> Vec<ContentBlock<'_>> {
        parse_content_blocks(content).expect("测试数据应当是内容块序列")
    }

    const PNG_BASE64: &str = "iVBORw0KGgo=";

    fn png_block() -> Value {
        json!({ "type": "image", "mime_type": "image/png", "data": PNG_BASE64 })
    }

    #[tokio::test]
    async fn images_are_spilled_and_returned_as_absolute_paths() {
        let cache = tempfile::tempdir().expect("临时目录");
        let content = json!([{ "type": "text", "text": "shot taken" }, png_block()]);

        let notes = spill_images(&view(&content), cache.path(), "run-1", "s1-r2-c0").await;

        assert_eq!(notes.len(), 1, "{notes:?}");
        let note = &notes[0];
        assert!(!note.contains(PNG_BASE64), "说明里不得出现 base64：{note}");
        assert!(note.contains("image/png"), "{note}");
        assert!(note.starts_with("- file@"), "说明须用 file@ 前缀标出文件：{note}");

        let spilled = cache.path().join("run-1").join("img-s1-r2-c0-0.png");
        assert!(
            spilled.exists(),
            "图片应落盘到 {}；实际说明：{notes:?}",
            spilled.display()
        );
        assert_eq!(
            std::fs::read(&spilled).expect("读回"),
            [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
        );

        let expected = std::path::absolute(&spilled).expect("绝对化");
        assert!(
            note.contains(&expected.display().to_string()),
            "路径必须是绝对路径：{note}"
        );
    }

    #[tokio::test]
    async fn multiple_images_get_distinct_files() {
        let cache = tempfile::tempdir().expect("临时目录");
        let content = json!([png_block(), png_block()]);

        let notes = spill_images(&view(&content), cache.path(), "run-2", "s2-r0-c0").await;

        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            cache.path().join("run-2").join("img-s2-r0-c0-0.png").exists(),
            "实际说明：{notes:?}"
        );
        assert!(
            cache.path().join("run-2").join("img-s2-r0-c0-1.png").exists(),
            "实际说明：{notes:?}"
        );
    }

    #[tokio::test]
    async fn unsupported_mime_is_reported_without_spilling() {
        let cache = tempfile::tempdir().expect("临时目录");
        let content = json!([{ "type": "image", "mime_type": "image/bmp", "data": PNG_BASE64 }]);

        let notes = spill_images(&view(&content), cache.path(), "run-3", "s3-r0-0").await;

        assert_eq!(notes.len(), 1, "被拒的图也要占一条说明：{notes:?}");
        assert!(notes[0].contains("不支持"), "{notes:?}");
        assert!(!cache.path().join("run-3").exists(), "不应创建目录");
    }

    #[tokio::test]
    async fn broken_base64_is_reported_without_panic() {
        let cache = tempfile::tempdir().expect("临时目录");
        let content =
            json!([{ "type": "image", "mime_type": "image/png", "data": "!!!not-base64!!!" }]);

        let notes = spill_images(&view(&content), cache.path(), "run-4", "s4-r0-0").await;

        assert!(notes[0].contains("解码失败"), "{notes:?}");
    }

    #[tokio::test]
    async fn non_image_blocks_are_ignored() {
        let cache = tempfile::tempdir().expect("临时目录");
        // 文本块与认不出的块都不该让本模块做任何事 —— 它们归渲染方管。
        let content = json!(["plain text", { "type": "something-new", "x": 1 }]);

        let notes = spill_images(&view(&content), cache.path(), "run-5", "s5-r0-c0").await;

        assert!(notes.is_empty(), "{notes:?}");
        assert!(!cache.path().join("run-5").exists(), "不应创建目录");
    }

    #[tokio::test]
    async fn image_count_is_capped() {
        let cache = tempfile::tempdir().expect("临时目录");
        // 多给一张，验证超出的那张既不落盘也不 panic。
        let content = Value::Array((0..=MAX_IMAGES_PER_RESULT).map(|_| png_block()).collect());

        let notes = spill_images(&view(&content), cache.path(), "run-6", "s6-r0-c0").await;

        assert_eq!(notes.len(), MAX_IMAGES_PER_RESULT + 1, "{notes:?}");
        let dir = cache.path().join("run-6");
        assert!(dir.join("img-s6-r0-c0-0.png").exists());
        assert!(
            !dir.join(format!("img-s6-r0-c0-{MAX_IMAGES_PER_RESULT}.png"))
                .exists(),
            "第 {MAX_IMAGES_PER_RESULT} 张之后不应落盘"
        );
        assert!(notes.last().expect("末条").contains("超过单次"), "{notes:?}");
    }

    #[tokio::test]
    async fn oversized_base64_is_rejected_before_decoding() {
        let cache = tempfile::tempdir().expect("临时目录");
        // 比单张上限还大的 base64（不真解码，靠长度预筛拦下）。
        let huge = "A".repeat((MAX_IMAGE_BYTES / 3 * 4 + 16) as usize);
        let content = json!([{ "type": "image", "mime_type": "image/png", "data": huge }]);

        let notes = spill_images(&view(&content), cache.path(), "run-7", "s7-r0-c0").await;

        assert!(notes[0].contains("超过本次可落盘额度"), "{notes:?}");
        assert!(!cache.path().join("run-7").exists(), "不应创建目录");
    }

    #[test]
    fn extension_mapping_covers_ai_openai_whitelist() {
        assert_eq!(extension_for_mime("image/png"), Some("png"));
        assert_eq!(extension_for_mime("image/jpeg"), Some("jpg"));
        assert_eq!(extension_for_mime("image/webp"), Some("webp"));
        assert_eq!(extension_for_mime("image/gif"), Some("gif"));
        assert_eq!(extension_for_mime("image/bmp"), None);
    }
}
