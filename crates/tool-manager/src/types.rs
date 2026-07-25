// 重新导出 core 中的类型
pub use planned_agent_core::tool_registry::{
    ToolSource, 
    ToolCategory, 
    ToolExecutor, 
    BuiltinToolProvider,
};

use serde::{Deserialize, Serialize};

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
