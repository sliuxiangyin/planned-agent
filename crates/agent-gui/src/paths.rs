//! GUI 落盘路径的**统一解析**（全部从全局 `cache_root` 派生）。
//!
//! 设计稿：`docs/planned-agent/gui-cache-root.md`（§3.3 三条派生规则、§3.6 env 语义）。
//!
//! 改造前：`context/kv.rs` 与 `context/storage.rs` 各有一份**逐字重复**的 `resolve_*_path`，
//! 而 `context/rag.rs` / `services/run_service.rs` 则完全不解析（把配置字符串原样交出）。
//! 这里收敛成一处，四个落盘点（sled KV / SQLite / RAG 向量库 / flexible 产出）都走它。

use std::path::{Component, Path, PathBuf};

/// 词法归一化：去掉 `.` 组件、消解可消解的 `..`，**不碰磁盘**。
///
/// 为什么不用 `canonicalize`：① 它要求路径**已存在**（配置里的目录启动时常常还没建）；
/// ② Windows 上会加上 `\\?\` 前缀，把路径变成另一种形态。
///
/// 不归一化的真实后果（2026-10-08 实测）：`absolutize("./data")` 得到 `<cwd>\./data` ——
/// `PathBuf::join` **不做归一化**，于是 `flexible_output_dir()` 给出
/// `...\agent-gui\./data\cache`，与别处算出的同一目录**字符串不等**；
/// 而它还会被拼进 prompt（环境段的「产出目录」），模型可能照抄这个畸形写法。
pub fn normalize(path: &Path) -> PathBuf {
    let mut components = path.components().peekable();
    // 前缀（Windows 盘符 / UNC）单独起底：`PathBuf::push` 遇到前缀组件会**替换**整个路径，
    // 不能跟 RootDir 一样走 push。
    let mut out = match components.peek() {
        Some(component @ Component::Prefix(_)) => {
            let base = PathBuf::from(component.as_os_str());
            components.next();
            base
        }
        _ => PathBuf::new(),
    };
    for component in components {
        match component {
            Component::Prefix(_) => unreachable!("前缀只可能在首位"),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                // 前一项是普通名才能消解；已在根 / 前缀之下则原样保留
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            Component::Normal(name) => out.push(name),
        }
    }
    out
}

/// 把可能为相对路径的值绝对化：**相对值以进程 cwd 为基准**。
///
/// 取不到 cwd 时保持原样（与改造前 `resolve_*_path` 的兜底一致）。
/// **出口一律经过 [`normalize`]** —— 四个派生点（sled / SQLite / RAG / flexible 产出）
/// 拿到的路径形态一致，且可以直接外发（如环境段的「产出目录」）。
pub fn absolutize(value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return normalize(&path);
    }
    match std::env::current_dir() {
        Ok(cwd) => normalize(&cwd.join(path)),
        Err(_) => normalize(&path),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// **出口一律归一化**：`./` 不再残留。
    ///
    /// 不归一化就会给出 `...\agent-gui\./data\cache`（2026-10-08 实测），既与别处
    /// 算出的同一目录字符串不等，又会被拼进 prompt 让模型照抄。
    #[test]
    fn absolutize_strips_dot_components() {
        let path = absolutize("./data");
        let shown = path.to_string_lossy().to_string();
        assert!(!shown.contains("./"), "{shown}");
        assert!(!shown.contains(".\\"), "{shown}");
        assert!(path.is_absolute(), "{shown}");
    }

    /// `resolve_under` 的三条规则（`gui-cache-root.md` §3.3）在归一化后仍然成立。
    #[test]
    fn resolve_under_matches_documented_rules() {
        let cwd = std::env::current_dir().expect("测试环境应有 cwd");
        // 单段名 → 根下的一级子项
        assert_eq!(resolve_under("./data", "cache"), cwd.join("data").join("cache"));
        // 多段相对路径 → 视为相对 cwd，**不再拼根**
        assert_eq!(
            resolve_under("./data", "./other/store"),
            cwd.join("other").join("store")
        );
        // 空子项 → 根自身
        assert_eq!(resolve_under("./data", "   "), cwd.join("data"));
    }

    /// `..` 只在前一项是普通名时消解；已在根 / 前缀之下则原样保留。
    #[test]
    fn normalize_resolves_parent_dir_lexically() {
        #[cfg(not(windows))]
        {
            assert_eq!(normalize(Path::new("a/b/../c")), PathBuf::from("a/c"));
            assert_eq!(normalize(Path::new("../a")), PathBuf::from("../a"));
        }
        assert_eq!(normalize(Path::new("a/./b")), PathBuf::from("a/b"));
        assert_eq!(normalize(Path::new("./a")), PathBuf::from("a"));
    }
}
