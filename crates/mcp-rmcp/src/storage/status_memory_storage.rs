//! 内存存储实现：[`McpStatusStorage`] 的测试后端
//!
//! 基于 `RwLock<BTreeMap<String, ServerStatus>>`，全部操作零 I/O，
//! 适合单元测试与未来 contract test。

use std::collections::BTreeMap;
use std::sync::RwLock;

use anyhow::Result;

use crate::storage::status_trait::{McpStatusStorage, ServerStatus};

/// 基于内存的 [`McpStatusStorage`] 实现（测试场景）
pub struct InMemoryMcpStatusStorage {
    inner: RwLock<BTreeMap<String, ServerStatus>>,
}

impl InMemoryMcpStatusStorage {
    /// 构造空存储
    pub fn new() -> Self {
        Self::with_map(BTreeMap::new())
    }

    /// 构造预填充存储（测试断言用）
    pub fn with_map(map: BTreeMap<String, ServerStatus>) -> Self {
        Self {
            inner: RwLock::new(map),
        }
    }

    /// 当前快照（仅测试断言用）
    pub fn snapshot(&self) -> BTreeMap<String, ServerStatus> {
        self.inner.read().unwrap().clone()
    }
}

impl Default for InMemoryMcpStatusStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl McpStatusStorage for InMemoryMcpStatusStorage {
    fn load_all(&self) -> Result<Vec<(String, ServerStatus)>> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }

    fn get(&self, name: &str) -> Result<Option<ServerStatus>> {
        Ok(self.inner.read().unwrap().get(name).cloned())
    }

    fn record(&self, name: &str, status: ServerStatus) -> Result<()> {
        self.inner
            .write()
            .unwrap()
            .insert(name.to_string(), status);
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<()> {
        self.inner.write().unwrap().remove(name);
        Ok(())
    }

    fn has_status(&self, name: &str) -> bool {
        self.inner.read().unwrap().contains_key(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::status_trait::LastStatus;

    fn ready_status(count: u32, ts: u64) -> ServerStatus {
        ServerStatus::ready(count, ts)
    }

    fn failed_status(kind: &str, msg: &str, ts: u64) -> ServerStatus {
        ServerStatus::failed(kind, msg, ts)
    }

    #[test]
    fn crud_roundtrip() {
        let s = InMemoryMcpStatusStorage::new();

        // 空
        assert!(s.load_all().unwrap().is_empty());
        assert!(!s.has_status("a"));

        // record ready
        s.record("playwright", ready_status(24, 1000)).unwrap();
        assert!(s.has_status("playwright"));
        assert_eq!(
            s.get("playwright").unwrap(),
            Some(ready_status(24, 1000))
        );

        // record failed（覆盖）
        s.record("desktop-commander", failed_status("timeout", "took 120s", 2000))
            .unwrap();
        assert_eq!(s.load_all().unwrap().len(), 2);

        // 再次 record 同名 = 覆盖
        s.record("playwright", failed_status("handshake", "conn closed", 3000))
            .unwrap();
        match s.get("playwright").unwrap().unwrap().status {
            LastStatus::Failed => {}
            _ => panic!("期望 Failed"),
        }

        // delete
        s.delete("desktop-commander").unwrap();
        assert!(!s.has_status("desktop-commander"));
        assert_eq!(s.load_all().unwrap().len(), 1);

        // delete 不存在的 key：no-op，不报错
        s.delete("ghost").unwrap();

        // snapshot 反映当前状态
        let snap = s.snapshot();
        assert_eq!(snap.len(), 1);
        assert!(snap.contains_key("playwright"));
    }

    #[test]
    fn staleness_detection() {
        let now = 100_000u64;
        let fresh = ServerStatus::ready(5, now - 600); // 10 min ago
        let stale = ServerStatus::ready(5, now - 7200); // 2 h ago
        assert!(!fresh.is_stale(now));
        assert!(stale.is_stale(now));
    }

    #[test]
    fn serde_roundtrip_preserves_all_fields() {
        let original = failed_status("handshake", "conn closed", 12345);
        let json = serde_json::to_string(&original).unwrap();
        let parsed: ServerStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(original, parsed);

        // error_kind / error_message 在 Ready 时不应出现
        let ready = ready_status(3, 99);
        let json2 = serde_json::to_string(&ready).unwrap();
        assert!(!json2.contains("error_kind"));
        assert!(!json2.contains("error_message"));
    }
}
