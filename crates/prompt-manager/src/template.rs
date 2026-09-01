use tera::{Tera, Context};
use anyhow::{Result, Context as AnyhowContext};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

/// Tera模板引擎封装
pub struct TemplateEngine {
    tera: Tera,
}

impl TemplateEngine {
    /// 创建新的模板引擎
    pub fn new() -> Self {
        Self {
            tera: Tera::default(),
        }
    }
    
    /// 添加模板
    pub fn add_template(&mut self, name: &str, content: &str) -> Result<()> {
        self.tera.add_raw_template(name, content)
            .context(format!("Failed to add template: {}", name))?;
        debug!("Added template: {}", name);
        Ok(())
    }
    
    /// 渲染模板
    pub fn render(&self, name: &str, variables: &HashMap<String, Value>) -> Result<String> {
        let mut context = Context::new();
        
        // 添加所有变量到上下文
        for (key, value) in variables {
            context.insert(key, value);
        }
        
        let result = self.tera.render(name, &context)
            .context(format!("Failed to render template: {}", name))?;
        
        Ok(result)
    }
    
    /// 渲染模板（带输出约束）
    pub fn render_with_constraints(
        &self, 
        name: &str, 
        variables: &HashMap<String, Value>,
        constraints: Option<&str>
    ) -> Result<String> {
        let mut rendered = self.render(name, variables)?;
        
        // 如果有输出约束，添加到渲染结果中
        if let Some(constraints) = constraints {
            rendered.push_str("\n\n");
            rendered.push_str(constraints);
        }
        
        Ok(rendered)
    }
    
    /// 检查模板是否存在
    pub fn has_template(&self, name: &str) -> bool {
        self.tera.get_template(name).is_ok()
    }
    
    /// 获取所有模板名称
    pub fn get_template_names(&self) -> Vec<String> {
        self.tera.get_template_names()
            .map(|name| name.to_string())
            .collect()
    }
    
    /// 重新加载所有模板
    pub fn reload(&mut self) -> Result<()> {
        // 注意：Tera的reload需要从文件系统重新加载
        // 这里我们只是清空模板，实际实现需要重新加载文件
        debug!("Template engine reload requested");
        Ok(())
    }
}

impl Default for TemplateEngine {
    fn default() -> Self {
        Self::new()
    }
}

