//! Plan 模块的内部纯类型与数据模型。
//!
//! 仅 `plan` 子模块内部使用；状态容器 `PlanState` 拆到同级 `states` 模块，
//! 本模块只保留不持有 `Signal` 的类型。
//!
//! 内部只有一个子文件：
//! - `plan` — 计划参数定义（`ParamDef`）
//!
//! 子文件内项声明为 `pub(crate)`（re-export 提级到 `plan` 可见的必要条件，
//! 且 `plan` 模块本身是私有 `mod`，crate 外不可达，封装不受影响），
//! 本文件统一以 `pub(super)`（对 `plan` 可见）重新导出。

mod plan;

pub(super) use plan::ParamDef;
