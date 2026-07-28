//! ExecutorContext：运行时的依赖注入桥接容器。
//!
//! 与 `ToolRegistry` 解耦，专供 executor 在运行时获取构造期不存在的对象（如 ChunkStore）。
//! 通过 `Weak` 引用避免与 `ToolRegistry` 形成循环。

use std::sync::{Arc, RwLock, Weak};

use super::chunk_store::ChunkStore;

/// 运行时注入上下文。
pub struct ExecutorContext {
    chunk_store: RwLock<Option<Arc<ChunkStore>>>,
}

impl ExecutorContext {
    /// 创建空上下文，后续调用 `set_chunk_store` 注入。
    pub fn new() -> Self {
        Self {
            chunk_store: RwLock::new(None),
        }
    }

    /// 注入 ChunkStore（由 DefaultReActAgent 在构造时调用）。
    pub fn set_chunk_store(&self, cs: Arc<ChunkStore>) {
        *self.chunk_store.write().unwrap() = Some(cs);
    }

    /// 获取 ChunkStore（无竞争锁，仅在执行器线程使用）。
    pub fn chunk_store(&self) -> Option<Arc<ChunkStore>> {
        self.chunk_store.read().unwrap().clone()
    }

    /// 创建 Weak<Self> 供 executor 持有，不增加引用计数。
    pub fn downgrade(self: &Arc<Self>) -> Weak<Self> {
        Arc::downgrade(self)
    }
}
