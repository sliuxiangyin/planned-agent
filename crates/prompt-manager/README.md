# Prompt Manager

一个基于文件系统的 Prompt 模板管理器，用于加载、管理和渲染 AI 提示词模板。

## 功能特性

- **多格式支持**：支持 TOML、文本（.txt）和 Markdown（.md）格式的 Prompt 文件
- **模板引擎**：使用 Tera 模板引擎，支持变量替换和条件逻辑
- **响应解析**：自动清理 LLM 响应中的 Markdown 代码块标记
- **Schema 验证**：支持 JSON Schema 验证，确保输出格式正确
- **示例生成**：从 JSON Schema 自动生成示例，指导 LLM 输出格式
- **热重载**：支持运行时重新加载 Prompt 模板

## 安装

在 `Cargo.toml` 中添加依赖：

```toml
[dependencies]
planned-agent-prompt-manager = { path = "../crates/prompt-manager" }
```

## 快速开始

### 1. 创建 Prompt 文件

在项目根目录创建 `prompts` 文件夹，并添加 Prompt 文件：

**示例：`prompts/chat/system.toml`**

```toml
[name]
description = "系统提示词模板"

[content]
text = """
你是一个AI助手。用户名称为 {{ user_name }}。

上下文信息：
{{ context }}

请根据以上信息提供帮助。
"""

[variables]
user_name = { description = "用户名称", required = true }
context = { description = "上下文信息", default_value = "无" }

[output_schema]
format = "json"
json_schema = { type = "object", properties = { response = { type = "string" } } }
```

### 2. 使用管理器

```rust
use planned_agent_prompt_manager::{FilePromptManager, PromptManagerConfig};
use planned_agent_core::prompt::{PromptManager, PromptContext};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 创建配置
    let config = PromptManagerConfig {
        prompt_dir: std::path::PathBuf::from("./prompts"),
        ..Default::default()
    };

    // 创建并初始化管理器
    let manager = FilePromptManager::new(config)?;
    manager.initialize().await?;

    // 列出所有可用的 Prompt
    let prompts = manager.list_prompts().await?;
    println!("可用的 Prompt: {}", prompts.len());

    // 渲染 Prompt
    let context = PromptContext::new()
        .with_variable("user_name", json!("张三"))
        .with_variable("context", json!("这是一个测试"));

    let rendered = manager.render("chat/system", &context).await?;
    println!("渲染后的 Prompt:\n{}", rendered);

    Ok(())
}
```

### 3. 验证和解析 LLM 响应

```rust
// 验证响应是否符合 Schema
let response = r#"{"response": "你好！"}"#;
let is_valid = manager.validate_response("chat/system", response).await?;
println!("响应有效: {}", is_valid);

// 解析响应到自定义类型
#[derive(serde::Deserialize)]
struct ChatResponse {
    response: String,
}

let parsed: ChatResponse = manager.parse_response("chat/system", response).await?;
println!("解析后的响应: {}", parsed.response);
```

## 配置说明

`PromptManagerConfig` 结构体包含以下配置项：

```rust
pub struct PromptManagerConfig {
    /// Prompt 文件目录
    pub prompt_dir: PathBuf,
    
    /// 模板引擎配置
    pub template_engine: TemplateEngineConfig,
    
    /// 缓存配置
    pub cache: CacheConfig,
}

pub struct TemplateEngineConfig {
    /// 是否自动重新加载模板
    pub auto_reload: bool,
}

pub struct CacheConfig {
    /// 是否启用缓存
    pub enabled: bool,
    /// 最大缓存条目数
    pub max_size: usize,
    /// 缓存过期时间（秒）
    pub ttl_seconds: u64,
}
```

默认配置：
- `prompt_dir`: `./prompts`
- `auto_reload`: `true`
- `cache.enabled`: `true`
- `cache.max_size`: `1000`
- `cache.ttl_seconds`: `3600`

### 从配置文件加载

```rust
let config = PromptManagerConfig::from_file(std::path::Path::new("config.toml"))?;
```

## Prompt 文件格式

### TOML 格式（推荐）

TOML 格式支持完整的功能，包括变量定义、输出 Schema 等。

```toml
[name]
description = "提示词描述"

[content]
text = """
你的提示词内容，支持 Tera 模板语法。
变量：{{ variable_name }}
"""

[variables]
variable_name = { description = "变量描述", required = true, default_value = "默认值" }

[output_schema]
format = "json"
json_schema = { ... }
example = { text = "..." }
constraints = "输出约束说明"
```

### 纯文本格式（.txt）

简单的文本 Prompt，不支持变量和 Schema：

```
你是一个AI助手，请根据用户输入提供帮助。
```

### Markdown 格式（.md）

Markdown 格式的 Prompt，支持富文本：

```markdown
# 系统提示词

你是一个AI助手，具有以下能力：

- 回答问题
- 生成文本
- 分析数据
```

## 模板语法

使用 [Tera](https://docs.rs/tera) 模板引擎语法：

### 变量替换

```text
用户名：{{ user_name }}
```

### 条件语句

```text
{% if context %}
上下文：{{ context }}
{% else %}
无上下文信息
{% endif %}
```

### 循环

```text
{% for item in items %}
- {{ item }}
{% endfor %}
```

## 输出 Schema 定义

### 基本结构

```toml
[output_schema]
format = "json"  # json, text, markdown, yaml, xml
json_schema = { ... }
example = { text = "..." }
constraints = "输出约束说明"
```

### JSON Schema 示例

```toml
[output_schema]
format = "json"
json_schema = { 
  type = "object", 
  properties = { 
    entities = { type = "array", items = { type = "string" } },
    summary = { type = "string" },
    sentiment = { type = "string", enum = ["positive", "negative", "neutral"] }
  },
  required = ["entities", "summary", "sentiment"]
}
```

### 自动示例生成

如果未提供 `example`，管理器会根据 `json_schema` 自动生成示例。

## 响应处理

### 自动清理

管理器会自动清理 LLM 响应中的：
- Markdown 代码块标记（```json ... ```）
- 多余的空白字符
- 未转义的引号（自动修复）

### JSON 解析

支持从复杂响应中提取 JSON：

1. 整体围栏清理
2. 代码块提取
3. 括号配对扫描
4. 自动修复常见错误

## API 参考

### `FilePromptManager`

```rust
impl FilePromptManager {
    /// 创建新的 Prompt 管理器
    pub fn new(config: PromptManagerConfig) -> Result<Self>;
    
    /// 初始化管理器（加载所有 Prompt）
    pub async fn initialize(&self) -> Result<()>;
}
```

### `PromptManager` trait

```rust
#[async_trait]
pub trait PromptManager {
    /// 加载模板
    async fn load_template(&self, name: &str) -> Result<PromptTemplate>;
    
    /// 渲染模板
    async fn render(&self, name: &str, context: &PromptContext) -> Result<String>;
    
    /// 列出所有 Prompt
    async fn list_prompts(&self) -> Result<Vec<PromptInfo>>;
    
    /// 检查 Prompt 是否存在
    async fn exists(&self, name: &str) -> Result<bool>;
    
    /// 重新加载 Prompt
    async fn reload(&self) -> Result<()>;
    
    /// 获取输出 Schema
    async fn get_output_schema(&self, name: &str) -> Result<Option<Value>>;
    
    /// 解析响应
    async fn parse_response<T: DeserializeOwned>(&self, name: &str, response: &str) -> Result<T>;
    
    /// 验证响应
    async fn validate_response(&self, name: &str, response: &str) -> Result<bool>;
}
```

## 目录结构

```
prompts/
├── chat/
│   ├── system.toml
│   └── user.toml
├── analysis/
│   └── extract_info.toml
└── planning/
    └── coarse_plan.toml
```

Prompt 名称由文件路径决定（去掉扩展名，使用 `/` 分隔）：
- `prompts/chat/system.toml` → `chat/system`
- `prompts/analysis/extract_info.toml` → `analysis/extract_info`

## 最佳实践

1. **使用 TOML 格式**：推荐使用 TOML 格式以获得完整功能支持
2. **定义清晰的变量**：为每个变量提供描述和默认值
3. **指定输出 Schema**：确保 LLM 输出符合预期格式
4. **提供示例**：在 Schema 中提供示例以指导 LLM 输出
5. **组织目录结构**：按功能模块组织 Prompt 文件

## 示例

### 文本提取 Prompt

```toml
[name]
description = "从文本中提取信息"

[content]
text = """
请从以下文本中提取关键信息：

{{ text }}

请返回 JSON 格式，包含以下字段：
- entities: 实体列表
- summary: 摘要
- sentiment: 情感（positive/negative/neutral）
"""

[variables]
text = { description = "输入文本", required = true }

[output_schema]
format = "json"
json_schema = {
  type = "object",
  properties = {
    entities = { type = "array", items = { type = "string" } },
    summary = { type = "string" },
    sentiment = { type = "string", enum = ["positive", "negative", "neutral"] }
  },
  required = ["entities", "summary", "sentiment"]
}
constraints = "请确保返回有效的 JSON 格式，不要包含其他文本说明"
```

### 多轮对话 Prompt

```toml
[name]
description = "多轮对话系统提示词"

[content]
text = """
你是一个AI助手，正在与用户 {{ user_name }} 进行对话。

{% if history %}
对话历史：
{% for message in history %}
{{ message.role }}: {{ message.content }}
{% endfor %}
{% endif %}

当前问题：{{ question }}

请提供有帮助的回答。
"""

[variables]
user_name = { description = "用户名称", required = true }
history = { description = "对话历史", required = false }
question = { description = "当前问题", required = true }
```

## 故障排除

### 常见问题

1. **Prompt 文件未加载**
   - 检查 `prompt_dir` 路径是否正确
   - 确认文件格式是否支持（.toml, .txt, .md）
   - 查看日志输出，确认是否有解析错误

2. **模板渲染失败**
   - 检查变量名是否正确
   - 确认必需变量是否都已提供
   - 验证 Tera 语法是否正确

3. **JSON 解析失败**
   - 检查 LLM 响应格式
   - 使用 `clean_json_response` 方法清理响应
   - 验证 JSON Schema 是否匹配

### 调试模式

启用详细日志：

```rust
use tracing_subscriber::EnvFilter;

tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::from_default_env())
    .init();
```

设置环境变量：

```bash
RUST_LOG=planned_agent_prompt_manager=debug
```

## 许可证

本项目是 `planned-agent` 的一部分，具体许可证请参考项目根目录的 LICENSE 文件。