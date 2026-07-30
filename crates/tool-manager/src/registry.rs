use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use anyhow::Result;
use serde_json::Value;
use tracing::info;

use planned_agent_core::types::Tool;
use planned_agent_core::tool_registry::{ToolSource, ToolCategory, ToolExecutor};
use crate::types::{ToolMetadata, ToolRegistryStats, ToolOutcome};
use crate::validator::ToolValidator;
// McpManagerTrait 已下沉到 core；这里通过 mcp_adapter 模块重新导出
// （保持向后兼容，未来可以直接 `use planned_agent_core::tool_registry::traits::McpManagerTrait`）
use crate::mcp_adapter::McpManagerTrait;

/// 统一工具注册表（线程安全）
pub struct ToolRegistry {
    /// 工具定义（name -> Tool）
    tools: RwLock<HashMap<String, Tool>>,
    
    /// 工具元数据（name -> Metadata）
    metadata: RwLock<HashMap<String, ToolMetadata>>,
    
    /// MCP 管理器（延迟注入）
    mcp_manager: RwLock<Option<Arc<dyn McpManagerTrait>>>,
    
    /// 自定义工具执行器（handler_id -> Executor）
    custom_executors: RwLock<HashMap<String, Arc<dyn ToolExecutor>>>,
    
    /// 内置工具执行器（handler_id -> Executor）
    builtin_executors: RwLock<HashMap<String, Arc<dyn ToolExecutor>>>,
    
    /// 分类索引（category -> tool_names）
    category_index: RwLock<HashMap<ToolCategory, Vec<String>>>,
}

impl ToolRegistry {
    /// 创建新的工具注册表
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
            metadata: RwLock::new(HashMap::new()),
            mcp_manager: RwLock::new(None),
            custom_executors: RwLock::new(HashMap::new()),
            builtin_executors: RwLock::new(HashMap::new()),
            category_index: RwLock::new(HashMap::new()),
        }
    }
    
    // ========== 注册方法 ==========
    
    /// 设置 MCP 管理器（自动同步 MCP 工具）
    pub fn set_mcp_manager(&self, manager: Arc<dyn McpManagerTrait>) {
        info!("Setting MCP manager, syncing tools...");
        
        // 获取 MCP 工具
        let mcp_tools = manager.get_all_tools();
        
        // 更新 MCP 管理器
        {
            let mut mcp = self.mcp_manager.write().unwrap();
            *mcp = Some(manager.clone());
        }
        
        // 同步 MCP 工具
        for tool in mcp_tools {
            //打印一下tool 
            let server_name = manager.find_server_for_tool(&tool.name)
                .unwrap_or_else(|| "unknown".to_string());
            
            // 获取服务器配置的分类
            let server_categories = manager.get_server_categories(&server_name)
                .map(|cats| cats.iter().filter_map(|s| {
                    // 将字符串转换为 ToolCategory
                    match s.as_str() {
                        "Browser" => Some(ToolCategory::Browser),
                        "File" => Some(ToolCategory::File),
                        "Text" => Some(ToolCategory::Text),
                        "Data" => Some(ToolCategory::Data),
                        "System" => Some(ToolCategory::System),
                        "Device" => Some(ToolCategory::Device),
                        "Dev" => Some(ToolCategory::Dev),
                        "Utility" => Some(ToolCategory::Utility),
                        _ => None,
                    }
                }).collect());
            
            // 自动推断分类（配置优先 + 自动推断）
            let categories = server_categories.unwrap_or_else(|| {
                // 默认使用 Utility 分类
                vec![ToolCategory::Utility]
            });
            
            let metadata = ToolMetadata {
                source: ToolSource::Mcp { server_name },
                categories,
                enabled: true,
                priority: 100, // MCP 工具默认优先级
                tags: vec![],
                created_at: chrono::Utc::now(),
                version: None,
            };
            
            self.register_tool(tool, metadata);
        }
        
        let tools = self.tools.read().unwrap();
        info!("MCP tools synced: {} tools", tools.len());
    }
    
    /// 注册单个工具
    pub fn register_tool(&self, tool: Tool, metadata: ToolMetadata) {
        let name = tool.name.clone();
        
        // 更新分类索引
        {
            let mut index = self.category_index.write().unwrap();
            for category in &metadata.categories {
                index.entry(category.clone())
                    .or_insert_with(Vec::new)
                    .push(name.clone());
            }
        }
        
        // 更新工具和元数据
        {
            let mut tools = self.tools.write().unwrap();
            let mut meta = self.metadata.write().unwrap();
            tools.insert(name.clone(), tool);
            meta.insert(name.clone(), metadata);
        }
        
        info!("Registered tool: {}", name);
    }
    
    /// 注册自定义工具（带执行器）
    pub fn register_custom_tool(
        &self,
        tool: Tool,
        categories: Vec<ToolCategory>,
        executor: Arc<dyn ToolExecutor>,
    ) {
        let handler_id = tool.name.clone();
        
        let metadata = ToolMetadata {
            source: ToolSource::Custom { handler_id: handler_id.clone() },
            categories,
            enabled: true,
            priority: 50, // 自定义工具默认优先级
            tags: vec![],
            created_at: chrono::Utc::now(),
            version: None,
        };
        
        // 更新执行器
        {
            let mut executors = self.custom_executors.write().unwrap();
            executors.insert(handler_id, executor);
        }
        
        self.register_tool(tool, metadata);
    }
    
    /// 注册内置工具（带执行器）
    pub fn register_builtin_tool(
        &self,
        tool: Tool,
        categories: Vec<ToolCategory>,
        executor: Arc<dyn ToolExecutor>,
    ) {
        let handler_id = tool.name.clone();
        
        let metadata = ToolMetadata {
            source: ToolSource::Builtin,
            categories,
            enabled: true,
            priority: 10, // 内置工具优先级最高
            tags: vec!["builtin".to_string()],
            created_at: chrono::Utc::now(),
            version: None,
        };
        
        // 更新执行器
        {
            let mut executors = self.builtin_executors.write().unwrap();
            executors.insert(handler_id, executor);
        }
        
        self.register_tool(tool, metadata);
    }
    
    /// 批量注册自定义工具
    pub fn register_custom_tools(
        &self,
        tools: Vec<(Tool, Vec<ToolCategory>)>,
        executor: Arc<dyn ToolExecutor>,
    ) {
        for (tool, categories) in tools {
            self.register_custom_tool(tool, categories, executor.clone());
        }
    }
    
    /// 注册内置工具提供者的所有工具
    pub fn register_builtin_provider(&self, provider: &dyn crate::builtin::BuiltinToolProvider) {
        let tools = provider.tools();
        let executor = provider.executor();
        for (tool, categories) in tools {
            self.register_builtin_tool(tool, categories, executor.clone());
        }
    }
    
    // ========== 卸载方法 ==========
    
    /// 卸载工具
    pub fn unregister_tool(&self, name: &str) -> Result<()> {
        // 检查工具是否存在
        {
            let tools = self.tools.read().unwrap();
            if !tools.contains_key(name) {
                return Err(anyhow::anyhow!("Tool not found: {}", name));
            }
        }
        
        // 获取元数据用于清理分类索引
        let metadata = {
            let meta = self.metadata.read().unwrap();
            meta.get(name).cloned()
        };
        
        // 清理分类索引
        if let Some(meta) = &metadata {
            let mut index = self.category_index.write().unwrap();
            for category in &meta.categories {
                if let Some(tools) = index.get_mut(category) {
                    tools.retain(|n| n != name);
                }
            }
        }
        
        // 清理执行器
        if let Some(meta) = &metadata {
            match &meta.source {
                ToolSource::Custom { handler_id } => {
                    let mut executors = self.custom_executors.write().unwrap();
                    executors.remove(handler_id);
                }
                ToolSource::Builtin => {
                    let mut executors = self.builtin_executors.write().unwrap();
                    executors.remove(name);
                }
                _ => {} // MCP 工具不需要清理执行器
            }
        }
        
        // 移除工具和元数据
        {
            let mut tools = self.tools.write().unwrap();
            let mut meta = self.metadata.write().unwrap();
            tools.remove(name);
            meta.remove(name);
        }
        
        info!("Unregistered tool: {}", name);
        Ok(())
    }
    
    /// 卸载 MCP 服务器的所有工具
    pub fn unregister_mcp_server_tools(&self, server_name: &str) -> Result<usize> {
        let tools_to_remove: Vec<String> = {
            let meta = self.metadata.read().unwrap();
            meta.iter()
                .filter(|(_, m)| {
                    matches!(&m.source, ToolSource::Mcp { server_name: sn } if sn == server_name)
                })
                .map(|(name, _)| name.clone())
                .collect()
        };
        
        let count = tools_to_remove.len();
        for name in tools_to_remove {
            self.unregister_tool(&name)?;
        }
        
        info!("Unregistered {} tools from MCP server: {}", count, server_name);
        Ok(count)
    }
    
    // ========== 查询方法 ==========
    
    /// 获取所有工具（传给 LLM）
    pub fn get_all_tools(&self) -> Vec<Tool> {
        let tools = self.tools.read().unwrap();
        let meta = self.metadata.read().unwrap();
        
        tools.values()
            .filter(|t| {
                meta.get(&t.name)
                    .map(|m| m.enabled)
                    .unwrap_or(false)
            })
            .cloned()
            .collect()
    }
    
    /// 获取指定分类的工具
    pub fn get_tools_by_category(&self, category: &ToolCategory) -> Vec<Tool> {
        let index = self.category_index.read().unwrap();
        let tools = self.tools.read().unwrap();
        let meta = self.metadata.read().unwrap();
        
        if let Some(tool_names) = index.get(category) {
            tool_names.iter()
                .filter_map(|name| tools.get(name))
                .filter(|t| {
                    meta.get(&t.name)
                        .map(|m| m.enabled)
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        }
    }
    
    /// 根据分类列表获取工具
    /// 
    /// 返回所有匹配这些分类的工具，去重
    pub fn get_tools_by_categories(&self, categories: &[ToolCategory]) -> Vec<Tool> {
        let mut all_tools = Vec::new();
        let mut seen_names = std::collections::HashSet::new();
        
        for category in categories {
            let tools = self.get_tools_by_category(category);
            for tool in tools {
                if seen_names.insert(tool.name.clone()) {
                    all_tools.push(tool);
                }
            }
        }
        
        all_tools
    }
    
    /// 获取指定来源的工具
    pub fn get_tools_by_source(&self, source_type: &str) -> Vec<Tool> {
        let tools = self.tools.read().unwrap();
        let meta = self.metadata.read().unwrap();
        
        meta.iter()
            .filter(|(_, m)| {
                match (&m.source, source_type) {
                    (ToolSource::Mcp { .. }, "mcp") => true,
                    (ToolSource::Custom { .. }, "custom") => true,
                    (ToolSource::Builtin, "builtin") => true,
                    _ => false,
                }
            })
            .filter(|(_, m)| m.enabled)
            .filter_map(|(name, _)| tools.get(name))
            .cloned()
            .collect()
    }
    
    /// 根据名称获取工具
    pub fn get_tool(&self, name: &str) -> Option<Tool> {
        let tools = self.tools.read().unwrap();
        tools.get(name).cloned()
    }
    
    /// 根据名称获取工具元数据
    pub fn get_metadata(&self, name: &str) -> Option<ToolMetadata> {
        let meta = self.metadata.read().unwrap();
        meta.get(name).cloned()
    }
    
    /// 搜索工具（按名称、描述、标签）
    pub fn search_tools(&self, query: &str) -> Vec<Tool> {
        let query_lower = query.to_lowercase();
        let tools = self.tools.read().unwrap();
        let meta = self.metadata.read().unwrap();
        
        tools.values()
            .filter(|t| {
                let m = meta.get(&t.name);
                let enabled = m.map(|m| m.enabled).unwrap_or(false);
                if !enabled {
                    return false;
                }
                
                // 名称匹配
                if t.name.to_lowercase().contains(&query_lower) {
                    return true;
                }
                
                // 描述匹配
                if t.description.to_lowercase().contains(&query_lower) {
                    return true;
                }
                
                // 标签匹配
                if let Some(m) = m {
                    if m.tags.iter().any(|tag| tag.to_lowercase().contains(&query_lower)) {
                        return true;
                    }
                }
                
                false
            })
            .cloned()
            .collect()
    }
    
    /// 按优先级排序获取工具
    pub fn get_tools_by_priority(&self) -> Vec<Tool> {
        let tools = self.tools.read().unwrap();
        let meta = self.metadata.read().unwrap();
        
        let mut tools_with_priority: Vec<(Tool, u32)> = tools.values()
            .filter(|t| {
                meta.get(&t.name)
                    .map(|m| m.enabled)
                    .unwrap_or(false)
            })
            .map(|t| {
                let priority = meta.get(&t.name)
                    .map(|m| m.priority)
                    .unwrap_or(100);
                (t.clone(), priority)
            })
            .collect();
        
        // 按优先级排序（数值越小优先级越高）
        tools_with_priority.sort_by_key(|(_, p)| *p);
        
        tools_with_priority.into_iter().map(|(t, _)| t).collect()
    }
    
    // ========== 执行方法 ==========
    
    /// 调用工具（自动路由，带参数验证）
    /// 返回 `ToolOutcome`，可在不再次查询注册表的情况下取得工具分类。
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolOutcome> {
        // 1. 检查工具是否存在
        let (tool, metadata) = {
            let tools = self.tools.read().unwrap();
            let meta = self.metadata.read().unwrap();

            let tool = tools.get(name)
                .ok_or_else(|| anyhow::anyhow!("Tool not found: {}", name))?
                .clone();
            let metadata = meta.get(name)
                .ok_or_else(|| anyhow::anyhow!("Tool metadata not found: {}", name))?
                .clone();

            (tool, metadata)
        };

        // 2. 检查工具是否启用
        if !metadata.enabled {
            return Err(anyhow::anyhow!("Tool is disabled: {}", name));
        }

        // 3. 验证参数
        ToolValidator::validate_arguments(&tool, &arguments)?;

        // 4. 根据来源路由到正确的执行器。
        // 只在读取共享状态时持锁；异步执行前克隆 Arc 并释放 RwLockReadGuard，
        // 避免 call_tool future 变成 !Send。
        let result = match &metadata.source {
            ToolSource::Mcp { server_name } => {
                let mcp = self.mcp_manager.read().unwrap().clone();
                if let Some(mcp) = mcp {
                    info!("Routing to MCP server '{}': {}", server_name, name);
                    mcp.call_tool(name, arguments).await
                } else {
                    Err(anyhow::anyhow!("MCP manager not initialized"))
                }
            }
            ToolSource::Custom { handler_id } => {
                let executor = self
                    .custom_executors
                    .read()
                    .unwrap()
                    .get(handler_id)
                    .cloned();
                if let Some(executor) = executor {
                    info!("Routing to custom executor '{}': {}", handler_id, name);
                    executor.execute(name, arguments).await
                } else {
                    Err(anyhow::anyhow!("Custom executor not found: {}", handler_id))
                }
            }
            ToolSource::Builtin => {
                let executor = self
                    .builtin_executors
                    .read()
                    .unwrap()
                    .get(name)
                    .cloned();
                if let Some(executor) = executor {
                    info!("Routing to builtin executor: {}", name);
                    executor.execute(name, arguments).await
                } else {
                    Err(anyhow::anyhow!("Builtin executor not found: {}", name))
                }
            }
        }?;

        Ok(ToolOutcome::new(result, metadata.categories))
    }
    
    // ========== 管理方法 ==========
    
    /// 启用/禁用工具
    pub fn set_tool_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        let mut meta = self.metadata.write().unwrap();
        if let Some(metadata) = meta.get_mut(name) {
            metadata.enabled = enabled;
            info!("Tool '{}' {}", name, if enabled { "enabled" } else { "disabled" });
            Ok(())
        } else {
            Err(anyhow::anyhow!("Tool not found: {}", name))
        }
    }
    
    /// 更新工具分类
    pub fn update_tool_categories(&self, name: &str, categories: Vec<ToolCategory>) -> Result<()> {
        // 获取旧分类
        let old_categories = {
            let meta = self.metadata.read().unwrap();
            meta.get(name)
                .map(|m| m.categories.clone())
                .ok_or_else(|| anyhow::anyhow!("Tool not found: {}", name))?
        };
        
        // 更新分类索引
        {
            let mut index = self.category_index.write().unwrap();
            
            // 移除旧的分类索引
            for category in &old_categories {
                if let Some(tools) = index.get_mut(category) {
                    tools.retain(|n| n != name);
                }
            }
            
            // 添加新的分类索引
            for category in &categories {
                index.entry(category.clone())
                    .or_insert_with(Vec::new)
                    .push(name.to_string());
            }
        }
        
        // 更新元数据
        {
            let mut meta = self.metadata.write().unwrap();
            if let Some(metadata) = meta.get_mut(name) {
                metadata.categories = categories;
            }
        }
        
        Ok(())
    }
    
    /// 更新工具优先级
    pub fn update_tool_priority(&self, name: &str, priority: u32) -> Result<()> {
        let mut meta = self.metadata.write().unwrap();
        if let Some(metadata) = meta.get_mut(name) {
            metadata.priority = priority;
            Ok(())
        } else {
            Err(anyhow::anyhow!("Tool not found: {}", name))
        }
    }
    
    /// 获取统计信息
    pub fn get_stats(&self) -> ToolRegistryStats {
        let tools = self.tools.read().unwrap();
        let meta = self.metadata.read().unwrap();
        
        let total = tools.len();
        let enabled = meta.values().filter(|m| m.enabled).count();
        let mcp_count = meta.values()
            .filter(|m| matches!(m.source, ToolSource::Mcp { .. }))
            .count();
        let custom_count = meta.values()
            .filter(|m| matches!(m.source, ToolSource::Custom { .. }))
            .count();
        let builtin_count = meta.values()
            .filter(|m| matches!(m.source, ToolSource::Builtin))
            .count();
        
        ToolRegistryStats {
            total,
            enabled,
            disabled: total - enabled,
            mcp_count,
            custom_count,
            builtin_count,
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
