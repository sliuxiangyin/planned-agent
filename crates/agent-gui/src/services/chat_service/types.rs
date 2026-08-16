//! ChatService 类型别名

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::ChatService;
use planned_agent_prompt_manager::FilePromptManager;

/// ChatService 缓存 Signal 的类型别名。
pub(crate) type ChatServiceSignal =
    Signal<Option<Arc<ChatService<FilePromptManager>>>, SyncStorage>;
