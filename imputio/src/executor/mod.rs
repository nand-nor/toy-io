pub mod handle;

use core_affinity::CoreId;
use std::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) struct ExecConfig {
    exec_thread_config: Vec<(ThreadConfig, ThreadConfig)>,
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            exec_thread_config: vec![
                (ThreadConfig::default(), ThreadConfig::default_io_poller()),
                (ThreadConfig::default(), ThreadConfig::default_io_poller()),
                (ThreadConfig::default(), ThreadConfig::default_io_poller()),
            ],
        }
    }
}

#[derive(Clone)]
pub(crate) struct ThreadConfig {
    thread_name: String,
    stack_size: usize,
    core_id: Option<CoreId>,
    poll_cfg: Option<PollCfg>,
}

impl Default for ThreadConfig {
    fn default() -> Self {
        Self {
            thread_name: "imputio-exec-thread".to_string(),
            stack_size: u16::MAX as usize,
            core_id: None,
            poll_cfg: None,
        }
    }
}

impl ThreadConfig {
    fn default_io_poller() -> Self {
        Self {
            poll_cfg: Some(PollCfg::default()),
            thread_name: "imputio-io-actor-thread".to_string(),
            ..Default::default()
        }
    }
}

use crate::{io::PollCfg, ImputioTaskHandle};

pub fn imputio_spawn<F, T>(fut: F, priority: crate::Priority) -> ImputioTaskHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let exec = crate::EXECUTOR.load();
    exec.spawn(fut, priority)
}

/// #Panics
///
/// If spawn_blocking is called in a blocking context, the thread will
/// panic
pub fn imputio_spawn_blocking<F, T>(fut: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    // FLAG is used to trigger panics (to avoid blocking forever)
    thread_local! {
        static FLAG: AtomicBool = const { AtomicBool::new(false) };
    };

    if !FLAG.with(|k| k.swap(true, Ordering::SeqCst)) {
        let exec = crate::EXECUTOR.load();
        let res = exec.spawn_blocking(fut);
        FLAG.with(|k| k.store(false, Ordering::SeqCst));
        res
    } else {
        panic!("Blocking twice on same thread will block forever")
    }
}
