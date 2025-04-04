//! imputio is an experimental async runtime
//! intended for educational purposes

mod executor;
mod io;
mod runtime;
mod task;
#[macro_use]
pub mod macros;

use arc_swap::ArcSwap;
use executor::handle::ExecHandleCoordinator;

use io::Operation;

pub use executor::{
    ExecConfig, ExecThreadConfig, PollThreadConfig, imputio_spawn as spawn,
    imputio_spawn_blocking as spawn_blocking,
};
use runtime::ImputioRuntimeBuilder;
pub use runtime::{ImputioRuntime, RuntimeError};
pub use task::{ImputioTask, ImputioTaskHandle};

// re-export mio dep's Interest and Event objects
pub use mio::{Interest, event::Event};

use std::{
    os::fd::RawFd,
    sync::{Arc, LazyLock},
};

#[cfg(feature = "tikv-jemallocator")]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

pub(crate) static EXECUTOR: LazyLock<ArcSwap<ExecHandleCoordinator>> =
    LazyLock::new(|| ArcSwap::from(Arc::new(ExecHandleCoordinator::new())));

/// Main entry point for running futures within an imputio runtime
/// without configuring the runtime with additional params
pub fn rt_entry<F, R>(fut: F) -> R
where
    F: std::future::Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    let mut rt = ImputioRuntimeBuilder::default()
        .build()
        .unwrap_or_else(|_| panic!("Unable to startup imputio runtime"));

    rt.block_on(fut)
}

fn check_io() -> Result<(), RuntimeError> {
    if !cfg!(feature = "io") {
        let msg = "IO feature is not currently enabled";
        tracing::error!("{msg}");
        Err(RuntimeError::Internal(msg.to_string()))
    } else {
        Ok(())
    }
}

/// Provides public method for registering TCP socket without having a handle to the
/// runtime object
pub fn register_tcp_socket(
    listener: std::net::TcpListener,
    interest: Option<Interest>,
    notifier: Option<flume::Sender<Event>>,
) -> Result<(), RuntimeError> {
    check_io()?;
    let interest = if let Some(i) = interest {
        i
    } else {
        Interest::READABLE
            .add(Interest::WRITABLE)
            .add(Interest::PRIORITY)
    };

    let op = Operation::RegistrationTcpAdd {
        fd: listener,
        interest,
        notify: notifier,
    };

    add_operation_to_io_poller(op)?;

    Ok(())
}

/// Provides public method for registering general purpose file descriptor
/// with the IO poller object, without having a handle to the runtime object
pub fn register_fd(
    fd: RawFd,
    interest: Option<Interest>,
    notifier: Option<flume::Sender<Event>>,
) -> Result<(), RuntimeError> {
    check_io()?;
    let interest = if let Some(i) = interest {
        i
    } else {
        Interest::READABLE
            .add(Interest::WRITABLE)
            .add(Interest::PRIORITY)
    };

    let op = Operation::RegistrationFdAdd {
        fd,
        interest,
        notify: notifier,
    };

    add_operation_to_io_poller(op)?;
    Ok(())
}

/// Provides public method for registering general IO operation having a handle to the
/// runtime object. Excepts runtime to be in running state
pub fn add_operation_to_io_poller(op: Operation) -> Result<(), RuntimeError> {
    check_io()?;
    crate::EXECUTOR.load().submit_io_op(op)?;
    Ok(())
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
