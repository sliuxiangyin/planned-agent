//! ChatService 类型别名

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::ChatService;
use planned_agent_prompt_manager::FilePromptManager;

/// ChatService 缓存 Signal 的类型别名
///
/// 泛型实参固定为 `FilePromptManager`，与 hook 中 `ChatService::new(... pm, ...)`
/// 的 `pm: Arc<FilePromptManager>` 对齐。后续若要换其它 PromptManager 实现（如
/// 测试用 mock），需同步扩展本别名 + hook 的泛型实参。
pub(crate) type ChatServiceSignal =
    Signal<Option<Arc<ChatService<FilePromptManager>>>, SyncStorage>;