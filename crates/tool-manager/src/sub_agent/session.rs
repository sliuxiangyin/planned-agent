//! 子 agent 会话存储：管理挂起会话的生命周期。
//!
//! - 挂起（`AwaitingUserAction`）→ `upsert` 入存储（同时登记 resume 信号）；
//! - 恢复（前端 resume）→ `signal_resume` 发信号唤醒挂起的 `execute_streamed`，
//!   由后者 `take` 取出会话继续执行（取出即移除，天然防重入）；
//! - 完成（`Done`）→ 会话已被 take，无需额外清理；
//! - 防泄漏：`TTL`（默认 10 分钟）惰性清理——每次操作时顺带扫描过期会话；
//!   调用方也可显式 `clear`。

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::sub_agent::types::SubAgentSession;

/// 默认 TTL：挂起会话 10 分钟未被 resume 即被清理
pub const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(10 * 60);

struct SessionEntry {
    session: Box<dyn SubAgentSession>,
    /// resume 信号：前端 resume 时 `signal_resume` 取出并发送用户选择，
    /// 唤醒正在 `rx.await` 阻塞的 `execute_streamed`。取出即移除，防重入。
    resume_tx: Option<oneshot::Sender<Value>>,
    // 以下字段供调试/统计/未来清理策略使用
    #[allow(dead_code)]
    agent_name: String,
    #[allow(dead_code)]
    created_at: DateTime<Utc>,
    last_active: DateTime<Utc>,
}

/// 子 agent 会话存储（线程安全）
pub struct SubAgentSessionStore {
    sessions: RwLock<HashMap<String, SessionEntry>>,
    ttl: Duration,
}

impl SubAgentSessionStore {
    /// 创建会话存储（默认 TTL 10 分钟）
    pub fn new() -> Self {
        Self::with_ttl(DEFAULT_SESSION_TTL)
    }

    /// 创建会话存储（自定义 TTL）
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            ttl,
        }
    }

    /// 存入（新会话）或更新（resume 后再次挂起，沿用原 id）会话，
    /// 同时登记本次挂起的 resume 信号。
    /// 顺带惰性清理过期会话。
    pub fn upsert(
        &self,
        session_id: String,
        session: Box<dyn SubAgentSession>,
        agent_name: String,
        resume_tx: oneshot::Sender<Value>,
    ) {
        self.purge_expired();
        let mut map = self.sessions.write().unwrap();
        let now = Utc::now();
        map.insert(
            session_id,
            SessionEntry {
                session,
                resume_tx: Some(resume_tx),
                agent_name,
                created_at: now,
                last_active: now,
            },
        );
    }

    /// 前端 resume：向挂起会话发送用户选择，唤醒阻塞中的 `execute_streamed`。
    ///
    /// 取出 `resume_tx` 即移除（防重入）：重复 resume 同一会话会报错。
    /// 会话不存在或未挂起时返回错误（可能已过期清理或已完成）。
    pub fn signal_resume(&self, session_id: &str, user_input: Value) -> Result<()> {
        let mut map = self.sessions.write().unwrap();
        if let Some(entry) = map.get_mut(session_id) {
            if let Some(tx) = entry.resume_tx.take() {
                let _ = tx.send(user_input);
                return Ok(());
            }
        }
        Err(anyhow!(
            "Sub agent session not found, expired, or not awaiting: {}",
            session_id
        ))
    }

    /// 仅更新 resume 信号（不更新 session），用于 resume 路径中
    /// 先取出会话后需要重新注册信号等待 signal_resume
    pub fn upsert_resume_signal(&self, session_id: String, resume_tx: oneshot::Sender<Value>) {
        let mut map = self.sessions.write().unwrap();
        if let Some(entry) = map.get_mut(&session_id) {
            entry.resume_tx = Some(resume_tx);
        }
    }

    /// 取出会话用于 resume（取出即移除，防重入）。
    /// 会话不存在时返回错误（可能已过期清理或已完成）。
    pub fn take(&self, session_id: &str) -> Result<Box<dyn SubAgentSession>> {
        self.purge_expired();
        let mut map = self.sessions.write().unwrap();
        map.remove(session_id)
            .map(|entry| entry.session)
            .ok_or_else(|| anyhow!("Sub agent session not found or expired: {}", session_id))
    }

    /// 检查会话是否存在（不取出）
    pub fn get(&self, session_id: &str) -> bool {
        let map = self.sessions.read().unwrap();
        map.contains_key(session_id)
    }

    /// 当前挂起的会话数量（不含已过期未清理的）
    pub fn len(&self) -> usize {
        self.sessions.read().unwrap().len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 显式清空所有挂起会话（主 agent 会话结束/取消时调用）
    pub fn clear(&self) -> usize {
        let mut map = self.sessions.write().unwrap();
        let count = map.len();
        map.clear();
        count
    }

    /// 惰性清理：移除超过 TTL 未活动的会话
    fn purge_expired(&self) {
        let mut map = self.sessions.write().unwrap();
        let now = Utc::now();
        map.retain(|_, entry| {
            let elapsed = now.signed_duration_since(entry.last_active);
            elapsed.num_seconds() < self.ttl.as_secs() as i64
        });
    }
}

impl Default for SubAgentSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sub_agent::stream::ToolStreamSender;
    use async_trait::async_trait;
    use planned_agent_core::mcp::types::ToolResult;
    use serde_json::{json, Value};

    struct DummySession;

    #[async_trait]
    impl SubAgentSession for DummySession {
        async fn resume(
            &mut self,
            _user_input: Value,
            _stream: ToolStreamSender,
        ) -> Result<crate::sub_agent::SubAgentRunOutcome> {
            Ok(crate::sub_agent::SubAgentRunOutcome::Done(ToolResult {
                call_id: String::new(),
                content: json!("ok"),
                is_error: false,
            }))
        }
    }

    #[test]
    fn upsert_take_roundtrip() {
        let store = SubAgentSessionStore::new();
        let (tx, _rx) = oneshot::channel();
        store.upsert("sid-1".into(), Box::new(DummySession), "agent".into(), tx);
        assert_eq!(store.len(), 1);
        let taken = store.take("sid-1").expect("take ok");
        assert!(store.is_empty());
        drop(taken);
        // 二次 take 失败（防重入）
        assert!(store.take("sid-1").is_err());
    }

    #[test]
    fn signal_resume_wakes_once() {
        let store = SubAgentSessionStore::new();
        let (tx, mut rx) = oneshot::channel();
        store.upsert("sid-1".into(), Box::new(DummySession), "agent".into(), tx);
        // 首次 signal 成功，唤醒接收端
        store
            .signal_resume("sid-1", json!("确认"))
            .expect("first signal ok");
        // 注意：signal 取出 tx 即移除，二次 signal 应报错
        assert!(store.signal_resume("sid-1", json!("again")).is_err());
        drop(rx);
    }

    #[test]
    fn ttl_expired_session_is_purged() {
        let store = SubAgentSessionStore::with_ttl(Duration::from_millis(1));
        let (tx, _rx) = oneshot::channel();
        store.upsert("sid-1".into(), Box::new(DummySession), "agent".into(), tx);
        std::thread::sleep(Duration::from_millis(20));
        // 下一次操作触发惰性清理
        assert!(store.take("sid-1").is_err());
        assert!(store.is_empty());
    }
}
