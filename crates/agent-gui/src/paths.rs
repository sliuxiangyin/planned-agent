//! GUI 落盘路径的**统一解析**（全部从全局 `cache_root` 派生）。
//!
//! 设计稿：`docs/planned-agent/gui-cache-root.md`（§3.3 三条派生规则、§3.6 env 语义）。
//!
//! 改造前：`context/kv.rs` 与 `context/storage.rs` 各有一份**逐字重复**的 `resolve_*_path`，
//! 而 `context/rag.rs` / `services/run_service.rs` 则完全不解析（把配置字符串原样交出）。
//! 这里收敛成一处，四个落盘点（sled KV / SQLite / RAG 向量库 / flexible 产出）都走它。

use std::path::{Path, PathBuf};

/// 把可能为相对路径的值绝对化：**相对值以进程 cwd 为基准**。
///
/// 取不到 cwd 时保持原样（与改造前 `resolve_*_path` 的兜底一致）。
pub fn absolutize(value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return path;
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path,
    }
}

/// 把**子项值**解析为最终落盘路径 —— `gui-cache-root.md` §3.3 的三条规则：
///
/// 1. **绝对路径** → 原样使用（等于完全自定义，**逃出根**）；
/// 2. **单段相对名**（不含 `/` / `\`，如 `kv_store`）→ `cache_root` 下的**一级子项**；
/// 3. **多段相对路径**（含路径分隔符，如 `./data/kv_store`）→ 视为**相对 cwd** 的完整路径，
///    **不再拼根**。
///
/// 第 3 条是刻意保留的，别改成「相对值一律拼根」：用户 `config.toml` 里残留的旧值
/// （`./data/kv_store`）若拼根会变成 `<root>/data/kv_store`，而且解析期的 `create_dir_all`
/// 会把它**静默建出来** —— 表现成「数据凭空消失 + 多出一层嵌套目录」。
pub fn resolve_under(root: &str, child: &str) -> PathBuf {
    let child = child.trim();
    if child.is_empty() {
        return absolutize(root);
    }
    let path = Path::new(child);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    if child.contains('/') || child.contains('\\') {
        return absolutize(child);
    }
    absolutize(root).join(child)
}

/// 与 [`resolve_under`] 相同，但先看 `env`：**存在即整值覆盖**（按「绝对 / 相对 cwd」解释），
/// 优先级高于 `cache_root` 派生（`gui-cache-root.md` §3.6）。
pub fn resolve_with_env(root: &str, child: &str, env: &str) -> PathBuf {
    match std::env::var(env) {
        Ok(value) => absolutize(&value),
        Err(_) => resolve_under(root, child),
    }
}

/// 建**父目录**（即 `cache_root`），**不建自身**：
/// sled / SQLite / PolarisDB 各自会创建自己的目录或文件（如 `sled::open`、`mode=rwc`、
/// `AsyncCollection::open_or_create`）。
pub fn ensure_parent(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}
