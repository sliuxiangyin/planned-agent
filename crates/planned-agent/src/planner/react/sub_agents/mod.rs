//! Sub-Agent 集合
//!
//! 每个 sub_agent 都是独立的、自包含的子智能体，与主 ReAct 上下文隔离：
//! - 调用方传入的是 `obs` / `intent` 等窄输入
//! - sub_agent 内部的 LLM 调用使用独立的 `AiClient` + `PromptManager`
//! - 主 ReAct 主链不会暴露到 sub_agent 的 prompt 里
//!
//! 当前成员：
//! - [`html_clean_subagent`] —— Browser 工具返回的 HTML 结构化清洗
//!
//! 新增 sub_agent 流程：
//! 1. 在本目录新建 `xxx.rs`
//! 2. 在本 `mod.rs` 加 `pub mod xxx;`
//! 3. 调用方按需注入依赖

pub mod html_clean_subagent;
