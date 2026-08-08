// 重新导出 core 中的类型
pub use planned_agent_core::tool_registry::{
    ToolSource,
    ToolCategory,
    ToolExecutor,
    BuiltinToolProvider,
};

use serde::{Deserialize, Serialize};
use planned_agent_core::mcp::types::ToolResult;

/// 工具调用结果（带分类）。
///
/// 由 `ToolRegistry::call_tool` 返回，
/// 既包含执行器产出的 `ToolResult`，
/// 也包含工具注册时已知的分类列表，
/// 便于上层（如 ReAct Agent）在不再次查询注册表的情况下做后处理路由。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub result: ToolResult,
    pub categories: Vec<ToolCategory>,
}

impl ToolOutcome {
    pub fn new(result: ToolResult, categories: Vec<ToolCategory>) -> Self {
        Self { result, categories }
    }
}

/// 工具元数据（扩展信息）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMetadata {
    /// 工具来源
    pub source: ToolSource,
    /// 工具分类（可多选）
    pub categories: Vec<ToolCategory>,
    /// 是否启用
    pub enabled: bool,
    /// 优先级（数值越小优先级越高，1-100）
    pub priority: u32,
    /// 标签（用于搜索）
    pub tags: Vec<String>,
    /// 创建时间
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 工具版本（可选）
    pub version: Option<String>,
}

/// 工具注册表统计信息
#[derive(Debug, Clone)]
pub struct ToolRegistryStats {
    pub total: usize,
    pub enabled: usize,
    pub disabled: usize,
    pub mcp_count: usize,
    pub custom_count: usize,
    pub builtin_count: usize,
}
