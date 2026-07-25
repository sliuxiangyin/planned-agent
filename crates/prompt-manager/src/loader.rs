use std::path::{Path, PathBuf};
use std::collections::HashMap;
use anyhow::{Result, Context};
use serde_json::{json, Value};
use walkdir::WalkDir;
use tracing::warn;
use planned_agent_core::prompt::{PromptTemplate, PromptMetadata, PromptVariable, OutputSchema, OutputFormat};

/// 文件系统加载器
pub struct FileLoader {
    prompt_dir: PathBuf,
}

impl FileLoader {
    /// 创建新的文件加载器
    pub fn new(prompt_dir: PathBuf) -> Self {
        Self { prompt_dir }
    }
    
    /// 加载所有prompt模板
    pub fn load_all(&self) -> Result<HashMap<String, PromptTemplate>> {
        let mut prompts = HashMap::new();
        
        if !self.prompt_dir.exists() {
            warn!("Prompt directory does not exist: {:?}", self.prompt_dir);
            return Ok(prompts);
        }
        
        println!("Loading prompts from: {:?}", self.prompt_dir);
        
        for entry in WalkDir::new(&self.prompt_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            let relative_path = path.strip_prefix(&self.prompt_dir)
                .context("Failed to strip prefix")?;
            
            // 生成prompt名称（去掉扩展名，使用/作为分隔符）
            let name = self.path_to_name(relative_path);
            
            println!("Processing file: {:?} -> name: {}", path, name);
            
            match self.load_file(path, &name) {
                Ok(template) => {
                    println!("Successfully loaded prompt: {}", name);
                    prompts.insert(name, template);
                }
                Err(e) => {
                    println!("Failed to load prompt {:?}: {}", path, e);
                    warn!("Failed to load prompt {:?}: {}", path, e);
                }
            }
        }
        
        println!("Loaded {} prompts", prompts.len());
        Ok(prompts)
    }
    
    /// 加载单个prompt文件
    fn load_file(&self, path: &Path, name: &str) -> Result<PromptTemplate> {
        let extension = path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        
        match extension {
            "toml" => self.load_toml(path, name),
            "txt" => self.load_text(path, name),
            "md" => self.load_markdown(path, name),
            _ => {
                warn!("Unsupported file format: {}", extension);
                Err(anyhow::anyhow!("Unsupported file format: {}", extension))
            }
        }
    }
    
    /// 加载TOML格式的prompt
    fn load_toml(&self, path: &Path, name: &str) -> Result<PromptTemplate> {
        let content = std::fs::read_to_string(path)
            .context(format!("Failed to read file: {:?}", path))?;
        
        let toml_value: toml::Value = toml::from_str(&content)
            .context(format!("Failed to parse TOML: {:?}", path))?;
        
        // 解析name部分
        let description = toml_value.get("name")
            .and_then(|n| n.get("description"))
            .and_then(|d| d.as_str())
            .map(|s| s.to_string());
        
        // 解析content部分
        let text = toml_value.get("content")
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .context("Missing content.text field")?
            .to_string();
        
        // 解析variables部分
        let variables = self.parse_variables(&toml_value);
        
        // 解析output_schema部分
        let output_schema = self.parse_output_schema(&toml_value);
        
        Ok(PromptTemplate {
            name: name.to_string(),
            content: text,
            metadata: PromptMetadata {
                description,
                version: None,
                variables,
                tags: Vec::new(),
            },
            output_schema,
        })
    }
    
    /// 解析变量定义
    fn parse_variables(&self, toml_value: &toml::Value) -> Vec<PromptVariable> {
        let mut variables = Vec::new();
        
        if let Some(vars) = toml_value.get("variables").and_then(|v| v.as_table()) {
            for (name, value) in vars {
                let description = value.get("description")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string());
                
                let required = value.get("required")
                    .and_then(|r| r.as_bool())
                    .unwrap_or(false);
                
                let default_value = value.get("default_value")
                    .map(|v| serde_json::to_value(v).unwrap_or_default());
                
                variables.push(PromptVariable {
                    name: name.clone(),
                    description,
                    required,
                    default_value,
                });
            }
        }
        
        variables
    }
    
    /// 解析输出schema定义
    fn parse_output_schema(&self, toml_value: &toml::Value) -> Option<OutputSchema> {
        let schema = toml_value.get("output_schema")?;
        
        let format_str = schema.get("format")
            .and_then(|f| f.as_str())
            .unwrap_or("text");
        
        let format = match format_str {
            "json" => OutputFormat::Json,
            "text" => OutputFormat::Text,
            "markdown" => OutputFormat::Markdown,
            "yaml" => OutputFormat::Yaml,
            "xml" => OutputFormat::Xml,
            _ => OutputFormat::Text,
        };
        
        let json_schema = schema.get("json_schema")
            .map(|s| serde_json::to_value(s).unwrap_or_default());
        
        // 如果没有定义example，从json_schema自动生成
        let example = schema.get("example")
            .and_then(|e| e.get("text"))
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                if format == OutputFormat::Json {
                    json_schema.as_ref().map(|s| self.generate_example_from_schema(s))
                } else {
                    None
                }
            });
        
        // 如果没有定义constraints，根据format自动生成
        let constraints = schema.get("constraints")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .or_else(|| Some(self.generate_default_constraints(&format)));
        
        Some(OutputSchema {
            format,
            json_schema,
            example,
            constraints,
        })
    }
    
    /// 从JSON Schema生成示例
    fn generate_example_from_schema(&self, schema: &Value) -> String {
        let mut example = serde_json::Map::new();
        
        if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
            for (key, prop) in properties {
                let value = self.generate_example_value(prop);
                example.insert(key.clone(), value);
            }
        }
        
        serde_json::to_string_pretty(&Value::Object(example)).unwrap_or_else(|_| "{}".to_string())
    }
    
    /// 根据属性定义生成示例值
    fn generate_example_value(&self, prop: &Value) -> Value {
        let prop_type = prop.get("type").and_then(|t| t.as_str()).unwrap_or("string");
        
        match prop_type {
            "string" => {
                if let Some(enum_values) = prop.get("enum").and_then(|e| e.as_array()) {
                    enum_values.first().cloned().unwrap_or(json!("example"))
                } else {
                    json!("example")
                }
            }
            "number" | "integer" => json!(0),
            "boolean" => json!(true),
            "array" => {
                if let Some(items) = prop.get("items") {
                    let item_value = self.generate_example_value(items);
                    json!([item_value])
                } else {
                    json!([])
                }
            }
            "object" => {
                if let Some(properties) = prop.get("properties").and_then(|p| p.as_object()) {
                    let mut obj = serde_json::Map::new();
                    for (key, sub_prop) in properties {
                        obj.insert(key.clone(), self.generate_example_value(sub_prop));
                    }
                    Value::Object(obj)
                } else {
                    json!({})
                }
            }
            _ => json!(null),
        }
    }
    
    /// 根据格式生成默认约束
    fn generate_default_constraints(&self, format: &OutputFormat) -> String {
        match format {
            OutputFormat::Json => "请确保返回有效的JSON格式，不要包含其他文本说明".to_string(),
            OutputFormat::Yaml => "请确保返回有效的YAML格式".to_string(),
            OutputFormat::Xml => "请确保返回有效的XML格式".to_string(),
            OutputFormat::Markdown => "请使用Markdown格式".to_string(),
            OutputFormat::Text => "请返回纯文本".to_string(),
        }
    }
    
    /// 加载纯文本格式的prompt
    fn load_text(&self, path: &Path, name: &str) -> Result<PromptTemplate> {
        let content = std::fs::read_to_string(path)
            .context(format!("Failed to read file: {:?}", path))?;
        
        Ok(PromptTemplate {
            name: name.to_string(),
            content,
            metadata: PromptMetadata {
                description: None,
                version: None,
                variables: Vec::new(),
                tags: Vec::new(),
            },
            output_schema: None,
        })
    }
    
    /// 加载Markdown格式的prompt
    fn load_markdown(&self, path: &Path, name: &str) -> Result<PromptTemplate> {
        let content = std::fs::read_to_string(path)
            .context(format!("Failed to read file: {:?}", path))?;
        
        Ok(PromptTemplate {
            name: name.to_string(),
            content,
            metadata: PromptMetadata {
                description: None,
                version: None,
                variables: Vec::new(),
                tags: Vec::new(),
            },
            output_schema: None,
        })
    }
    
    /// 将文件路径转换为prompt名称
    fn path_to_name(&self, path: &Path) -> String {
        path.components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/")
            .replace("\\", "/")
            .trim_end_matches(".toml")
            .trim_end_matches(".txt")
            .trim_end_matches(".md")
            .to_string()
    }
}
