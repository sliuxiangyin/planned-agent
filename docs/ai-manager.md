# AI 管理器 (`crates/ai-manager`)

## 概述

AI 管理器是一个专门用于管理多个 AI 客户端的模块。它负责：
- 读取配置文件中的 AI 提供商配置
- 根据配置创建对应的 AI 客户端实例
- 提供统一的接口访问不同 AI 提供商
- 通过配置中的 `name` 字段切换使用哪个 AI 客户端

## 目录结构

```
crates/ai-manager/
├── Cargo.toml
└── src/
    └── lib.rs         # AI 管理器实现
```

## 核心功能

### 1. 多提供商支持

AI 管理器支持同时管理多个 AI 提供商，每个提供商通过配置文件中的 `name` 字段进行标识。

### 2. 动态客户端创建

根据配置中的 `provider` 字段，自动创建对应的 AI 客户端实例：
- `provider = "openai"` -> 创建 `OpenAiClient`
- 未来可扩展支持其他提供商（如 Anthropic、Google 等）

### 3. 统一访问接口

提供两个核心方法：
- `default()` - 获取默认的 AI 客户端
- `get(name)` - 获取指定名称的 AI 客户端

## 接口设计

### AiManager 结构体

```rust
pub struct AiManager {
    clients: HashMap<String, Arc<dyn AiClient>>,
    default_name: Option<String>,
}
```

### 核心方法

#### `from_config(configs: Vec<AiProviderConfig>) -> anyhow::Result<Self>`

从配置列表初始化 AI 管理器。

**参数**：
- `configs`: AI 提供商配置列表

**返回**：
- `Ok(AiManager)`: 初始化成功的 AI 管理器
- `Err(anyhow::Error)`: 初始化失败（如不支持的提供商类型）

#### `default(&self) -> anyhow::Result<&dyn AiClient>`

获取默认的 AI 客户端。

**返回**：
- `Ok(&dyn AiClient)`: 默认的 AI 客户端
- `Err(anyhow::Error)`: 没有配置默认提供商

#### `get(&self, name: &str) -> anyhow::Result<&dyn AiClient>`

获取指定名称的 AI 客户端。

**参数**：
- `name`: 提供商名称

**返回**：
- `Ok(&dyn AiClient)`: 指定的 AI 客户端
- `Err(anyhow::Error)`: 指定的提供商不存在

#### `provider_names(&self) -> Vec<String>`

获取所有提供商名称。

**返回**：
- `Vec<String>`: 提供商名称列表

#### `has_default(&self) -> bool`

检查是否有默认提供商。

**返回**：
- `bool`: 是否有默认提供商

#### `provider_count(&self) -> usize`

获取提供商数量。

**返回**：
- `usize`: 提供商数量

## 使用示例

### 基本使用

```rust
use planned_agent_ai_manager::AiManager;
use planned_agent_core::ai::config::AiProviderConfig;

// 从配置初始化
let configs = vec![
    AiProviderConfig {
        name: "deepseek".to_string(),
        provider: "openai".to_string(),
        api_key: "sk-deepseek-key".to_string(),
        model: "deepseek-v4-flash".to_string(),
        base_url: Some("https://api.deepseek.com/v1".to_string()),
        temperature: Some(0.7),
        max_tokens: Some(4096),
        is_default: true,
    },
];

let manager = AiManager::from_config(configs)?;

// 获取默认客户端
let default_client = manager.default()?;

// 获取指定客户端
let deepseek_client = manager.get("deepseek")?;

// 检查提供商信息
println!("提供商数量: {}", manager.provider_count());
println!("提供商列表: {:?}", manager.provider_names());
```

### 在 Agent 中使用

```rust
use planned_agent_ai_manager::AiManager;

pub struct Agent {
    ai_manager: Option<AiManager>,
    // ... 其他字段
}

impl Agent {
    pub fn init_ai_clients(&mut self, configs: Vec<AiProviderConfig>) -> Result<()> {
        self.ai_manager = Some(AiManager::from_config(configs)?);
        Ok(())
    }
    
    pub fn get_ai_client(&self, provider_name: Option<&str>) -> Result<&dyn AiClient> {
        let manager = self.ai_manager.as_ref()
            .ok_or_else(|| anyhow::anyhow!("AI manager not initialized"))?;
        
        match provider_name {
            Some(name) => manager.get(name),
            None => manager.default(),
        }
    }
}
```

## 配置示例

### config.toml

```toml
# 多AI提供商配置
[[ai_providers]]
name = "deepseek"
provider = "openai"
base_url = "https://api.deepseek.com/v1"
api_key = "sk-deepseek-key"
model = "deepseek-v4-flash"
max_tokens = 4096
temperature = 0.7
is_default = true

[[ai_providers]]
name = "openai"
provider = "openai"
api_key = "sk-openai-key"
model = "gpt-4"
is_default = false

[[ai_providers]]
name = "anthropic"
provider = "anthropic"
api_key = "sk-anthropic-key"
model = "claude-3-opus"
is_default = false
```

## 扩展性

AI 管理器的设计支持轻松扩展新的 AI 提供商：

1. **添加新的 AI 适配器模块**（如 `ai-anthropic`）
2. **在 AI 管理器中注册新的提供商类型**
3. **在配置文件中添加新的提供商配置**

### 扩展示例

```rust
// 在 AiManager::from_config 中添加新的提供商支持
pub fn from_config(configs: Vec<AiProviderConfig>) -> anyhow::Result<Self> {
    let mut clients = HashMap::new();
    let mut default_name = None;
    
    for config in configs {
        let client: Arc<dyn AiClient> = match config.provider.as_str() {
            "openai" => {
                // 创建 OpenAI 客户端
                Arc::new(OpenAiClient::new(client_config))
            }
            "anthropic" => {
                // 创建 Anthropic 客户端
                Arc::new(AnthropicClient::new(client_config))
            }
            _ => return Err(anyhow::anyhow!("Unsupported AI provider: {}", config.provider)),
        };
        
        clients.insert(config.name.clone(), client);
    }
    
    Ok(Self { clients, default_name })
}
```

## 错误处理

AI 管理器使用 `anyhow::Result` 进行错误处理，主要错误情况包括：

1. **不支持的提供商类型**：配置中的 `provider` 字段不是已知的提供商类型
2. **没有默认提供商**：调用 `default()` 时没有配置 `is_default = true` 的提供商
3. **提供商不存在**：调用 `get(name)` 时指定的名称不存在
4. **管理器未初始化**：在调用方法前未调用 `from_config()` 初始化

## 依赖关系

```
planned-agent-ai-manager
├── planned-agent-core        # 核心 trait 和类型定义
├── planned-agent-ai-openai   # OpenAI 客户端实现
└── 其他 AI 适配器模块（未来）
```

## 设计优势

1. **模块化**：AI 管理逻辑独立于具体的 AI 实现
2. **可扩展**：轻松添加新的 AI 提供商
3. **统一接口**：通过 `default()` 和 `get(name)` 提供一致的访问方式
4. **配置驱动**：通过配置文件管理多个 AI 提供商
5. **类型安全**：使用 Rust 的类型系统确保安全性
