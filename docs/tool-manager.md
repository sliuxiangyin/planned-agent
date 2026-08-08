# 工具管理器 (`crates/tool-manager`) 详细设计

## 概述

`planned-agent-tool-manager` 是一个独立的工具管理 crate，提供统一的工具注册、管理和调用接口。它将 MCP 工具、自定义工具和内置工具整合到一个系统中，支持工具分类、参数验证和自动路由。

## 核心特性

1. **统一管理**：所有工具通过 ToolRegistry 统一管理
2. **自动路由**：工具调用时自动路由到正确的执行器
3. **分类支持**：支持 24 种工具分类，便于按类别筛选
4. **参数验证**：调用工具前自动验证参数
5. **线程安全**：使用 RwLock 确保多线程环境下的安全性
6. **易于扩展**：支持自定义工具和内置工具
7. **自动推断**：支持从工具名称和描述自动推断分类

## 目录结构

```
crates/tool-manager/
├── Cargo.toml
└── src/
    ├── lib.rs           # 模块入口
    ├── types.rs         # 核心类型定义
    ├── executor.rs      # ToolExecutor trait
    ├── registry.rs      # ToolRegistry 核心实现
    ├── custom_tool.rs   # CustomTool trait
    ├── mcp_adapter.rs   # MCP 适配器
    ├── validator.rs     # 参数验证器
    └── builtin/
        ├── mod.rs       # 内置工具模块
        ├── file_tools.rs # 文件工具
        └── text_tools.rs # 文本工具
```

## 核心类型

### ToolSource

工具来源枚举，用于区分工具的来源：

```rust
pub enum ToolSource {
    Mcp { server_name: String },  // MCP 服务器工具
    Custom { handler_id: String }, // 自定义工具
    Builtin,                        // 内置工具
}
```

### ToolCategory

工具分类枚举，支持 24 种分类：

```rust
pub enum ToolCategory {
    // 网络相关
    Browser,        // 浏览器操作
    WebRequest,     // HTTP请求
    WebScraping,    // 网页抓取
    
    // 文件系统
    FileRead,       // 文件读取
    FileWrite,      // 文件写入
    FileManage,     // 文件管理（复制、移动、删除）
    Directory,      // 目录操作
    
    // 文本处理
    TextProcess,    // 文本处理
    TextAnalysis,   // 文本分析
    TextTransform,  // 文本转换
    
    // 数据处理
    Database,       // 数据库操作
    DataProcess,    // 数据处理
    DataAnalysis,   // 数据分析
    
    // 系统操作
    SystemCommand,  // 系统命令
    ProcessManage,  // 进程管理
    Environment,    // 环境变量
    
    // 设备操作
    AdbDevice,      // ADB设备操作
    MobileDevice,   // 移动设备操作
    
    // 开发工具
    Git,            // Git操作
    Build,          // 构建工具
    Test,           // 测试工具
    
    // 其他
    Utility,        // 工具类
    Custom,         // 自定义
    Builtin,        // 内置工具
}
```

#### 自动推断分类

`ToolCategory::infer_from_tool` 方法可以根据工具名称和描述自动推断分类：

```rust
impl ToolCategory {
    pub fn infer_from_tool(tool: &Tool) -> Vec<ToolCategory> {
        let mut categories = Vec::new();
        let name_lower = tool.name.to_lowercase();
        let desc_lower = tool.description.to_lowercase();
        
        // 文件相关
        if name_lower.contains("file") || name_lower.contains("read") || desc_lower.contains("文件") {
            categories.push(ToolCategory::FileRead);
        }
        // ... 其他规则
        
        if categories.is_empty() {
            categories.push(ToolCategory::Utility);
        }
        
        categories
    }
}
```

### ToolMetadata

工具元数据，包含工具的扩展信息：

```rust
pub struct ToolMetadata {
    pub source: ToolSource,
    pub categories: Vec<ToolCategory>,
    pub enabled: bool,
    pub priority: u32,  // 1-100，数值越小优先级越高
    pub tags: Vec<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub version: Option<String>,
}
```

## 核心组件

### ToolExecutor

工具执行器 trait，所有自定义工具和内置工具都需要实现此 trait：

```rust
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult>;
    fn name(&self) -> &str;
    fn supported_tools(&self) -> Vec<String>;
    fn supports_tool(&self, tool_name: &str) -> bool { ... }
}
```

### CustomTool

自定义工具 trait，用户实现此 trait 来创建自定义工具：

```rust
#[async_trait]
pub trait CustomTool: Send + Sync {
    fn tool_definition(&self) -> Tool;
    fn categories(&self) -> Vec<ToolCategory>;
    async fn execute(&self, arguments: Value) -> Result<ToolResult>;
}
```

### McpManagerTrait

> **位置变更（依赖反转）**：`McpManagerTrait` 已下沉到 `planned-agent-core::tool_registry::traits::McpManagerTrait`，
> `tool-manager` 仅做 `pub use` 重新导出。这样 `mcp-rmcp` 不再反向依赖 `tool-manager`，
> 三个 crate 的依赖方向是 `core ← tool-manager` 和 `core ← mcp-rmcp`，互不交叉。

MCP 管理器 trait，用于解耦 `ToolRegistry` 与具体 MCP 实现（`mcp-rmcp`）：

```rust
// 定义在 core（planned-agent-core/src/tool_registry/traits.rs）
#[async_trait]
pub trait McpManagerTrait: Send + Sync {
    async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<ToolResult>;
    fn get_all_tools(&self) -> Vec<Tool>;
    fn find_server_for_tool(&self, tool_name: &str) -> Option<String>;
    fn get_server_names(&self) -> Vec<String>;
    fn get_server_categories(&self, server_name: &str) -> Option<Vec<String>>;
}
```

### ToolRegistry

统一工具注册表，提供完整的工具管理功能：

```rust
pub struct ToolRegistry { ... }

impl ToolRegistry {
    // 注册方法
    pub fn set_mcp_manager(&self, manager: Arc<dyn McpManagerTrait>);
    pub fn register_tool(&self, tool: Tool, metadata: ToolMetadata);
    pub fn register_custom_tool(&self, tool: Tool, categories: Vec<ToolCategory>, executor: Arc<dyn ToolExecutor>);
    pub fn register_builtin_tool(&self, tool: Tool, categories: Vec<ToolCategory>, executor: Arc<dyn ToolExecutor>);
    pub fn register_builtin_provider(&self, provider: &dyn BuiltinToolProvider);
    
    // 卸载方法
    pub fn unregister_tool(&self, name: &str) -> Result<()>;
    pub fn unregister_mcp_server_tools(&self, server_name: &str) -> Result<usize>;
    
    // 查询方法
    pub fn get_all_tools(&self) -> Vec<Tool>;
    pub fn get_tools_by_category(&self, category: &ToolCategory) -> Vec<Tool>;
    pub fn get_tools_by_categories(&self, categories: &[ToolCategory]) -> Vec<Tool>;
    pub fn get_tools_by_source(&self, source_type: &str) -> Vec<Tool>;
    pub fn get_tool(&self, name: &str) -> Option<Tool>;
    pub fn get_metadata(&self, name: &str) -> Option<ToolMetadata>;
    pub fn search_tools(&self, query: &str) -> Vec<Tool>;
    pub fn get_tools_by_priority(&self) -> Vec<Tool>;
    
    // 执行方法
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolResult>;
    
    // 管理方法
    pub fn set_tool_enabled(&self, name: &str, enabled: bool) -> Result<()>;
    pub fn update_tool_categories(&self, name: &str, categories: Vec<ToolCategory>) -> Result<()>;
    pub fn update_tool_priority(&self, name: &str, priority: u32) -> Result<()>;
    pub fn get_stats(&self) -> ToolRegistryStats;
}
```

## 使用示例

### 基本使用

```rust
use planned_agent_tool_manager::{ToolRegistry, ToolSource, ToolCategory};
use std::sync::Arc;

// 创建工具注册表
let registry = ToolRegistry::new();

// 设置 MCP 管理器（自动同步 MCP 工具）
registry.set_mcp_manager(mcp_manager);

// 获取所有工具（传给 LLM）
let tools = registry.get_all_tools();

// 调用工具
let result = registry.call_tool("tool_name", arguments).await?;
```

### 注册自定义工具

```rust
use planned_agent_tool_manager::{CustomTool, ToolCategory, ToolExecutor};
use planned_agent_core::mcp::types::{Tool, ToolResult};

struct MyCustomTool;

#[async_trait]
impl CustomTool for MyCustomTool {
    fn tool_definition(&self) -> Tool {
        Tool {
            name: "my_tool".to_string(),
            description: "My custom tool".to_string(),
            input_schema: json!({...}),
        }
    }
    
    fn categories(&self) -> Vec<ToolCategory> {
        vec![ToolCategory::Custom, ToolCategory::Utility]
    }
    
    async fn execute(&self, arguments: Value) -> Result<ToolResult> {
        // 实现工具逻辑
    }
}

// 注册自定义工具
let tool = Box::new(MyCustomTool);
let executor = Arc::new(CustomToolExecutor::new(tool));
registry.register_custom_tool(
    tool.tool_definition(),
    tool.categories(),
    executor,
);
```

### 注册内置工具提供者

```rust
use planned_agent_tool_manager::builtin::{FileToolsProvider, TextToolsProvider};

// 注册内置文件工具
let file_provider = FileToolsProvider;
registry.register_builtin_provider(&file_provider);

// 注册内置文本工具
let text_provider = TextToolsProvider;
registry.register_builtin_provider(&text_provider);
```

### 按分类查询工具

```rust
// 获取所有文件相关工具
let file_tools = registry.get_tools_by_category(&ToolCategory::FileRead);

// 根据分类列表获取工具（去重）
let categories = vec![ToolCategory::FileRead, ToolCategory::Directory];
let tools = registry.get_tools_by_categories(&categories);

// 获取所有内置工具
let builtin_tools = registry.get_tools_by_source("builtin");

// 搜索工具
let search_results = registry.search_tools("file");
```

### 工具优先级

```rust
// 按优先级排序获取工具
let tools = registry.get_tools_by_priority();

// 更新工具优先级
registry.update_tool_priority("tool_name", 10)?;
```

### 卸载工具

```rust
// 卸载单个工具
registry.unregister_tool("tool_name")?;

// 卸载 MCP 服务器的所有工具
let count = registry.unregister_mcp_server_tools("server_name")?;
println!("Unregistered {} tools", count);
```

### 更新工具分类

```rust
// 更新工具分类
registry.update_tool_categories("tool_name", vec![
    ToolCategory::FileRead,
    ToolCategory::Utility,
])?;
```

## 内置工具

### 文件工具 (FileToolsProvider)

| 工具名 | 描述 |
|--------|------|
| `builtin_read_file` | 读取文件内容 |
| `builtin_write_file` | 写入文件内容 |
| `builtin_list_dir` | 列出目录内容 |

### 文本工具 (TextToolsProvider)

| 工具名 | 描述 |
|--------|------|
| `builtin_text_search` | 在文本中搜索关键词 |
| `builtin_text_replace` | 替换文本中的内容 |

## 集成到主程序

### 1. 添加依赖

在 `crates/planned-agent/Cargo.toml` 中添加：

```toml
[dependencies]
planned-agent-tool-manager = { path = "../tool-manager" }
```

### 2. 为 McpManager 实现 McpManagerTrait

在 `crates/mcp-rmcp/src/manager.rs` 中（注意：现在从 core 导入，不再需要 `tool-manager`）：

```rust
use planned_agent_core::tool_registry::traits::McpManagerTrait;

#[async_trait]
impl McpManagerTrait for McpManager {
    async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        self.call_tool_auto(tool_name, arguments).await
    }
    
    fn get_all_tools(&self) -> Vec<Tool> {
        self.tool_manager.to_openai_tools()
    }
    
    fn find_server_for_tool(&self, tool_name: &str) -> Option<String> {
        self.tool_manager.find_server_for_tool(tool_name)
    }
    
    fn get_server_names(&self) -> Vec<String> {
        self.servers.keys().cloned().collect()
    }
}
```

### 3. 在 Agent 中使用

在 `crates/planned-agent/src/agent.rs` 中：

```rust
use planned_agent_tool_manager::{ToolRegistry, builtin::{FileToolsProvider, TextToolsProvider}};

// 创建工具注册表
let registry = ToolRegistry::new();

// 设置 MCP 管理器
registry.set_mcp_manager(Arc::new(mcp_manager));

// 注册内置工具
registry.register_builtin_provider(&FileToolsProvider);
registry.register_builtin_provider(&TextToolsProvider);

// 获取工具列表传给 LLM
let tools = registry.get_all_tools();

// 调用工具
let result = registry.call_tool(&tool_name, arguments).await?;
```

## 参数验证

ToolRegistry 在调用工具前会自动验证参数：

1. **必需字段检查**：检查 schema 中定义的 required 字段是否存在
2. **类型检查**：验证字段类型是否匹配（警告级别）

```rust
// 验证会自动执行
let result = registry.call_tool("my_tool", json!({
    "required_field": "value",
    "optional_field": 123
})).await?;
```

## 工具分类自动推断

MCP 工具注册时会自动推断分类：

```rust
// 设置 MCP 管理器时，工具会自动推断分类
registry.set_mcp_manager(mcp_manager);

// 手动推断分类
let tool = Tool {
    name: "read_file".to_string(),
    description: "读取文件内容".to_string(),
    input_schema: json!({}),
};
let categories = ToolCategory::infer_from_tool(&tool);
// 结果: [FileRead, Utility]
```

## 线程安全

ToolRegistry 使用 `RwLock` 包装所有字段，确保多线程环境下的安全性：

- 读操作（查询工具）可以并发执行
- 写操作（注册/卸载工具）是互斥的
- 工具调用可以并发执行

## 统计信息

```rust
let stats = registry.get_stats();
println!("Total tools: {}", stats.total);
println!("Enabled: {}", stats.enabled);
println!("MCP tools: {}", stats.mcp_count);
println!("Custom tools: {}", stats.custom_count);
println!("Builtin tools: {}", stats.builtin_count);
```

## 自动路由机制

调用工具时，ToolRegistry 会自动路由到正确的执行器：

```rust
let result = registry.call_tool("tool_name", arguments).await?;
```

路由规则：
1. **MCP 工具**：路由到 MCP 管理器
2. **自定义工具**：路由到对应的自定义执行器
3. **内置工具**：路由到对应的内置执行器

## 工具优先级

工具优先级用于在多个工具中选择最合适的：

- **优先级范围**：1-100，数值越小优先级越高
- **默认优先级**：
  - 内置工具：10（最高）
  - 自定义工具：50
  - MCP 工具：100

```rust
// 按优先级排序获取工具
let tools = registry.get_tools_by_priority();

// 更新工具优先级
registry.update_tool_priority("tool_name", 10)?;
```

## 依赖项

```toml
[dependencies]
planned-agent-core = { path = "../core" }
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
anyhow.workspace = true
async-trait.workspace = true
tracing.workspace = true
chrono = "0.4"
uuid = { version = "1", features = ["v4"] }
```
