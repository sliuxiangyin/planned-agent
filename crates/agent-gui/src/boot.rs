//! 统一启动门：一次性初始化全部服务，全部成功后才以纯 `Arc` 形态注入 UI。
//!
//! 相比旧版「每个服务一个 `Resource<Option<Arc<T>>>`」：
//! - 消除消费端 `use_context` 后的层层 `Option` / `Resource` 解包判断；
//! - 启动阶段只渲染 Splash；任一「必需模块」失败则整页报错 + 重试；
//! - 就绪后由 `main.rs::ReadyShell` 同步调用 `use_context_provider` 注入干净 `Arc`。

use std::sync::Arc;

use crate::config::GuiConfig;

use crate::context::{
    AiContext, KvContext, McpContext, PromptContext, RagContext, StorageContext, ToolsContext,
};
use crate::shared::OnProgress;

/// 就绪后持有全部服务实例的句柄。
///
/// 除 `rag` 为可选外，其余字段在就绪时必定存在。
/// 由 [`bootstrap`] 全部成功后构造；`main.rs::ReadyShell` 据此注入 context。
pub struct ReadyServices {
    pub ai: Arc<AiContext>,
    pub prompt: Arc<PromptContext>,
    pub kv: Arc<KvContext>,
    pub mcp: Arc<McpContext>,
    pub tools: Arc<ToolsContext>,
    /// RAG 可选：未配置 embedding key 或初始化失败时为 `None`（不阻塞启动）。
    /// 因 RAG 目前无 UI 消费端，就绪后不注入 context，仅保留实例备用。
    pub rag: Option<Arc<RagContext>>,
    pub storage: Arc<StorageContext>,
}

/// 顺序执行全部服务初始化，每完成一个回调 `on_progress(模块名)`。
///
/// 依赖关系：
/// - `kv` 必须先于 `mcp`（mcp 需要 kv 作为后端存储）；
/// - `tools` 构造不依赖 mcp，但 MCP 工具需在 mcp 完成后由
///   `tools.set_mcp_manager(...)` 统一注册进 ToolRegistry（唯一入口）。
///
/// 失败策略：
/// - `rag` 为**可选模块**：失败不阻塞，返回 `rag: None`；
/// - 其余模块任一失败 → 短路返回 `Err`（失败模块名 + 错误），交由 UI 展示错误页。
pub async fn bootstrap(
    config: &GuiConfig,
    on_progress: OnProgress,
) -> Result<ReadyServices, Vec<(String, String)>> {
    // 1. AI 管理器（同步）
    on_progress("ai");
    let ai = AiContext::init(&config.ai_providers).map_err(|e| {
        vec![("AI 管理器".to_string(), e.to_string())]
    })?;

    // 2. Prompt 管理器（异步）
    on_progress("prompt");
    let prompt = PromptContext::init(&config.prompt_manager)
        .await
        .map_err(|e| vec![("Prompt 管理器".to_string(), e.to_string())])?;

    // 3. KV 缓存（异步，必须先于 mcp）
    on_progress("kv");
    let kv = Arc::new(
        KvContext::init(&config.cache)
            .await
            .map_err(|e| vec![("KV 缓存".to_string(), e.to_string())])?,
    );

    // 4. MCP 管理器（异步，依赖 kv）
    on_progress("mcp");
    let mcp = Arc::new(
        McpContext::init(Some(kv.clone()))
            .await
            .map_err(|e| vec![("MCP 管理器".to_string(), e.to_string())])?,
    );

    // 5. Tool Registry（同步；MCP 延后注入）
    on_progress("tools");
    let docs_dir = config.prompt_manager.prompt_dir.join("docs");
    let tools = ToolsContext::init(docs_dir)
        .map_err(|e| vec![("工具注册中心".to_string(), e.to_string())])?;

    // 把 MCP 工具统一注册进 ToolRegistry（唯一入口，避免重复注册）
    tools.set_mcp_manager(mcp.manager.clone());

    // 6. Storage（异步：SQLite + 迁移 + Repos）
    on_progress("storage");
    let storage = StorageContext::init(&config.storage)
        .await
        .map_err(|e| vec![("Storage 数据库".to_string(), e.to_string())])?;

    // 7. RAG（异步，可选：失败不阻塞）——放到必需模块之后，避免可选 init 阻塞就绪
    on_progress("rag");
    let rag = match RagContext::init(&config.rag).await {
        Ok(ctx) => Some(Arc::new(ctx)),
        Err(e) => {
            tracing::warn!("RAG 未启用（可选模块，不阻塞启动）: {}", e);
            None
        }
    };

    Ok(ReadyServices {
        ai: Arc::new(ai),
        prompt: Arc::new(prompt),
        kv,
        mcp,
        tools: Arc::new(tools),
        rag,
        storage: Arc::new(storage),
    })
}
