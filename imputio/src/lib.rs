//! imputio is an experimental async runtime
//! intended for educational purposes

mod executor;
mod io;
mod runtime;
mod task;
#[macro_use]
pub mod macros;

pub use executor::{imputio_spawn as spawn, imputio_spawn_blocking as spawn_blocking, ExecHandle};
pub use runtime::{ImputioRuntime, RuntimeScheduler};
pub use task::{ImputioTask, ImputioTaskHandle};

use std::sync::LazyLock;

// FIXME work this into builder pattern
pub static EXECUTOR: LazyLock<ExecHandle> = LazyLock::new(|| ExecHandle::initialize(None));

/// Main entry point for running futures within an imputio runtime
/// without configuring the runtime with additional params
pub fn rt_entry<F, R>(fut: F) -> R
where
    F: std::future::Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    ImputioRuntime::new().block_on(fut)
}

/// Priority is used to enqueue tasks onto priority queues
/// note that idx 0 is reserved for system priority work
#[repr(usize)]
#[derive(Default, Clone, Copy, Debug)]
pub enum Priority {
    BestEffort = 4,
    #[default]
    Low = 3,
    Medium = 2,
    High = 1,
}

/// Internal system priorities to allow
/// scheduler to prioritize system tasks / threads
/// as needed. For internal use only
#[allow(unused)]
#[repr(usize)]
#[derive(Clone, Copy, Debug)]
enum SystemPriority {
    SystemPrioritize = 0, // highest priority
    UserPriority(Priority),
}
