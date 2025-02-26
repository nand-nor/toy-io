pub mod handle;

use std::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
};

pub use handle::ExecHandle;

use crate::ImputioTaskHandle;

pub fn imputio_spawn<F, T>(fut: F, priority: crate::Priority) -> ImputioTaskHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    crate::EXECUTOR.spawn(fut, priority)
}

/// #Panics
///
/// If spawn_blocking is called in a blocking context, the thread will
/// panic
pub fn imputio_spawn_blocking<F, T>(fut: F, priority: crate::Priority) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    // FLAG is used to trigger panics (to avoid blocking forever)
    static FLAG: AtomicBool = AtomicBool::new(false);

    if !FLAG.swap(true, Ordering::SeqCst) {
        let res = crate::EXECUTOR.spawn_blocking(fut, priority);
        FLAG.store(false, Ordering::SeqCst);
        res
    } else {
        panic!("Blocking twice on same thread will block forever")
    }
}
