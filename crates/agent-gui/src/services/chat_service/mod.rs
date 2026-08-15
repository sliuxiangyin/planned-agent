//! ChatService 服务模块
//!
//! 把 `ai` + `tools` (+ `prompt`) 三个 context 组合成 `ChatService` 缓存 signal。
//!
//! 调用方在组件内写 `use_chat_service(ChatConfig{...})` 即可，
//! 不必关心底层 Resource 细节。
//!
//! - v1：`use_chat_service` / `ChatServiceSignal`（`pages/plan/*` 仍在使用）
//! - v2：`use_v2_chat_service` / `V2ChatServiceSignal`（`pages/chat/*` 使用）

mod hook;
mod types;

pub(crate) use hook::{use_chat_service, use_v2_chat_service};
pub(crate) use types::{ChatServiceSignal, V2ChatServiceSignal};