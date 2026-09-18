//! 子 agent Runner：持有工厂参数，每次 `start()` 新建独立 `ChatService`。

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use planned_agent_ai_manager::AiManager;
use planned_agent_core::mcp::types::ToolResult;
use planned_agent_prompt_manager::FilePromptManager;
use planned_agent_tool_manager::{
    SubAgentRunOutcome, SubAgentSessionRunner, ToolRegistry, ToolStreamSender,
};
use serde_json::Value;
use tracing::{error, info, warn};

use crate::chat::service::{ChatConfig, ChatService};

use super::callback::{
    BeforeDecision, SubAgentBeforeCallback, SubAgentCallContext, SubAgentResultChain,
};
use super::collect::collect_until_outcome;

/// 子 agent runner：持有 `ChatService` 工厂参数，每次 `start()`
/// 新建独立 `ChatService`，完成即 drop，天然隔离且 driver loop
/// 随 `Arc<State>` 归零自动退出。
///
/// - `depth`：当前嵌套深度（0 = 顶层）
/// - `max_depth`：最大允许嵌套深度（防递归）
pub struct SubAgentRunner {
    ai_manager: AiManager,
    tool_registry: Arc<ToolRegistry>,
    prompt_manager: Arc<FilePromptManager>,
    /// 子 agent 专属配置（不含 run_id，run_id 每次调用时注入）
    config: ChatConfig,
    depth: u32,
    max_depth: u32,
    /// 结果链：前置分析 + 串行业务回调（见 `collect::run_chain`）
    result_chain: SubAgentResultChain,
    /// 启动前回调链：在 task 文本生成前按顺序注入系统侧数据（只碰入参，不碰产物）
    before_callbacks: Vec<Arc<dyn SubAgentBeforeCallback>>,
}

impl SubAgentRunner {
    pub fn new(
        ai_manager: AiManager,
        tool_registry: Arc<ToolRegistry>,
        prompt_manager: Arc<FilePromptManager>,
        config: ChatConfig,
        depth: u32,
        max_depth: u32,
        result_chain: SubAgentResultChain,
        before_callbacks: Vec<Arc<dyn SubAgentBeforeCallback>>,
    ) -> Self {
        Self {
            ai_manager,
            tool_registry,
            prompt_manager,
            config,
            depth,
            max_depth,
            result_chain,
            before_callbacks,
        }
    }
}

#[async_trait]
impl SubAgentSessionRunner for SubAgentRunner {
    async fn start(
        &self,
        arguments: Value,
        stream: ToolStreamSender,
    ) -> Result<SubAgentRunOutcome> {
        info!(
            "[子agent] start() 被调用, depth={}, max_depth={}",
            self.depth, self.max_depth
        );

        // 防递归
        if self.depth >= self.max_depth {
            info!("[子agent] 嵌套深度超限，拒绝执行");
            return Ok(SubAgentRunOutcome::Done(ToolResult {
                call_id: String::new(),
                is_error: true,
                content: Value::String(format!(
                    "子 agent 嵌套深度 {} 已达上限 {}",
                    self.depth, self.max_depth
                )),
            }));
        }

        // 提取 task 参数（剔除不进入 task 文本的控制字段，如宿主注入的会话标识）
        let mut task_arguments = strip_hidden_args(&arguments, &self.config.hidden_args);

        // ── 启动前回调链：在 task 文本生成前注入系统侧数据 ──
        // 注入只改写「发给子 agent 的参数」，不读也不写子 agent 的输出，
        // 因此与结果链（`result_chain`）互不影响。
        // 位置刻意放在 `strip_hidden_args` 之后：注入字段不受 hidden_args 剔除影响。
        if !self.before_callbacks.is_empty() {
            let ctx = SubAgentCallContext {
                agent_name: stream.tool_name().to_string(),
                tool_call_id: stream.invocation_id().to_string(),
                arguments: arguments.clone(),
            };
            for cb in &self.before_callbacks {
                match cb.before_start(&ctx).await {
                    BeforeDecision::Continue => {}
                    BeforeDecision::Inject(extra) => {
                        merge_into_object(&mut task_arguments, &extra, cb.name());
                    }
                    BeforeDecision::Abort(reason) => {
                        // before 阶段没有模型输出可重做，重试无意义：立即以失败结果收场。
                        error!("[子agent] before 回调 {} 中断本次调用：{}", cb.name(), reason);
                        return Ok(SubAgentRunOutcome::Done(ToolResult {
                            call_id: String::new(),
                            is_error: true,
                            content: Value::String(reason),
                        }));
                    }
                }
            }
        }

        let task = serde_json::to_string_pretty(&task_arguments)
            .unwrap_or_else(|_| "请完成指定任务".to_string());
        info!("[子agent] 准备发送任务: {}", task);

        // 每次调用新建独立 ChatService：run_id 在构造时写入 config，
        // 完成后 service 自动 drop，driver loop 随 Arc<State> 归零自然退出，
        // history / subscribers / config 天然隔离。
        let mut call_config = self.config.clone();
        call_config.run_id = Some(stream.invocation_id().to_string());
        let service = ChatService::new(
            self.ai_manager.clone(),
            self.tool_registry.clone(),
            self.prompt_manager.clone(),
            call_config,
        )
        .await
        .map_err(|e| {
            info!("[子agent] ChatService::new 失败: {}", e);
            e
        })?;
        // 挂接上游（父级）取消信号：父服务被取消时，本子 agent 级联取消。
        if let Some(upstream) = stream.upstream_cancel() {
            service.attach_upstream(upstream);
        }
        service.start_driver()?;

        // 发送任务给子 agent 的 ChatService
        let ticket = service.send_text(task).map_err(|e| {
            info!("[子agent] send_text 失败: {}", e);
            anyhow::anyhow!("{}", e)
        })?;
        info!("[子agent] send_text 成功，ticket 已返回，开始收集事件");

        // 收集事件直到完成或挂起
        // - Completed/Failed：service 随函数返回后 drop
        // - Suspended：service clone 移入 ChatSubAgentSession 持有
        collect_until_outcome(
            &service,
            ticket,
            &stream,
            self.depth,
            self.max_depth,
            self.result_chain.clone(),
            arguments,
        )
        .await
    }
}

/// 从父 agent 传入的 `arguments` 中剔除「不进入子 agent task 文本」的控制字段。
///
/// `hidden` 为空（默认）时原样返回，保持既有行为。
/// 把 `extra` 的**顶层字段合并**进 `base`：不同名各自保留，同名 key 由 `extra` 覆盖。
///
/// 只做顶层浅合并，不做深合并——同名 key **整块替换**。深合并会产出「半个对象来自
/// LLM、半个来自系统」的混合态，谁都预期不到；整块替换才是可预测的。
fn merge_into_object(base: &mut Value, extra: &Value, who: &str) {
    let Some(extra_map) = extra.as_object() else {
        warn!("[子agent] before 回调 {} 注入的不是对象，已忽略：{}", who, extra);
        return;
    };
    let Some(base_map) = base.as_object_mut() else {
        warn!("[子agent] before 回调 {} 注入失败：task 参数不是对象", who);
        return;
    };
    for (k, v) in extra_map {
        base_map.insert(k.clone(), v.clone());
    }
}

fn strip_hidden_args(args: &Value, hidden: &[String]) -> Value {
    if hidden.is_empty() {
        return args.clone();
    }
    match args {
        Value::Object(map) => {
            let mut map = map.clone();
            for key in hidden {
                map.remove(key);
            }
            Value::Object(map)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 走一遍真实的合并路径：把 `extra` 注入 `base`，返回合并结果。
    fn merge(base: Value, extra: Value) -> Value {
        let mut base = base;
        merge_into_object(&mut base, &extra, "test");
        base
    }

    #[test]
    fn inject_keeps_disjoint_fields() {
        let out = merge(
            json!({ "host_session_id": "s1", "output_format": "文本" }),
            json!({ "execution_trace": [1, 2] }),
        );
        assert_eq!(
            out,
            json!({ "host_session_id": "s1", "output_format": "文本", "execution_trace": [1, 2] })
        );
    }

    #[test]
    fn inject_overwrites_same_key() {
        // 父 agent 传了空/脏数据时，系统注入的真实数据必须赢（防污染保证）。
        let out = merge(
            json!({ "host_session_id": "s1", "execution_trace": [] }),
            json!({ "execution_trace": [{ "tool": "builtin_write_file" }] }),
        );
        assert_eq!(
            out,
            json!({ "host_session_id": "s1", "execution_trace": [{ "tool": "builtin_write_file" }] })
        );
    }

    #[test]
    fn inject_replaces_same_key_wholesale() {
        // 浅合并：同名 key 整块替换，不与父 agent 传的对象深合并。
        let out = merge(
            json!({ "field_selection_result": { "a": 1, "b": 2 } }),
            json!({ "field_selection_result": { "c": 3 } }),
        );
        assert_eq!(out, json!({ "field_selection_result": { "c": 3 } }));
    }

    #[test]
    fn inject_ignores_non_object_extra() {
        let out = merge(json!({ "a": 1 }), json!("not an object"));
        assert_eq!(out, json!({ "a": 1 }));
    }

    #[test]
    fn inject_into_non_object_base_is_ignored() {
        let out = merge(json!("plain text"), json!({ "a": 1 }));
        assert_eq!(out, json!("plain text"));
    }

    #[test]
    fn empty_extra_leaves_base_untouched() {
        let out = merge(json!({ "a": 1 }), json!({}));
        assert_eq!(out, json!({ "a": 1 }));
    }
}
