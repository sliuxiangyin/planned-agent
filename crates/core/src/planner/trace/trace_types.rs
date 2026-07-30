//! 执行轨迹记忆系统 - 核心数据结构（Phase 1）
//!
//! ExecutionTrace = 一个步骤的成功执行模板（LLM 泛化后，具体值已替换为 {{变量}}）
//! 序列化为 JSON 文件存储于 traces/ 目录。

use serde::{Deserialize, Serialize};

/// 执行轨迹（泛化后的可复用模板）
///
/// 通过 LLM 将原始工具调用中的具体实例值（人名、地名、URL 等）
/// 替换为 {{变量}} 占位符，生成可复用的操作模板。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTrace {
    /// 轨迹唯一标识，格式: {date}-{seq}，如 "2026-07-30-001"
    pub id: String,

    /// 原始意图（保留用于调试回溯）
    /// 例如: "在百度搜索安仁乡"
    pub original_intent: String,

    /// 泛化后的意图（{{变量}} 替换具体值）
    /// 例如: "在搜索引擎搜索{{搜索关键词}}"
    pub generalized_intent: String,

    /// 前序步骤意图（用于后续 Phase 2 检索锚定）
    pub upstream_intent: Option<String>,

    /// 泛化后的工具调用序列
    pub actions: Vec<GeneralizedAction>,

    // ── 质量标记 ──
    /// 总迭代次数（越小 = 质量越高；超过 max_iterations_for_record 不入库）
    pub total_iterations: usize,
    /// 总执行耗时（毫秒）
    pub total_duration_ms: u64,
    /// 记录时间（ISO 8601）
    pub recorded_at: String,
}

/// 泛化后的单个工具调用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralizedAction {
    /// 工具名称（如 "browser_type"、"browser_click"）
    pub tool_name: String,

    /// 泛化参数（具体值 → {{变量}}）
    /// 例: {"selector": "#kw", "text": "{{搜索关键词}}"}
    pub params: serde_json::Value,

    /// 原始参数（保留用于调试，不被泛化覆盖）
    pub original_params: serde_json::Value,

    /// 步骤说明（注入 Prompt 时展示给 LLM）
    pub description: String,

    /// 数据流转推理（LLM 泛化时自动推断）
    /// 例: "从 snapshot 中找到搜索框 #kw，填入关键词；输出确认后点击 #su"
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning_hint: String,
}
