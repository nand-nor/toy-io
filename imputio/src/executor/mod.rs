pub mod handle;

use std::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug)]
pub struct ExecConfig {
    exec_thread_config: Vec<ExecThreadConfig>,
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            exec_thread_config: vec![ExecThreadConfig::default()],
        }
    }
}

impl IntoIterator for ExecConfig {
    type Item = ExecThreadConfig;

    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.exec_thread_config.into_iter()
    }
}

impl ExecConfig {
    pub fn with_cfg(mut self, exec_thread_config: Vec<ExecThreadConfig>) -> Self {
        self.exec_thread_config = exec_thread_config;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.exec_thread_config.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct ThreadConfig {
    thread_name: String,
    stack_size: usize,
    core_id: Option<usize>,
}

impl Default for ThreadConfig {
    fn default() -> Self {
        Self {
            thread_name: "imputio-thread".to_string(),
            stack_size: u16::MAX as usize,
            core_id: None,
        }
    }
}

impl ThreadConfig {
    pub fn with_name(mut self, name: String) -> Self {
        self.thread_name = name;
        self
    }

    pub fn with_core_affinity(mut self, core_id: usize) -> Self {
        self.core_id = Some(core_id);
        self
    }

    pub fn with_stack_size(mut self, size: usize) -> Self {
        self.stack_size = size;
        self
    }
}

#[derive(Clone, Debug)]
pub struct ExecThreadConfig {
    thread_cfg: ThreadConfig,
    poll_thread_cfg: PollThreadConfig,
}

impl Default for ExecThreadConfig {
    fn default() -> Self {
        Self {
            thread_cfg: ThreadConfig {
                thread_name: "imputio-exec-thread".to_string(),
                ..Default::default()
            },
            poll_thread_cfg: PollThreadConfig::default(),
        }
    }
}

impl ExecThreadConfig {
    pub fn with_poll_thread_cfg(mut self, poll_cfg: PollThreadConfig) -> Self {
        self.poll_thread_cfg = poll_cfg;
        self
    }

    pub fn with_name(mut self, name: String) -> Self {
        self.thread_cfg = self.thread_cfg.with_name(name);
        self
    }

    pub fn with_core_affinity(mut self, core_id: usize) -> Self {
        self.thread_cfg = self.thread_cfg.with_core_affinity(core_id);
        self
    }

    pub fn with_stack_size(mut self, size: usize) -> Self {
        self.thread_cfg = self.thread_cfg.with_stack_size(size);
        self
    }
}

#[derive(Clone, Debug)]
pub struct PollThreadConfig {
    thread_cfg: ThreadConfig,
    poll_cfg: PollerCfg,
}

impl Default for PollThreadConfig {
    fn default() -> Self {
        Self {
            thread_cfg: ThreadConfig {
                thread_name: "imputio-io-actor-thread".to_string(),
                ..Default::default()
            },
            poll_cfg: PollerCfg::default(),
        }
    }
}

impl PollThreadConfig {
    pub fn with_poll_thread_cfg(mut self, poll_cfg: PollerCfg) -> Self {
        self.poll_cfg = poll_cfg;
        self
    }

    pub fn with_name(mut self, name: String) -> Self {
        self.thread_cfg = self.thread_cfg.with_name(name);
        self
    }

    pub fn with_core_affinity(mut self, core_id: usize) -> Self {
        self.thread_cfg = self.thread_cfg.with_core_affinity(core_id);
        self
    }

    pub fn with_stack_size(mut self, size: usize) -> Self {
        self.thread_cfg = self.thread_cfg.with_stack_size(size);
        self
    }
}

use crate::{io::PollerCfg, ImputioTaskHandle};

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
