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
    /// 修复 JSON 字符串值中 LLM 常犯的错误：未转义的双引号。
    ///
    /// 问题示例：
    ///   `"reasoning": "...section中的"某某标题"..."` 
    ///   → 内层 `"` 会提前终止字符串，导致 JSON 解析失败。
    ///
    /// 策略：状态机扫描，当处于字符串内部时，若遇到 `"` 且后邻字符
    /// 不是 JSON 结构符（`,` `}` `]` `:` 或空白），则判定为需转义的
    /// 内容引号，自动在前面插入 `\`。
    fn repair_json_string_quotes(json_str: &str) -> String {
        let chars: Vec<char> = json_str.chars().collect();
        // 预分配，避免反复扩容
        let mut result = String::with_capacity(json_str.len() + json_str.len() / 20);
        let mut i = 0;
        let mut in_string = false;

        while i < chars.len() {
            let ch = chars[i];

            // 已转义序列 → 原样透传两个字符
            if ch == '\\' && in_string && i + 1 < chars.len() {
                result.push(ch);
                result.push(chars[i + 1]);
                i += 2;
                continue;
            }

            if ch == '"' {
                if !in_string {
                    // 进入字符串
                    in_string = true;
                    result.push(ch);
                } else {
                    // 字符串内遇到 `"`：判断是真正的结束引号还是需修复的内容引号
                    let is_terminator = {
                        let rest: String = chars[i + 1..].iter().take(50).collect();
                        let t = rest.trim_start();
                        t.is_empty()
                            || t.starts_with(',')
                            || t.starts_with('}')
                            || t.starts_with(']')
                            || t.starts_with(':')
                    };

                    if is_terminator {
                        in_string = false;
                        result.push(ch);
                    } else {
                        // 内容引号 → 转义
                        result.push('\\');
                        result.push(ch);
                    }
                }
                i += 1;
                continue;
            }

            result.push(ch);
            i += 1;
        }

        result
    }

    /// 清理LLM响应，去掉markdown代码块标记和多余文本。
    ///
    /// 处理顺序：
    /// 1. 整体围栏（```json ... ``` / ``` ... ```）直接剥离；
    /// 2. 任意位置的 ```json ... ``` 代码块（前有自然语言段落也可）优先取围栏内内容；
    /// 3. 逐候选扫描：从每个 `{`/`[` 起始做括号配对，取第一个可解析的 JSON 段
    ///    （避免「输出格式」段落里的 `{keyword}` 等花括号干扰定位）；
    /// 4. 候选段解析失败时尝试修复未转义引号；最终兜底返回原响应。
    fn clean_json_response(&self, response: &str) -> String {
        let trimmed = response.trim();

        // 1. 整体围栏（```json 或 ``` 开头且 ``` 结尾）直接剥离
        if trimmed.starts_with("```json") && trimmed.ends_with("```") {
            return trimmed[7..trimmed.len() - 3].trim().to_string();
        }
        if trimmed.starts_with("```") && trimmed.ends_with("```") {
            return trimmed[3..trimmed.len() - 3].trim().to_string();
        }

        // 2. 任意位置的 ```json ... ``` 代码块（前有自然语言段落也可）优先取围栏内内容
        if let Some(block_start) = trimmed.find("```json") {
            let content_start = block_start + "```json".len();
            if let Some(rel_end) = trimmed[content_start..].find("```") {
                let inner = trimmed[content_start..content_start + rel_end].trim();
                if serde_json::from_str::<serde_json::Value>(inner).is_ok() {
                    return inner.to_string();
                }
            }
        }

        // 3. 逐候选扫描：从每个 `{`/`[` 起始括号配对（正确跳过字符串内字符），
        //    取第一个可解析的 JSON 段 —— 规避 `{keyword}` 这类段落花括号干扰定位
        let chars: Vec<char> = trimmed.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let open = chars[i];
            if open == '{' || open == '[' {
                let mut depth: i64 = 0;
                let mut in_string = false;
                let mut escaped = false;
                let mut j = i;
                while j < chars.len() {
                    let c = chars[j];
                    if in_string {
                        if escaped {
                            escaped = false;
                        } else if c == '\\' {
                            escaped = true;
                        } else if c == '"' {
                            in_string = false;
                        }
                    } else {
                        match c {
                            '"' => in_string = true,
                            '{' | '[' => depth += 1,
                            '}' | ']' => {
                                depth -= 1;
                                if depth == 0 {
                                    let candidate: String = chars[i..=j].iter().collect();
                                    if serde_json::from_str::<serde_json::Value>(&candidate).is_ok()
                                    {
                                        return candidate;
                                    }
                                    // 配对闭合但非合法 JSON，尝试修复未转义引号后重试
                                    let repaired = Self::repair_json_string_quotes(&candidate);
                                    if repaired != candidate
                                        && serde_json::from_str::<serde_json::Value>(&repaired)
                                            .is_ok()
                                    {
                                        tracing::warn!("JSON 已自动修复未转义引号");
                                        return repaired;
                                    }
                                    break; // 该候选不可用 → 继续找下一个起始位置
                                }
                            }
                            _ => {}
                        }
                    }
                    j += 1;
                }
            }
            i += 1;
        }

        // 4. 如果以上都不匹配，返回原始响应
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
