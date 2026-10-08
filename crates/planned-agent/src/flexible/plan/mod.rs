//! 计划期：落库模板的**静态形态**。
//!
//! 这一组只描述「计划长什么样」，不关心怎么执行 —— 模板的强类型、占位符的
//! 收集与替换、运行参数表、输出契约。执行期在 [`crate::flexible::exec`]。

pub mod category;
pub mod output_schema;
pub mod params;
pub mod placeholder;
pub mod template;
