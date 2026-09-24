//! 灵活计划的**执行服务**（内核）：常驻命令循环 + 状态表 + 订阅，**零依赖接缝**。
//!
//! 需求映射：
//! - 长驻服务 → [`RunServiceCore::run`]（宿主用 dioxus `spawn_forever` 驱动）
//! - 订阅 / 卸载 → [`RunService::subscribe`] / [`RunService::unsubscribe`]
//! - 按会话查进度 → [`RunService::snapshot`]
//! - 启动会话 → [`RunService::start`]，输入是自包含的 [`RunRequest`]
//!
//! 边界：**不依赖 dioxus、不依赖 storage，也不定义依赖接缝**。
//! 「会话有没有定稿模板」「有没有配 AI 客户端」由宿主解析（见
//! `docs/planned-agent/flexible-run-service.md` §12），内核只收可执行的四样
//! （模板 / 参数 / 客户端 / 执行配置）。
//!
//! ```text
//! run_service/
//! ├── mod.rs      门面：子模块 + 对外导出 + 组装函数
//! ├── types.rs    命令 / 请求 / 快照 / 订阅（纯数据）
//! ├── state.rs    事件 → 快照的归并（纯函数）
//! ├── store.rs    状态表 + 订阅登记 + 取消通道
//! ├── core.rs     常驻命令循环 + 事件 sink
//! └── service.rs  宿主句柄（命令入队 / 查询 / 订阅）
//! ```

mod core;
mod service;
mod state;
mod store;
mod types;

pub use core::RunServiceCore;
pub use service::RunService;
pub use state::apply_event;
pub use store::RunStore;
pub use types::{
    RunCommand, RunRequest, RunSnapshot, RunStatus, RunUpdate, SessionFilter, SessionId, StepPhase,
    StepSnapshot, StepTrackLine, SubscriptionId,
};

use std::sync::Arc;

use planned_agent_tool_manager::ToolRegistry;

/// 组装服务：返回「宿主句柄 + 待驱动的内核」。
///
/// 宿主的接法（见设计稿 §6.1）：
/// ```ignore
/// let (service, core) = new_run_service(store, tools);
/// use_context_provider(move || service);
/// spawn_forever(core.run());
/// ```
pub fn new_run_service(
    store: Arc<RunStore>,
    tools: Arc<ToolRegistry>,
) -> (Arc<RunService>, RunServiceCore) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RunCommand>();
    let service = Arc::new(RunService::new(tx.clone(), store.clone()));
    let core = RunServiceCore::new(store, tools, tx, rx);
    (service, core)
}
