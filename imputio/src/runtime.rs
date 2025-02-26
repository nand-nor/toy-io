use std::{
    fmt::Debug,
    future::Future,
    num::NonZero,
    sync::Arc,
    thread::{self, ThreadId},
};

use core_affinity::CoreId;
use derive_builder::Builder;
use thiserror::Error;

use crate::{
    executor::handle::{ExecError, ExecHandleCoordinator},
    io::PollError,
    spawn_blocking, ExecConfig,
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
    // TODO: allow config to pin certain threads to cores
    #[builder(default)]
    num_cores: usize,
    // TODO: allow spawning multiple exec threads
    #[builder(default = 1)]
    _num_exec_threads: usize,
    exec_thread_id: ThreadId,
    shutdown: (flume::Sender<()>, flume::Receiver<()>),
    // TODO: allow optional config to pin certain threads to cores
    _core_ids: Vec<CoreId>,
}

impl ImputioRuntimeBuilder {
    pub fn build(&mut self) -> Result<ImputioRuntime, RuntimeError> {
        if self.shutdown.is_none() {
            self.shutdown = Some(flume::bounded(1));
        }

        if self.num_cores.is_none() {
            self.num_cores = Some(
                thread::available_parallelism()
                    .unwrap_or(NonZero::new(1usize).unwrap())
                    .get(),
            );
        }

        if self._num_exec_threads.is_none() {
            self._num_exec_threads = Some(1usize);
        }

        if self.exec_thread_id.is_none() {
            self.exec_thread_id = Some(thread::current().id());
        }

        if self._core_ids.is_none() {
            self._core_ids = Some(
                core_affinity::get_core_ids()
                    .ok_or_else(|| RuntimeError::Internal("core ids not available".to_string()))?,
            );
        }

        let rt = self
            .build_impl()
            .map_err(|e| RuntimeError::Internal(format!("Invalid runtime builder args {e:}")))?;
        Ok(rt)
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
            .field("num_cores", &self.num_cores)
            .field("_num_exec_threads", &self._num_exec_threads)
            .field("exec_thread_id", &self.exec_thread_id)
            .field("shutdown", &self.shutdown)
            .finish()
    }
}

impl ImputioRuntime {
    pub fn builder() -> ImputioRuntimeBuilder {
        ImputioRuntimeBuilder::default()
    }

    pub fn new() -> Self {
        Self {
            _core_ids: core_affinity::get_core_ids()
                .unwrap_or_else(|| panic!("Unable to pull core IDs on platform")),
            num_cores: thread::available_parallelism()
                .unwrap_or(NonZero::new(1usize).unwrap())
                .get(),
            _num_exec_threads: 1,
            exec_thread_id: thread::current().id(), // replaced later with exec thread id
            shutdown: flume::bounded(1),
        }
    }

    pub fn replace_shutdown_notifier(
        mut self,
        shutdown_tx: flume::Sender<()>,
        shutdown_rx: flume::Receiver<()>,
    ) -> Self {
        self.shutdown = (shutdown_tx, shutdown_rx);

        self
    }

    // TODO: Populate the Exec thread config struct
    // based on builder options
    fn populate_exec_cfg(&self) -> ExecConfig {
        ExecConfig::default()
    }

    pub fn run(&mut self) -> flume::Sender<()> {
        let (tx, rx) = self.shutdown.clone();

        let exec_cfg = self.populate_exec_cfg();

        let exec = Arc::new(ExecHandleCoordinator::initialize(exec_cfg));
        crate::EXECUTOR.store(exec);

        let handle = std::thread::spawn(move || loop {
            let exec = crate::EXECUTOR.load();
            exec.poll();
            if let Ok(()) = rx.try_recv() {
                tracing::debug!("Shutdown notice received");
                break;
            }
        });
        self.exec_thread_id = handle.thread().id();
        tracing::info!(
            "Running ImputioRuntime on thread id {:?}, num cores available: {:?}",
            self.exec_thread_id,
            self.num_cores
        );

        tx
    }

    pub fn block_on<F, R>(self, fut: F) -> R
    where
        F: Future<Output = R> + Send + 'static,
        R: Send + 'static,
    {
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
        ImputioRuntime::new().run();
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
            .num_cores(0usize)
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
}
