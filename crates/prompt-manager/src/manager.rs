use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use anyhow::{Result, Context};
use async_trait::async_trait;
use serde_json::Value;
use tracing::info;

use planned_agent_core::prompt::{
    PromptManager, PromptTemplate, PromptContext, PromptInfo, OutputFormat,
};

use crate::config::PromptManagerConfig;
use crate::loader::FileLoader;
use crate::template::TemplateEngine;

/// 基于文件系统的Prompt管理器实现
pub struct FilePromptManager {
    config: PromptManagerConfig,
    prompts: Arc<RwLock<HashMap<String, PromptTemplate>>>,
    template_engine: Arc<RwLock<TemplateEngine>>,
}

impl Clone for FilePromptManager {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            prompts: Arc::clone(&self.prompts),
            template_engine: Arc::clone(&self.template_engine),
        }
    }
}

impl FilePromptManager {
    /// 清理LLM响应，去掉markdown代码块标记和多余文本
    fn clean_json_response(&self, response: &str) -> String {
        let trimmed = response.trim();
        
        // 去掉 ```json 和 ``` 标记
        if trimmed.starts_with("```json") && trimmed.ends_with("```") {
            return trimmed[7..trimmed.len()-3].trim().to_string();
        }
        if trimmed.starts_with("```") && trimmed.ends_with("```") {
            return trimmed[3..trimmed.len()-3].trim().to_string();
        }
        
        // 尝试提取JSON部分
        // 查找第一个 { 或 [
        if let Some(start) = trimmed.find('{').or_else(|| trimmed.find('[')) {
            // 查找最后一个 } 或 ]
            if let Some(end) = trimmed.rfind('}').or_else(|| trimmed.rfind(']')) {
                if start < end {
                    let json_part = &trimmed[start..=end];
                    // 验证是否是有效的JSON
                    if serde_json::from_str::<serde_json::Value>(json_part).is_ok() {
                        return json_part.to_string();
                    }
                }
            }
        }
        
        // 如果以上都不匹配，返回原始响应
        trimmed.to_string()
    }
    
    /// 创建新的Prompt管理器
    pub fn new(config: PromptManagerConfig) -> Result<Self> {
        let manager = Self {
            config,
            prompts: Arc::new(RwLock::new(HashMap::new())),
            template_engine: Arc::new(RwLock::new(TemplateEngine::new())),
        };
        
        Ok(manager)
    }
    
    /// 初始化管理器（加载所有prompt）
    pub async fn initialize(&self) -> Result<()> {
        info!("Initializing PromptManager with directory: {:?}", self.config.prompt_dir);
        
        let loader = FileLoader::new(self.config.prompt_dir.clone());
        let prompts = loader.load_all()?;
        
        // 添加模板到模板引擎
        let mut engine = self.template_engine.write().await;
        for (name, template) in &prompts {
            engine.add_template(name, &template.content)?;
        }
        
        // 保存prompt数据
        let mut prompts_map = self.prompts.write().await;
        *prompts_map = prompts;
        
        info!("PromptManager initialized with {} prompts", prompts_map.len());
        Ok(())
    }
    
    /// 验证LLM响应是否符合JSON Schema
    fn validate_json_response(&self, schema: &Value, response: &str) -> Result<bool> {
        // 尝试解析JSON响应
        let json_value: Value = serde_json::from_str(response)
            .context("Failed to parse response as JSON")?;
        
        // 简单的schema验证（实际项目中可以使用jsonschema库）
        // 这里我们只检查基本的类型匹配
        if let Some(schema_type) = schema.get("type").and_then(|t| t.as_str()) {
            match schema_type {
                "object" => {
                    if !json_value.is_object() {
                        return Ok(false);
                    }
                    
                    // 检查required字段
                    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
                        for field in required {
                            if let Some(field_name) = field.as_str() {
                                if !json_value.get(field_name).is_some() {
                                    return Ok(false);
                                }
                            }
                        }
                    }
                }
                "array" => {
                    if !json_value.is_array() {
                        return Ok(false);
                    }
                }
                "string" => {
                    if !json_value.is_string() {
                        return Ok(false);
                    }
                }
                "number" | "integer" => {
                    if !json_value.is_number() {
                        return Ok(false);
                    }
                }
                _ => {}
            }
        }
        
        Ok(true)
    }
}

#[async_trait]
impl PromptManager for FilePromptManager {
    async fn load_template(&self, name: &str) -> Result<PromptTemplate> {
        let prompts = self.prompts.read().await;
        prompts.get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Prompt not found: {}", name))
    }
    
    async fn render(&self, name: &str, context: &PromptContext) -> Result<String> {
        let prompts = self.prompts.read().await;
        let template = prompts.get(name)
            .ok_or_else(|| anyhow::anyhow!("Prompt not found: {}", name))?;
        
        let engine = self.template_engine.read().await;
        
        // 合并用户变量和自动生成的变量
        let mut variables = context.variables.clone();
        
        // 如果用户没有提供example变量，使用schema中定义的example
        if !variables.contains_key("example") {
            if let Some(schema) = &template.output_schema {
                if let Some(example) = &schema.example {
                    variables.insert("example".to_string(), serde_json::json!(example));
                }
            }
        }
        
        // 如果有输出约束，在渲染时添加
        let constraints = template.output_schema.as_ref()
            .and_then(|s| s.constraints.as_deref());
        
        engine.render_with_constraints(name, &variables, constraints)
    }
    
    async fn list_prompts(&self) -> Result<Vec<PromptInfo>> {
        let prompts = self.prompts.read().await;
        let mut result = Vec::new();
        
        for (name, template) in prompts.iter() {
            result.push(PromptInfo {
                name: name.clone(),
                description: template.metadata.description.clone(),
                variables: template.metadata.variables.iter()
                    .map(|v| v.name.clone())
                    .collect(),
                has_output_schema: template.output_schema.is_some(),
            });
        }
        
        Ok(result)
    }
    
    async fn exists(&self, name: &str) -> Result<bool> {
        let prompts = self.prompts.read().await;
        Ok(prompts.contains_key(name))
    }
    
    async fn reload(&self) -> Result<()> {
        info!("Reloading prompts...");
        self.initialize().await
    }
    
    async fn get_output_schema(&self, name: &str) -> Result<Option<Value>> {
        let prompts = self.prompts.read().await;
        let template = prompts.get(name)
            .ok_or_else(|| anyhow::anyhow!("Prompt not found: {}", name))?;
        
        match &template.output_schema {
            Some(schema) => {
                let schema_value = serde_json::to_value(schema)?;
                Ok(Some(schema_value))
            }
            None => Ok(None),
        }
    }
    
    async fn parse_response<T: serde::de::DeserializeOwned>(
        &self, 
        name: &str, 
        response: &str
    ) -> Result<T> {
        let prompts = self.prompts.read().await;
        let template = prompts.get(name)
            .ok_or_else(|| anyhow::anyhow!("Prompt not found: {}", name))?;
        
        // 清理响应，去掉markdown代码块标记
        let cleaned_response = self.clean_json_response(response);
        
        // 检查是否有输出schema定义
        if let Some(schema) = &template.output_schema {
            match schema.format {
                OutputFormat::Json => {
                    // 尝试解析JSON
                    let result: T = serde_json::from_str(&cleaned_response)
                        .map_err(|e| {
                            tracing::error!("JSON解析失败，原始响应：{}", response);
                            tracing::error!("清理后响应：{}", cleaned_response);
                            anyhow::anyhow!("Failed to parse JSON response: {}", e)
                        })?;
                    Ok(result)
                }
                _ => {
                    // 对于非JSON格式，直接反序列化
                    let result: T = serde_json::from_str(&cleaned_response)
                        .map_err(|e| {
                            tracing::error!("解析失败，原始响应：{}", response);
                            tracing::error!("清理后响应：{}", cleaned_response);
                            anyhow::anyhow!("Failed to parse response: {}", e)
                        })?;
                    Ok(result)
                }
            }
        } else {
            // 没有schema定义，尝试直接解析
            let result: T = serde_json::from_str(&cleaned_response)
                .map_err(|e| {
                    tracing::error!("解析失败，原始响应：{}", response);
                    tracing::error!("清理后响应：{}", cleaned_response);
                    anyhow::anyhow!("Failed to parse response: {}", e)
                })?;
            Ok(result)
        }
    }
    
    async fn validate_response(&self, name: &str, response: &str) -> Result<bool> {
        let prompts = self.prompts.read().await;
        let template = prompts.get(name)
            .ok_or_else(|| anyhow::anyhow!("Prompt not found: {}", name))?;
        
        // 清理响应，去掉markdown代码块标记
        let cleaned_response = self.clean_json_response(response);
        
        // 检查是否有输出schema定义
        if let Some(schema) = &template.output_schema {
            match schema.format {
                OutputFormat::Json => {
                    // 验证JSON格式
                    if let Some(json_schema) = &schema.json_schema {
                        return self.validate_json_response(json_schema, &cleaned_response);
                    }
                    
                    // 只检查是否为有效JSON
                    let _: Value = serde_json::from_str(&cleaned_response)?;
                    Ok(true)
                }
                _ => {
                    // 对于非JSON格式，暂时返回true
                    Ok(true)
                }
            }
        } else {
            // 没有schema定义，返回true
            Ok(true)
        }
    }
}
