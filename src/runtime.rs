use std::{
    fmt::Debug,
    future::Future,
    marker::PhantomData,
    num::NonZero,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle, ThreadId},
};

use crate::{spawn_blocking, Executor};

pub struct ImputioRuntime<S: 'static> {
    num_cores: usize,
    scheduler: PhantomData<S>,
    exec_thread_id: ThreadId,
    exec_thread_handle: Option<JoinHandle<()>>,
    shutdown_tx: flume::Sender<()>,
    shutdown_rx: flume::Receiver<()>,
}

impl<S: 'static> Debug for ImputioRuntime<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImputioRuntime")
            .field("exec_thread_id", &self.exec_thread_id)
            .finish()
    }
}

impl Default for ImputioRuntime<Executor> {
    fn default() -> Self {
        Self::new()
    }
}

impl ImputioRuntime<Executor> {
    pub fn new() -> Self {
        let num_cores = thread::available_parallelism()
            .unwrap_or(NonZero::new(1usize).unwrap())
            .get();
        let (tx, rx) = flume::unbounded();

        Self {
            num_cores,
            scheduler: PhantomData,
            exec_thread_id: thread::current().id(),
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
        tracing::info!(
            "Running ImputioRuntime on thread id {:?}, num cores available: {:?}",
            self.exec_thread_id,
            self.num_cores
        );

        let tx = self.shutdown_tx.clone();
        let rx = self.shutdown_rx.clone();

        let handle = std::thread::spawn(move || loop {
            let mut exec = crate::EXECUTOR
                .get_or_init(|| Arc::new(Mutex::new(Box::new(Executor::initialize()))))
                .lock()
                .unwrap_or_else(|_| panic!("panic"));

            exec.poll();
            if let Ok(()) = rx.try_recv() {
                tracing::debug!("Shutdown notice received");
                break;
            }
        });

        self.exec_thread_handle = Some(handle);
        tx
    }

    pub fn block_on<J, T>(self, fut: J) -> T
    where
        J: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        spawn_blocking!(fut)
    }
}
