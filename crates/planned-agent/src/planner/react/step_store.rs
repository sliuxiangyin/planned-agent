//! 步骤结果存储（StepStore）
//!
//! 封装共享的步骤结果存储，内部使用 `_summary` 后缀约定：
//! - `#E1`         → output（工具输出数据）
//! - `#E1_summary` → observe 阶段的执行摘要
//!
//! 调用方通过 get_output / get_summary / list_entries 访问，
//! 无需感知内部 key 后缀约定。

use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard};
use anyhow::{anyhow, Result};
use serde_json::Value;

/// 步骤条目（list_entries 用，自动关联 output 和 summary）
#[derive(Debug, Clone)]
pub struct StepEntry {
    /// 引用标识，如 "#E1"
    pub ref_id: String,
    /// 原始输出
    pub output: Value,
    /// 序列化字节数
    pub data_size: usize,
    /// observe 摘要（可选）
    pub summary: Option<String>,
    /// 是否失败
    pub is_error: bool,
}

/// 步骤结果存储（线程安全，可 Clone 共享）
#[derive(Clone)]
pub struct StepStore {
    inner: Arc<RwLock<HashMap<String, Value>>>,
}

impl StepStore {
    /// 创建空的 StepStore
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.inner
            .read()
            .map(|g| g.is_empty())
            .unwrap_or(true)
    }

    /// 存入步骤结果。
    ///
    /// 内部：
    /// - `inner[ref_id]` = output
    /// - `inner[ref_id_summary]` = summary（如果提供）
    pub fn insert(&self, ref_id: &str, output: Value, summary: Option<String>) -> Result<()> {
        let mut guard = self
            .inner
            .write()
            .map_err(|e| anyhow!("StepStore 写锁获取失败: {}", e))?;

        guard.insert(ref_id.to_string(), output);

        if let Some(s) = summary {
            let summary_key = summary_key(ref_id);
            guard.insert(summary_key, Value::String(s));
        }

        Ok(())
    }

    /// 获取原始读锁（供 expand_refs / fetch_step_result 直接操作内部 map，保持兼容）
    pub fn read(&self) -> Result<RwLockReadGuard<'_, HashMap<String, Value>>> {
        self.inner
            .read()
            .map_err(|e| anyhow!("StepStore 读锁获取失败: {}", e))
    }

    /// 按引用标识获取输出数据
    pub fn get_output(&self, ref_id: &str) -> Option<Value> {
        self.inner
            .read()
            .ok()
            .and_then(|g| g.get(ref_id).cloned())
    }

    /// 按引用标识获取执行摘要
    pub fn get_summary(&self, ref_id: &str) -> Option<String> {
        self.inner
            .read()
            .ok()
            .and_then(|g| g.get(&summary_key(ref_id)).cloned())
            .and_then(|v| v.as_str().map(|s| s.to_string()))
    }

    /// 列出所有步骤条目（自动过滤 `_summary` 后缀，关联 output + summary）
    ///
    /// 用于 init_messages 构建前序步骤摘要展示。
    pub fn list_entries(&self) -> Result<Vec<StepEntry>> {
        let guard = self.read()?;
        let mut entries: Vec<StepEntry> = Vec::new();

        for (k, v) in guard.iter() {
            // 跳过 summary 子 key
            if k.ends_with(SUMMARY_SUFFIX) {
                continue;
            }

            let data_size = serde_json::to_string(v).map(|s| s.len()).unwrap_or(0);
            let is_error = v
                .get("error")
                .and_then(|e| e.as_bool())
                .unwrap_or(false);
            let summary = guard
                .get(&summary_key(k))
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());

            entries.push(StepEntry {
                ref_id: k.clone(),
                output: v.clone(),
                data_size,
                summary,
                is_error,
            });
        }

        Ok(entries)
    }
}

impl Default for StepStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── 内部工具 ──────────────────────────────────────────

const SUMMARY_SUFFIX: &str = "_summary";

fn summary_key(ref_id: &str) -> String {
    format!("{}{}", ref_id, SUMMARY_SUFFIX)
}
