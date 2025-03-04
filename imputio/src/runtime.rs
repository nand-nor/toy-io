use std::{
    fmt::Debug,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, ThreadId},
};

use core_affinity::CoreId;
use derive_builder::Builder;
use flume::{Receiver, Sender, TryRecvError};
use thiserror::Error;

use crate::{
    executor::handle::{ExecError, ExecHandleCoordinator},
    io::PollError,
    spawn_blocking, ExecConfig, ExecThreadConfig, PollThreadConfig,
};

#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error("Executor error {0}")]
    ExecutorError(#[from] ExecError),
    #[error("StdIo error")]
    StdIo(#[from] std::io::Error),
    #[error("Io Poller error")]
    IoPoller(#[from] PollError),
    #[error("Internal error {0}")]
    Internal(String),
}

#[derive(Builder, Clone)]
#[builder(build_fn(name = "build_impl"))]
pub struct ImputioRuntime {
    exec_thread_id: ThreadId,
    shutdown: (Sender<()>, Receiver<()>),
    // TODO: allow optional config to reserve certain cores
    // to not be used by imputio runtime
    reserved_core_ids: Vec<usize>,
    runtime_core: Option<usize>,
    cfg: ExecConfig,
    state: Arc<AtomicBool>,
}

impl ImputioRuntimeBuilder {
    pub fn build(&mut self) -> Result<ImputioRuntime, RuntimeError> {
        if self.shutdown.is_none() {
            self.shutdown = Some(flume::bounded(1));
        }

        if self.exec_thread_id.is_none() {
            self.exec_thread_id = Some(thread::current().id());
        }

        if self.cfg.is_none() {
            tracing::trace!("exec cfg is none, using defaults");
            let exec_cfg = ExecConfig::default().with_shutdown(Arc::new(AtomicBool::new(false)));
            self.cfg = Some(exec_cfg);
        }

        if self.reserved_core_ids.is_none() {
            self.reserved_core_ids = Some(vec![]);
        }

        if self.runtime_core.is_none() {
            self.runtime_core = Some(None);
        }

        self.state = Some(Arc::new(AtomicBool::new(false)));

        let rt = self
            .build_impl()
            .map_err(|e| RuntimeError::Internal(format!("Invalid runtime builder args {e:}")))?;
        Ok(rt)
    }

    /// Allow user to supply fine-grain control over
    /// which threads get pinned to what core, what their
    /// names are, stack size, etc.
    pub fn with_exec_config(mut self, mut cfg: ExecConfig) -> Self {
        cfg = cfg.with_shutdown(Arc::new(AtomicBool::new(false)));
        self.cfg = Some(cfg);
        self
    }

    /// Allow user to specify that all available cores should be
    /// used. With this mechanism there will essentially be two threads
    /// pinned to each available core, one for an
    /// [`crate::executor::handle::Executor`] actor and one for the
    /// I/O poller ([`crate::io::Poller`]) actor that is spawned by
    /// the given [`crate::executor::handle::Executor`] actor.
    /// Issues warning if config could cause issues
    pub fn use_all_available_cores_with_executor_per_core(mut self) -> Self {
        if let Some(Some(_core)) = &self.runtime_core {
            tracing::warn!(
                "Pinning runtime core with exec-cfg options \
                may cause deadlock in I/O intensive applications"
            );
        }

        let available_cores = core_affinity::get_core_ids().unwrap_or_default();

        let execs = available_cores
            .iter()
            .map(|core| {
                let poller_cfg = PollThreadConfig::default()
                    .with_core_affinity(core.id)
                    .with_name(format!("imputio-io-{:?}", core.id));
                ExecThreadConfig::default()
                    .with_core_affinity(core.id)
                    .with_poll_thread_cfg(poller_cfg)
                    .with_name(format!("imputio-exec-{:?}", core.id))
            })
            .collect::<Vec<_>>();

        let exec_cfg = if execs.is_empty() {
            ExecConfig::default().with_shutdown(Arc::new(AtomicBool::new(false)))
        } else {
            ExecConfig::default()
                .with_cfg(execs)
                .with_shutdown(Arc::new(AtomicBool::new(false)))
        };

        self.cfg = Some(exec_cfg);
        self
    }

    /// Allow caller to specify one or more reserved cores that should
    /// not be used for pinning threads to.
    ///
    /// # Note:
    /// This must not be used with the build option provided by
    /// [`ImputioRuntimeBuilder::use_all_available_cores_with_executor_per_core`].
    /// Can be used in conjunction with fine-grain executor control
    /// provided by the [`ImputioRuntimeBuilder::with_exec_config`]
    /// option, but the user is expected to not provide conflicting
    /// setup as no checks will be made to avoid this
    pub fn with_reserved_cores(mut self, reserved: Vec<usize>) -> Self {
        self.reserved_core_ids = Some(reserved);
        self
    }

    /// Allow caller to pin the main runtime thread to a specific
    /// core. Issues warning if config could cause issues
    ///
    /// #Note:
    /// Caller is responsible for ensuring this option does
    /// not confict with [`ImputioRuntimeBuilder::with_reserved_cores`]
    /// if supplied
    pub fn pin_main_runtime_to_core(mut self, runtime_core: usize) -> Self {
        if let Some(cfg) = &self.cfg {
            if !cfg.is_empty() {
                tracing::warn!(
                    "Pinning runtime core with exec-cfg options \
                may cause deadlock in I/O intensive applications"
                );
            }
        }

        self.runtime_core = Some(Some(runtime_core));
        self
    }
}

impl Default for ImputioRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl Debug for ImputioRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImputioRuntime")
            .field("exec_thread_id", &self.exec_thread_id)
            .field("shutdown", &self.shutdown)
            .field("reserved_core_ids", &self.reserved_core_ids)
            .field("cfg", &self.cfg)
            .finish()
    }
}

impl ImputioRuntime {
    pub fn builder() -> ImputioRuntimeBuilder {
        ImputioRuntimeBuilder::default()
            .with_exec_config(ExecConfig::default().with_shutdown(Arc::new(AtomicBool::new(false))))
    }

    pub fn new() -> Self {
        ImputioRuntime::builder()
            .build()
            .unwrap_or_else(|_| panic!("Unable to build imputio runtime"))
    }

    pub fn replace_shutdown_notifier(
        mut self,
        shutdown_tx: Sender<()>,
        shutdown_rx: Receiver<()>,
    ) -> Self {
        self.shutdown = (shutdown_tx, shutdown_rx);

        self
    }

    pub fn run(&mut self) -> Sender<()> {
        let (tx, rx) = self.shutdown.clone();

        // dont re-run executor threads if already running
        if self.state.swap(true, Ordering::SeqCst) {
            return tx;
        }

        if let Some(core_id) = self.runtime_core {
            tracing::trace!("Pinning main runtime to core {core_id:}");
            core_affinity::set_for_current(CoreId { id: core_id });
        }

        let exec = Arc::new(ExecHandleCoordinator::initialize(self.cfg.clone()));
        crate::EXECUTOR.store(exec);

        let handle = std::thread::spawn(move || {
            loop {
                let exec = crate::EXECUTOR.load();
                exec.poll();
                match rx.try_recv() {
                    Ok(()) => {
                        tracing::warn!("Shutdown notice received");
                        break;
                    }
                    Err(TryRecvError::Disconnected) => {
                        // static var used to control only issuing this warning once
                        // as this may be intentionally dropped by the caller
                        static LOG: AtomicBool = AtomicBool::new(false);

                        if !LOG.swap(true, Ordering::SeqCst) {
                            // TODO should this be trace level?
                            tracing::warn!("tx end of shutdown notice dropped");
                        }
                        // TODO break-on-disconnect feature or build cfg option?
                    }
                    _ => {}
                }
            }
            let exec = crate::EXECUTOR.load();
            exec.shutdown();
        });
        self.exec_thread_id = handle.thread().id();
        tracing::trace!("Running ImputioRuntime with cfg: {:?}", self.cfg);
        tx
    }

    pub fn block_on<F, R>(mut self, fut: F) -> R
    where
        F: Future<Output = R> + Send + 'static,
        R: Send + 'static,
    {
        // make sure runtime is running prior to spawning. Atomic bool
        // tracks if runtime is already in running state
        self.run();
        spawn_blocking!(fut)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::{Future, IntoFuture},
        pin::Pin,
        task::{Context, Poll},
    };

    use crate::{spawn, spawn_blocking, Priority};

    /// The [`std::future::Future`] impl for [`ExampleTask`]
    /// returns [`Poll::Pending`] until internal count reaches 5
    #[derive(Debug)]
    pub struct ExampleTask {
        pub count: usize,
    }

    #[tracing::instrument]
    pub async fn async_fn() -> usize {
        let task: ExampleTask = ExampleTask { count: 1 };
        task.into_future().await
    }

    impl Future for ExampleTask {
        type Output = usize;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.count += 1;
            if self.count >= 5 {
                Poll::Ready(self.count)
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    /// Increments internal count once, then
    /// returns it (see below impl of
    /// [`std::future::IntoFuture`])
    #[derive(Debug, Default)]
    pub struct OtherExampleTask {
        pub count: usize,
    }

    impl IntoFuture for OtherExampleTask {
        type Output = usize;

        type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;
        #[tracing::instrument]
        fn into_future(mut self) -> Self::IntoFuture {
            Box::pin(async move {
                self.count += 1;
                self.count
            })
        }
    }

    const EXPECTED_ASYNC_RET: usize = 5;
    const EXPECTED_EX_RET: usize = 5;
    const OTHER_EX_BASE: usize = 2;
    const EXPECTED_OTHER_EX_RET: usize = 3;

    #[test]
    fn test_spawn_simple() -> Result<(), Box<dyn std::error::Error>> {
        let tx = ImputioRuntime::new().run();
        // can be any value but choosing something below EXPECTED_EX_RET
        // so the task returns Poll::Pending a few times
        let one = ExampleTask { count: 2 };
        let two = OtherExampleTask {
            count: OTHER_EX_BASE,
        };

        let t_one = spawn!(one, Priority::High);
        let t_two = spawn!(async move { two.await }, Priority::Medium);
        let async_fn_task = spawn!(async { async_fn().await }, Priority::Low);

        let t_one_res = t_one.receiver().recv();
        let t_two_res = t_two.receiver().recv();
        let async_fn_res = async_fn_task.receiver().recv();

        assert_eq!(t_one_res, Ok(EXPECTED_EX_RET));
        assert_eq!(t_two_res, Ok(EXPECTED_OTHER_EX_RET));
        assert_eq!(async_fn_res, Ok(EXPECTED_ASYNC_RET));

        tx.send(()).expect("Failed to send shutdown");
        Ok(())
    }

    #[test]
    fn test_await_in_block_on_ctx() {
        ImputioRuntime::new().block_on(async move {
            let ex = OtherExampleTask {
                count: OTHER_EX_BASE,
            };
            let t_ex = ex.await;

            let async_fn_res = async_fn().await;

            assert_eq!(async_fn_res, EXPECTED_ASYNC_RET);

            assert_eq!(t_ex, EXPECTED_OTHER_EX_RET);
        });
    }

    #[test]
    #[ignore] 
    fn test_spawn_in_block_on_ctx() {
        ImputioRuntime::new().block_on(async move {
            // count does not matter here as this test
            // is checking for panics to arise when expected
            let ex = ExampleTask { count: 0 };

            let result = std::panic::catch_unwind(|| spawn_blocking!(ex));
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_builder() {
        let _rt = ImputioRuntimeBuilder::default()
            .exec_thread_id(thread::current().id())
            .build()
            .expect("Expected to succeed build");

        let _rt = ImputioRuntimeBuilder::default()
            .build()
            .expect("Expected to succeed build");

        // expect err here, calling build_impl() requires providing all member vars
        let _rt = ImputioRuntimeBuilder::default()
            .build_impl()
            .expect_err("Expected to fail build");

        // test calling through ImputioRuntime object
        let _rt = ImputioRuntime::builder()
            .shutdown(flume::bounded(1))
            .build()
            .expect("Expected to succeed build");
    }

    #[test]
    fn test_runtime_pin_core_build() {
        use libc::{cpu_set_t, sched_getaffinity, CPU_ISSET};

        // test assumes the runner has at least 4 cores!
        let cpu = 3;

        let tx = ImputioRuntimeBuilder::default()
            .pin_main_runtime_to_core(cpu)
            .exec_thread_id(thread::current().id())
            .build()
            .expect("Expected to succeed build")
            .run();

        let mut set = unsafe { std::mem::zeroed::<cpu_set_t>() };
        assert!(unsafe { sched_getaffinity(0, std::mem::size_of::<cpu_set_t>(), &mut set) } == 0);
        assert!(unsafe { CPU_ISSET(cpu, &set) });
        tx.send(()).expect("Failed to send shutdown");
    }
}
