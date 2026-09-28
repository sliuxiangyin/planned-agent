//! 宿主事实：本机运行环境这类**初始化时探测一次、之后只读**的信息。
//!
//! # 准入标准
//!
//! 想往本目录加东西，必须**全部**满足：
//! - ✅ 进程启动时探测 / 读取一次，之后只读；
//! - ✅ 与 agent 领域逻辑无关（不认识 AI / 工具 / 规划）；
//! - ✅ 不需要调用方传参。
//!
//! 不符合的请另找归处：
//! - ❌ 运行时**可变**状态（会话 / 缓存 / 连接池）—— 那不是「事实」；
//! - ❌ 领域相关抽象 —— 回 `ai/` / `tool_registry/` / `planner/` 各自模块。
//!
//! # 约定
//!
//! 本目录内的 API 必须让人一眼看出**是否含副作用**（spawn / 读盘 / 读环境
//! 变量）：例如 [`environment::RuntimeEnvironment::detect_host`] 是纯的，
//! [`environment::RuntimeEnvironment::detect`] 会 spawn 进程。
//! 这样调用方（如宿主启动时的初始化）才能判断能不能在首屏同步调。

pub mod environment;

pub use environment::{probe_executables, ExecutableProbe, RuntimeEnvironment, DEFAULT_PROBE_NAMES};
