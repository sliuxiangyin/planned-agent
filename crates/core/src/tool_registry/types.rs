use serde::{Deserialize, Serialize};

/// 工具来源类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ToolSource {
    /// MCP 服务器工具
    Mcp { server_name: String },
    /// 自定义工具
    Custom { handler_id: String },
    /// 内置工具
    Builtin,
    /// 子 agent 工具（注册的子 agent，执行时输出流经旁路通道转发）
    SubAgent { agent_id: String },
}

/// 工具分类（大分类）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ToolCategory {
    /// 浏览器相关：浏览器操作、HTTP请求、网页抓取
    Browser,
    /// 文件相关：文件读写、文件管理、目录操作
    File,
    /// 文本处理：文本处理、文本分析、文本转换
    Text,
    /// 数据处理：数据库、数据处理、数据分析
    Data,
    /// 系统操作：系统命令、进程管理、环境变量
    System,
    /// 设备操作：ADB设备、移动设备
    Device,
    /// 开发工具：Git、构建、测试
    Dev,
    /// 工具类：工具、自定义、内置、数据库查询读写
    Utility,
    /// 子 Agent：由 `register_sub_agent` 注册的子 agent 工具
    SubAgent,
}

impl ToolCategory {
    /// 获取所有工具分类
    pub fn all() -> Vec<ToolCategory> {
        vec![
            ToolCategory::Browser,
            ToolCategory::File,
            ToolCategory::Text,
            ToolCategory::Data,
            ToolCategory::System,
            ToolCategory::Device,
            ToolCategory::Dev,
            ToolCategory::Utility,
            ToolCategory::SubAgent,
        ]
    }

    /// 获取分类的中文描述
    pub fn description(&self) -> &str {
        match self {
            ToolCategory::Browser => "浏览器",
            ToolCategory::File => "文件",
            ToolCategory::Text => "文本",
            ToolCategory::Data => "数据",
            ToolCategory::System => "系统",
            ToolCategory::Device => "设备",
            ToolCategory::Dev => "开发",
            ToolCategory::Utility => "工具",
            ToolCategory::SubAgent => "子Agent",
        }
    }

    /// 稳定英文标识（与注册 / MCP 分类字符串一致），用作 `ChatConfig::allowed_tools` 的分类 token。
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolCategory::Browser => "Browser",
            ToolCategory::File => "File",
            ToolCategory::Text => "Text",
            ToolCategory::Data => "Data",
            ToolCategory::System => "System",
            ToolCategory::Device => "Device",
            ToolCategory::Dev => "Dev",
            ToolCategory::Utility => "Utility",
            ToolCategory::SubAgent => "SubAgent",
        }
    }

    /// 由英文标识解析分类；非法分类名返回 `None`。
    pub fn from_name(name: &str) -> Option<ToolCategory> {
        Some(match name {
            "Browser" => ToolCategory::Browser,
            "File" => ToolCategory::File,
            "Text" => ToolCategory::Text,
            "Data" => ToolCategory::Data,
            "System" => ToolCategory::System,
            "Device" => ToolCategory::Device,
            "Dev" => ToolCategory::Dev,
            "Utility" => ToolCategory::Utility,
            "SubAgent" => ToolCategory::SubAgent,
            _ => return None,
        })
    }
}
