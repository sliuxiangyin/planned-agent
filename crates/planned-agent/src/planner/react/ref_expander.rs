//! 引用展开工具
//!
//! 递归扫描 JSON 值中的 "#E" 引用字符串，自动从 StepStore 展开为真实数据。
//! LLM 拿到 fetch_step_result 返回的引用后，自然会把 "#E1" 当作参数值传给其他工具。
//! 此模块在系统层透明替换，LLM 无需感知。

use serde_json::Value;
use std::collections::HashMap;

/// 超过此大小的引用数据不展开，LLM 必须通过 chunk_read 路径获取片段。
const MAX_EXPAND_BYTES: usize = 800;

/// 递归扫描 JSON 值中的 "#E" 引用字符串，自动从 store 展开为真实数据。
///
/// 匹配规则：严格匹配 "#E" + 纯数字（如 #E1、#E12），
/// 避免误匹配 hex 颜色(#FFF)、CSS 选择器(#header) 等。
pub(crate) fn expand_refs(value: &mut Value, store: &HashMap<String, Value>) {
    match value {
        Value::String(s) => {
            let is_ref = s.len() >= 3
                && s.starts_with("#E")
                && s[2..].chars().all(|c| c.is_ascii_digit());
            if is_ref {
                if let Some(data) = store.get(s.as_str()) {
                    let size = serde_json::to_string(data)
                        .map(|s| s.len())
                        .unwrap_or(0);
                    // 仅小数据自动展开，大数据需走 chunk 路径
                    if size <= MAX_EXPAND_BYTES {
                        *value = data.clone();
                    }
                }
            }
        }
        Value::Array(arr) => {
            for v in arr {
                expand_refs(v, store);
            }
        }
        Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                expand_refs(v, store);
            }
        }
        _ => {}
    }
}
