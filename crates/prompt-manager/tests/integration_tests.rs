use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::json;

use planned_agent_core::prompt::{PromptManager, PromptContext};
use planned_agent_prompt_manager::{FilePromptManager, PromptManagerConfig};

fn get_test_config() -> PromptManagerConfig {
    // 获取当前文件的目录
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // 从crate目录向上两级到达项目根目录
    let project_root = manifest_dir.parent().unwrap().parent().unwrap();
    let prompt_dir = project_root.join("prompts");
    
    PromptManagerConfig {
        prompt_dir,
        ..Default::default()
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct ExtractedInfo {
    entities: Vec<String>,
    summary: String,
    sentiment: String,
}

#[tokio::test]
async fn test_prompt_manager_initialization() {
    let config = get_test_config();
    
    let manager = FilePromptManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    
    // 检查是否加载了prompt
    let prompts = manager.list_prompts().await.unwrap();
    assert!(!prompts.is_empty());
    println!("Loaded {} prompts", prompts.len());
}

#[tokio::test]
async fn test_prompt_exists() {
    let config = get_test_config();
    
    let manager = FilePromptManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    
    // 检查chat/system是否存在
    assert!(manager.exists("chat/system").await.unwrap());
    
    // 检查不存在的prompt
    assert!(!manager.exists("nonexistent/prompt").await.unwrap());
}

#[tokio::test]
async fn test_prompt_rendering() {
    let config = get_test_config();
    
    let manager = FilePromptManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    
    // 渲染chat/system prompt
    let context = PromptContext::new()
        .with_variable("user_name", json!("张三"))
        .with_variable("context", json!("这是一个测试"));
    
    let rendered = manager.render("chat/system", &context).await.unwrap();
    println!("Rendered prompt:\n{}", rendered);
    assert!(!rendered.is_empty());
}

#[tokio::test]
async fn test_prompt_with_output_schema() {
    let config = get_test_config();
    
    let manager = FilePromptManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    
    // 检查analysis/extract_info是否有output_schema
    let schema = manager.get_output_schema("analysis/extract_info").await.unwrap();
    assert!(schema.is_some());
    println!("Output schema: {:?}", schema);
}

#[tokio::test]
async fn test_response_validation() {
    let config = get_test_config();
    
    let manager = FilePromptManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    
    // 测试有效的JSON响应
    let valid_response = r#"
    {
        "entities": ["张三", "北京"],
        "summary": "张三在北京",
        "sentiment": "neutral"
    }
    "#;
    
    let is_valid = manager.validate_response("analysis/extract_info", valid_response).await.unwrap();
    assert!(is_valid);
    
    // 测试无效的JSON响应（缺少必需字段）
    let invalid_response = r#"
    {
        "entities": ["张三"],
        "summary": "张三在北京"
    }
    "#;
    
    let is_valid = manager.validate_response("analysis/extract_info", invalid_response).await.unwrap();
    assert!(!is_valid);
}

#[tokio::test]
async fn test_response_parsing() {
    let config = get_test_config();
    
    let manager = FilePromptManager::new(config).unwrap();
    manager.initialize().await.unwrap();
    
    // 测试解析JSON响应
    let response = r#"
    {
        "entities": ["张三", "北京", "公司"],
        "summary": "张三在北京的公司工作",
        "sentiment": "neutral"
    }
    "#;
    
    let extracted: ExtractedInfo = manager.parse_response("analysis/extract_info", response).await.unwrap();
    assert_eq!(extracted.entities.len(), 3);
    assert_eq!(extracted.summary, "张三在北京的公司工作");
    assert_eq!(extracted.sentiment, "neutral");
}

#[tokio::test]
async fn test_prompt_list() {
    let config = get_test_config();

    let manager = FilePromptManager::new(config).unwrap();
    manager.initialize().await.unwrap();

    let prompts = manager.list_prompts().await.unwrap();
    println!("Available prompts:");
    for prompt in &prompts {
        println!("  - {} (has schema: {})", prompt.name, prompt.has_output_schema);
    }

    // 应该至少有3个prompt
    assert!(prompts.len() >= 3);
}

#[tokio::test]
async fn test_coarse_plan_prompt_has_entity_preservation_rules() {
    let config = get_test_config();

    let manager = FilePromptManager::new(config).unwrap();
    manager.initialize().await.unwrap();

    // 渲染粗粒度计划 Prompt，验证 user_input 原样透传
    let context = PromptContext::new()
        .with_variable("user_input", json!("打开百度，搜索安仁乡，给出前三条相关信息并整理给我"))
        .with_variable("context", json!("无历史上下文"))
        .with_variable("available_categories", json!("- Browser（浏览器）\n- File（文件）"));

    let rendered = manager
        .render("planning/coarse_plan", &context)
        .await
        .unwrap();

    // 用户原始输入必须原样出现在渲染结果中
    assert!(
        rendered.contains("打开百度，搜索安仁乡，给出前三条相关信息并整理给我"),
        "渲染结果必须原样保留用户输入：\n{}",
        rendered
    );
    // 必须包含实体保留强约束
    assert!(
        rendered.contains("用户实体保留约束"),
        "粗粒度计划 Prompt 必须包含用户实体保留约束：\n{}",
        rendered
    );
    assert!(
        rendered.contains("安仁乡"),
        "Prompt 必须保留示例中的关键地名：\n{}",
        rendered
    );
    // 必须显式禁止占位符替代
    assert!(
        rendered.contains("禁止使用") && rendered.contains("占位符"),
        "Prompt 必须显式禁止占位符：\n{}",
        rendered
    );
}
