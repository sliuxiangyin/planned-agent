//! 设置页面类型定义

use planned_agent_core::tool_registry::ToolCategory;

/// 设置页面左侧导航 Tab
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsTab {
    /// 通用设置（暂未实现）
    General,
    /// 模型设置（暂未实现）
    Model,
    /// 工具管理
    ToolManagement,
    /// MCP 服务管理
    McpService,
}

impl SettingsTab {
    pub fn label(&self) -> &str {
        match self {
            SettingsTab::General => "通用",
            SettingsTab::Model => "模型",
            SettingsTab::ToolManagement => "工具管理",
            SettingsTab::McpService => "MCP 服务",
        }
    }

    pub fn icon(&self) -> &str {
        match self {
            SettingsTab::General => "⚙",
            SettingsTab::Model => "🤖",
            SettingsTab::ToolManagement => "🔧",
            SettingsTab::McpService => "🔌",
        }
    }

    pub fn enabled(&self) -> bool {
        matches!(self, SettingsTab::ToolManagement | SettingsTab::McpService)
    }

    pub fn all() -> Vec<SettingsTab> {
        vec![
            SettingsTab::General,
            SettingsTab::Model,
            SettingsTab::ToolManagement,
            SettingsTab::McpService,
        ]
    }
}

/// 工具来源筛选
#[derive(Debug, Clone, PartialEq)]
pub enum ToolSourceFilter {
    All,
    Mcp,
    Builtin,
    Custom,
}

impl ToolSourceFilter {
    pub fn label(&self) -> &str {
        match self {
            ToolSourceFilter::All => "全部",
            ToolSourceFilter::Mcp => "MCP",
            ToolSourceFilter::Builtin => "内置",
            ToolSourceFilter::Custom => "自定义",
        }
    }

    pub fn all() -> Vec<ToolSourceFilter> {
        vec![
            ToolSourceFilter::All,
            ToolSourceFilter::Mcp,
            ToolSourceFilter::Builtin,
            ToolSourceFilter::Custom,
        ]
    }
}

/// 工具分类筛选（带"全部"选项）
#[derive(Debug, Clone, PartialEq)]
pub enum CategoryFilter {
    All,
    Specific(ToolCategory),
}

impl CategoryFilter {
    pub fn label(&self) -> String {
        match self {
            CategoryFilter::All => "全部分类".to_string(),
            CategoryFilter::Specific(cat) => cat.description().to_string(),
        }
    }

    pub fn all_options() -> Vec<CategoryFilter> {
        let mut opts = vec![CategoryFilter::All];
        for cat in ToolCategory::all() {
            opts.push(CategoryFilter::Specific(cat));
        }
        opts
    }
}
