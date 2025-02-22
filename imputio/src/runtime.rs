use std::{
    fmt::Debug,
    future::Future,
    marker::PhantomData,
    net::TcpListener,
    num::NonZero,
    thread::{self, JoinHandle, ThreadId},
};

use crate::{
    io::{Operation, PollError, PollHandle},
    spawn_blocking, ImputioTaskHandle, Priority,
};

pub trait RuntimeScheduler {
    fn spawn<F, T>(&mut self, fut: F) -> ImputioTaskHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static;

    fn priority_spawn<F, T>(&mut self, fut: F, priority: Priority) -> ImputioTaskHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static;

    fn poll(&mut self);

    fn poller_handle(&self) -> PollHandle;
}

pub struct ImputioRuntime<S: RuntimeScheduler + 'static> {
    num_cores: usize,
    phantom: PhantomData<S>,
    exec_thread_id: ThreadId,
    exec_thread_handle: Option<JoinHandle<()>>,
    shutdown_tx: flume::Sender<()>,
    shutdown_rx: flume::Receiver<()>,
}

impl<S: RuntimeScheduler + 'static> Default for ImputioRuntime<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: RuntimeScheduler + 'static> Debug for ImputioRuntime<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImputioRuntime")
            .field("exec_thread_id", &self.exec_thread_id)
            .finish()
    }
}

impl<S: RuntimeScheduler + 'static> ImputioRuntime<S> {
    pub fn new() -> Self {
        let num_cores = thread::available_parallelism()
            .unwrap_or(NonZero::new(1usize).unwrap())
            .get();
        let (tx, rx) = flume::unbounded();

        Self {
            num_cores,
            phantom: PhantomData,
            exec_thread_id: thread::current().id(), // replaced later with exec thread id
            exec_thread_handle: None,
            shutdown_tx: tx,
            shutdown_rx: rx,
        }
    }

    pub fn with_shutdown_notifier(
        mut self,
        shutdown_tx: flume::Sender<()>,
        shutdown_rx: flume::Receiver<()>,
    ) -> Self {
        self.shutdown_tx = shutdown_tx;
        self.shutdown_rx = shutdown_rx;

        self
    }

    pub fn run(&mut self) -> flume::Sender<()> {
        let tx = self.shutdown_tx.clone();
        let rx = self.shutdown_rx.clone();

        let handle = std::thread::spawn(move || loop {
            let mut exec = crate::EXECUTOR
                .lock()
                .unwrap_or_else(|_| panic!("Unable to obtain lock"));

            exec.poll();
            if let Ok(()) = rx.try_recv() {
                tracing::debug!("Shutdown notice received");
                break;
            }
        });
        self.exec_thread_id = handle.thread().id();
        self.exec_thread_handle = Some(handle);
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
        spawn_blocking!(fut, Priority::High)
    }

    pub fn with_tcp_listeners(
        self,
        listeners: Vec<(TcpListener, Option<flume::Sender<mio::event::Event>>)>,
    ) -> Self {
        use mio::Interest;

        for (listener, notify) in listeners {
            let token = Operation::RegistrationTcpAdd {
                fd: listener,
                interest: Interest::READABLE
                    .add(Interest::WRITABLE)
                    .add(Interest::PRIORITY),
                notify,
            };

            let exec = crate::EXECUTOR
                .lock()
                .unwrap_or_else(|_| panic!("Unable to obtain lock"));
            if let Err(e) = exec.push_to_poller(token) {
                tracing::error!("Error pushing to uring {e:}");
            }
        }

        self
    }

    pub fn add_operation_to_io_poller(&mut self, _token: Operation) -> Result<(), PollError> {
        // TODO
        Ok(())
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

    use crate::{spawn, spawn_blocking, Executor, Priority};

    /// Returns Poll::Pending until
    /// internal count reaches 5
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
    /// returns it (see impl for IntoFuture)
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
        ImputioRuntime::<Executor>::new().run();
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
        ImputioRuntime::<Executor>::new().block_on(async move {
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
        ImputioRuntime::<Executor>::new().block_on(async move {
            // count does not matter here as this test
            // is checking for panics to arise when expected
            let ex = ExampleTask { count: 0 };

            // spawning a blocking task within the block_on context should panic
            // if we use #[should_panic], because there is a single global executor
            // object, that can cause other tests to fail
            let result = std::panic::catch_unwind(|| spawn_blocking!(ex, Priority::High));
            assert!(result.is_err());
        });
    }
}
