//! 启动阶段的视图状态表达。

use std::sync::Arc;

/// 启动阶段的视图状态（泛型化 `Ready` 载荷，供全局 bootstrap 与各页面会话启动复用）。
pub enum BootPhase<T> {
    /// 仍在初始化；携带「已完成模块名」列表，供 Splash 展示加载进度
    Loading(Vec<&'static str>),
    /// 就绪：持有可注入的服务句柄
    Ready(Arc<T>),
    /// 失败：携带「模块名 + 错误信息」清单
    Failed(Vec<(String, String)>),
}

// `Arc<T>` 无条件 Clone，故 `BootPhase<T>` 也无需 `T: Clone` 即可克隆
// （`Signal` 的「读-改-写回」需要它）。
impl<T> Clone for BootPhase<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Loading(done) => Self::Loading(done.clone()),
            Self::Ready(services) => Self::Ready(services.clone()),
            Self::Failed(errors) => Self::Failed(errors.clone()),
        }
    }
}

/// 启动进度回调：`bootstrap` / `boot_flexible_session` 每完成一个阶段回调一次。
pub type OnProgress = Arc<dyn Fn(&'static str) + Send + Sync>;
