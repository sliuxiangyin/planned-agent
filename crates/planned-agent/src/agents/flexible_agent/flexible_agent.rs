//! 灵活模式主编排 Agent —— chat messages 模式。
//!
//! ## 设计
//!
//! 提供类似聊天页面的体验：一次 `chat()` 调用处理全部流程（需求检测 → 执行 → 输出 → 输入）。
//! 内部自动编排步骤，对外呈现为单次对话交互。
//!
//! ## 执行拦截
//!
//! 全局 system prompt 要求 AI 在需求明确后输出 `[TRIGGER_EXECUTE]` 标记。
//! `chat()` 在流式文本中检测到该标记时：
//! 1. 移除标记（不展示给用户）
//! 2. 运行 FlexibleExecuteAgent（子对话，全量工具，结果压缩）
//! 3. 将压缩结果注入消息历史
//! 4. 继续对话（AI 看到结果后进入输出/输入步骤）

use std::sync::Arc;

use anyhow::Result;

use planned_agent_core::prompt::{PromptContext, PromptManager};
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

use crate::chat::{ChatEvent, ChatService, PendingUIAction, SubAgentChatEvent};
use super::types::CompletenessDoc;
use super::flexible_execute_agent::FlexibleExecuteAgent;

// ── 公开类型 ──────────────────────────────────────────────────────────

/// 一次 chat 调用的结果。
#[derive(Debug, Clone)]
pub struct ChatResult {
    /// 是否被取消
    pub cancelled: bool,
    /// 待处理 UI 操作（非空 = 需要用户操作）
    pub pending: Vec<PendingUIAction>,
    /// 完整度文档是否已变更
    pub completeness_changed: bool,
    /// 累积消息历史（供 GUI 同步显示）
    pub history: Vec<Message>,
}

// ── Agent ──────────────────────────────────────────────────────────────

/// 灵活模式编排 Agent（chat messages 模式）。
#[derive(Clone)]
pub struct FlexibleAgent<PM: PromptManager + Send + Sync + 'static> {
    chat_service: ChatService<PM>,
    #[allow(dead_code)]
    prompt_manager: Arc<PM>,
    execute_agent: FlexibleExecuteAgent<PM>,
    /// 累积全局消息历史（[0] = 全局 system prompt）
    messages: Vec<Message>,
    /// 完整度文档
    completeness: CompletenessDoc,
    /// 是否已执行（避免重复 execute）
    executed: bool,
}

impl<PM: PromptManager + Send + Sync + 'static> FlexibleAgent<PM> {
    /// 创建 FlexibleAgent 并初始化全局上下文。
    pub async fn new(
        chat_service: ChatService<PM>,
        prompt_manager: Arc<PM>,
        execute_agent: FlexibleExecuteAgent<PM>,
        user_requirement: &str,
        historical_context: Option<&str>,
    ) -> Result<Self> {
        let global_prompt = prompt_manager
            .render("flexible/flexible_global_system", &PromptContext::new())
            .await
            .map_err(|e| anyhow::anyhow!("渲染全局 system prompt 失败: {}", e))?;

        let mut messages = vec![Message {
            role: MessageRole::System,
            content: Some(MessageContent::Text { text: global_prompt }),
            tool_calls: None, tool_call_id: None, name: None, reasoning_content: None,
            ..Default::default()
        }];

        let mut user_text = user_requirement.to_string();
        if let Some(ctx) = historical_context {
            if !ctx.is_empty() {
                user_text.push_str(&format!("\n\n## 历史计划上下文\n{}", ctx));
            }
        }
        messages.push(Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text { text: user_text }),
            tool_calls: None, tool_call_id: None, name: None, reasoning_content: None,
            ..Default::default()
        });

        let mut agent = Self {
            chat_service,
            prompt_manager,
            execute_agent,
            messages,
            completeness: CompletenessDoc::default(),
            executed: false,
        };
        agent.append_completeness_to_messages();
        Ok(agent)
    }

    // ── 访问器 ──

    pub fn completeness(&self) -> &CompletenessDoc {
        &self.completeness
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// 将用户选择内联到消息历史的最后一条 Assistant 消息中。
    pub fn inline_user_choice(&mut self, choice: &str) {
        if let Some(last) = self.messages.last_mut() {
            if matches!(last.role, MessageRole::Assistant) {
                if let Some(MessageContent::Text { text }) = &mut last.content {
                    text.push_str(&format!("\n\n---\n\n**{}**\n\n", choice));
                }
            }
        }
    }

    // ── 增量恢复（从持久化快照重建） ──

    /// 从持久化快照恢复 Agent 状态（用于从 DB 加载历史计划后继续对话）。
    pub async fn from_snapshot(
        chat_service: ChatService<PM>,
        prompt_manager: Arc<PM>,
        execute_agent: FlexibleExecuteAgent<PM>,
        completeness: CompletenessDoc,
        executed: bool,
    ) -> Result<Self> {
        let global_prompt = prompt_manager
            .render("flexible/flexible_global_system", &PromptContext::new())
            .await
            .map_err(|e| anyhow::anyhow!("渲染全局 system prompt 失败: {}", e))?;

        let messages = vec![Message {
            role: MessageRole::System,
            content: Some(MessageContent::Text { text: global_prompt }),
            tool_calls: None, tool_call_id: None, name: None, reasoning_content: None,
            ..Default::default()
        }];

        let mut agent = Self {
            chat_service,
            prompt_manager,
            execute_agent,
            messages,
            completeness,
            executed,
        };
        agent.add_user_message("[恢复对话] 继续之前的工作。");
        agent.append_completeness_to_messages();
        Ok(agent)
    }

    // ── Chat 入口 ──

    /// 主 Chat 入口。
    ///
    /// 内部根据当前状态决定执行哪个步骤：
    /// - 需求未整理 → clarity check
    /// - 需求已整理但未执行 → 自动 execute
    /// - 已执行但输出未确认 → output suggest
    /// - 已执行但输入未识别 → param identify
    /// - 全部完成 → 自由对话（用户调整指令）
    ///
    /// `on_event` 接收 ChatEvent 用于流式展示。
    pub async fn chat<F>(
        &mut self,
        user_message: &str,
        mut on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatEvent) + Send,
    {
        if !user_message.trim().is_empty() {
            self.add_user_message(user_message);
        }

        // ── Step 1: 需求未整理 → clarity check ──
        if self.completeness.requirement.is_empty() {
            return self.chat_with_limited_tools(
                "[步骤 1/4：需求清晰度检测]\n请判断以上需求是否明确。若明确，整理需求并输出。",
                &mut on_event,
                true, // extract requirement
            ).await;
        }

        // ── Step 2: 未执行 → execute ──
        if !self.executed {
            return self.run_execute_and_continue(&mut on_event).await;
        }

        // ── Step 3: 输出未确认 → output suggest ──
        if self.completeness.output_schema.is_empty() {
            return self.chat_with_limited_tools(
                "[步骤 3/4：确认输出类型]\n请根据以上结果推断输出数据类型，并与用户确认。",
                &mut on_event,
                false,
            ).await.map(|mut r| {
                if r.pending.is_empty() {
                    self.completeness.output_schema =
                        extract_last_assistant_text(&self.messages);
                    self.append_completeness_to_messages();
                    r.completeness_changed = true;
                }
                r
            });
        }

        // ── Step 4: 输入未识别 → param identify ──
        if self.completeness.input_params.is_empty() {
            return self.chat_with_limited_tools(
                "[步骤 4/4：识别输入参数]\n请识别可参数化的动态值，并与用户确认。",
                &mut on_event,
                false,
            ).await.map(|mut r| {
                if r.pending.is_empty() {
                    self.completeness.input_params =
                        extract_last_assistant_text(&self.messages);
                    self.append_completeness_to_messages();
                    r.completeness_changed = true;
                }
                r
            });
        }

        // ── 全部完成：自由对话（用户调整指令） ──
        self.chat_with_limited_tools("", &mut on_event, false).await.map(|mut r| {
            if r.pending.is_empty() {
                self.append_completeness_to_messages();
                r.completeness_changed = true;
            }
            r
        })
    }

    /// 恢复：用户对 pending UI 作出响应。
    ///
    /// 不需要额外的状态管理——消息历史已保留，追加用户选择文本即可。
    /// 注意：调用方需在调用前将用户选择内联到消息历史的最后一条 Assistant 消息中。
    pub async fn resume<F>(
        &mut self,
        mut on_event: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatEvent) + Send,
    {
        // 使用与当前状态匹配的工具白名单
        let needs_execute = !self.completeness.requirement.is_empty() && !self.executed;

        if needs_execute {
            return self.run_execute_and_continue(&mut on_event).await;
        }

        self.chat_with_limited_tools("", &mut on_event, false).await.map(|mut r| {
            if r.pending.is_empty() {
                // 尝试从回复中提取字段更新
                self.try_update_completeness_from_messages();
                self.append_completeness_to_messages();
                r.completeness_changed = true;
            }
            r
        })
    }

    // ── 内部方法 ──

    /// 用受限工具（request_user_action + builtin_read_documentation）进行一次 chat。
    async fn chat_with_limited_tools<F>(
        &mut self,
        instruction: &str,
        on_event: &mut F,
        extract_requirement_after: bool,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatEvent) + Send,
    {
        if !instruction.is_empty() {
            self.add_user_message(instruction);
        }

        // TODO(重构 service.rs): with_system_prompt_template 语义可能调整，调用方需同步。
        let svc = self.chat_service
            .with_system_prompt_template(None)
            // TODO(重构 service.rs): with_allowed_tools 语义可能调整，调用方需同步。
            .with_allowed_tools(Some(vec![
                "request_user_action".to_string(),
                "builtin_read_documentation".to_string(),
            ]));

        // TODO(重构 service.rs): chat_with_callback 签名/返回可能变化（详见 batch_id + HistoryStore 重构方案），调用方需同步。
        let result = svc.chat_with_callback(self.messages.clone(), |ev| on_event(ev), None::<fn(SubAgentChatEvent)>).await?;
        self.messages = result.history;

        if result.cancelled {
            return Ok(ChatResult { cancelled: true, pending: vec![], completeness_changed: false, history: self.messages.clone() });
        }

        if !result.pending_ui_actions.is_empty() {
            return Ok(ChatResult { cancelled: false, pending: result.pending_ui_actions, completeness_changed: false, history: self.messages.clone() });
        }

        // 提取需求
        if extract_requirement_after {
            let text = extract_last_assistant_text(&self.messages);
            self.completeness.requirement = extract_requirement(&text);
            self.append_completeness_to_messages();
        }

        Ok(ChatResult { cancelled: false, pending: vec![], completeness_changed: true, history: self.messages.clone() })
    }

    /// 运行 execute_agent，注入结果，继续对话。
    async fn run_execute_and_continue<F>(
        &mut self,
        on_event: &mut F,
    ) -> Result<ChatResult>
    where
        F: FnMut(ChatEvent) + Send,
    {
        // 注入"正在执行"提示
        self.add_assistant_message("⚡ 正在执行任务…");

        // 运行子 agent
        let exec_output = self.execute_agent
            .execute(&self.completeness, |ev| on_event(ev))
            .await?;

        // 更新完整度文档
        self.completeness.execution_steps = exec_output.key_steps.join("\n");
        self.completeness.tool_paths = exec_output.tool_steps.join("\n");
        self.executed = true;

        // 注入压缩结果
        let exec_summary = format!(
            "[步骤 2/4：执行完成]\n\n执行结果:\n{}\n\n关键步骤:\n{}",
            exec_output.execution_result,
            exec_output.key_steps.iter().enumerate()
                .map(|(i, s)| format!("{}. {}", i + 1, s))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        self.add_assistant_message(exec_summary);
        self.append_completeness_to_messages();

        // 继续：走 output suggest
        self.chat_with_limited_tools(
            "[步骤 3/4：确认输出类型]\n请根据以上结果推断输出数据类型，并与用户确认。",
            on_event,
            false,
        ).await.map(|mut r| {
            if r.pending.is_empty() {
                self.completeness.output_schema = extract_last_assistant_text(&self.messages);
                self.append_completeness_to_messages();
                r.completeness_changed = true;
            }
            r
        })
    }

    /// 尝试从消息历史的最新 Assistant 回复中提取完整度文档字段更新。
    fn try_update_completeness_from_messages(&mut self) {
        let text = extract_last_assistant_text(&self.messages);
        if text.is_empty() { return; }

        // 尝试匹配各字段
        let markers = [
            ("## 需求描述", 0usize),
            ("## 输入参数",  1),
            ("## 执行步骤",  2),
            ("## 工具路径",  3),
            ("## 输出格式",  4),
        ];
        for (marker, field_idx) in &markers {
            if let Some(idx) = text.find(marker) {
                let after = &text[idx + marker.len()..];
                let value = after.split("\n##").next().unwrap_or(after).trim();
                if !value.is_empty() && value != "（待填充）" {
                    self.set_field(*field_idx, value);
                }
            }
        }
    }

    fn set_field(&mut self, idx: usize, value: &str) {
        match idx {
            0 => self.completeness.requirement = value.to_string(),
            1 => self.completeness.input_params = value.to_string(),
            2 => self.completeness.execution_steps = value.to_string(),
            3 => self.completeness.tool_paths = value.to_string(),
            4 => self.completeness.output_schema = value.to_string(),
            _ => {}
        }
    }

    // ── 辅助 ──

    fn add_user_message(&mut self, text: impl Into<String>) {
        self.messages.push(Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text { text: text.into() }),
            tool_calls: None, tool_call_id: None, name: None, reasoning_content: None,
            ..Default::default()
        });
    }

    fn add_assistant_message(&mut self, text: impl Into<String>) {
        self.messages.push(Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text { text: text.into() }),
            tool_calls: None, tool_call_id: None, name: None, reasoning_content: None,
            ..Default::default()
        });
    }

    fn append_completeness_to_messages(&mut self) {
        let md = format!("[完整度文档]\n{}", self.completeness.to_markdown());
        self.add_assistant_message(md);
    }
}

// ── 辅助函数 ──

fn extract_last_assistant_text(history: &[Message]) -> String {
    history.iter().rev()
        .find(|m| matches!(m.role, MessageRole::Assistant))
        .and_then(|m| match &m.content {
            Some(MessageContent::Text { text }) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn extract_requirement(output: &str) -> String {
    const MARK: &str = "## 整理后的需求";
    output.find(MARK)
        .map(|idx| output[idx + MARK.len()..].trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| output.to_string())
}
