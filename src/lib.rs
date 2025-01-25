//! imputio is an experimental async runtime
//! intended for educational purposes

// FIXME
#![allow(static_mut_refs)]
// todo figure out better way that does not require nightly
#![feature(once_cell_get_mut)]

pub mod events;
mod executor;
mod runtime;
mod task;
#[macro_use]
pub mod macros;

pub use events::{event_poll_matcher, EventBus, EventHandle};
pub use executor::{imputio_spawn as spawn, imputio_spawn_blocking as spawn_blocking, Executor};

pub use runtime::{ImputioRuntime, RuntimeScheduler};
pub use task::{ImputioTask, ImputioTaskHandle};

// FIXME
pub static mut EXECUTOR: std::sync::OnceLock<Executor> = std::sync::OnceLock::new();

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
