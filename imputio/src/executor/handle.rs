use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    thread::{self, park_timeout},
    time::Duration,
};

use core_affinity::CoreId;
#[cfg(feature = "fairness")]
use rand::{self, Rng};

use crate::{
    Priority,
    io::{Operation, PollError, PollHandle},
    task::{ImputioTask, ImputioTaskHandle},
};

use super::{ExecConfig, PollThreadConfig, ThreadConfig};

#[derive(thiserror::Error, Debug)]
pub enum ExecError {
    #[error("Actor receive error {0}")]
    ActorRcv(#[from] flume::RecvError),
    #[error("Actor send error {0}")]
    ActorSend(#[from] flume::SendError<Transaction>),
    #[error("StdIo error")]
    StdIo(#[from] std::io::Error),
    #[error("Io Poller error")]
    IoPoller(#[from] PollError),
}

type Result<T> = std::result::Result<T, ExecError>;
type Reply<T> = flume::Sender<T>;

/// [`Transaction`] enum defines the [`Executor`]
/// actor methods a handle ([`ExecHandle`]) to
/// an [`Executor`] object can execute
pub enum Transaction {
    Spawn {
        task: ImputioTask,
    },
    Poll,
    SubmitIoOp {
        op: Operation,
        reply: Reply<Result<()>>,
    },
}

/// The [`ExecHandleCoordinator`] provides a simple, global
/// entry point to a series of handles ([`ExecHandle`]) that
/// can be used to send  [`Transaction`] requests to one or
/// more [`Executor`] objects executing on their own thread.
/// Each [`Executor`] additionally spawns another thread to
/// run it's own I/O (epoll) actor, which is used to enqueue
/// I/O futures onto separate from the general purpose task
/// queue that each [`Executor`] actor is responsible for
/// polling
pub struct ExecHandleCoordinator {
    handles: Vec<ExecHandle>,
    index: AtomicUsize,
    shutdown: Arc<AtomicBool>,
}

impl ExecHandleCoordinator {
    pub fn new() -> Self {
        let (tx, _rx) = flume::bounded(1);
        Self {
            handles: vec![ExecHandle::new(tx)],
            index: AtomicUsize::new(0),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// # Panics
    ///
    /// This function will panic if the provided configuration
    /// is invalid or otherwise unsupported by the OS
    #[inline]
    pub fn initialize(exec_cfg: ExecConfig) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let handles = exec_cfg
            .exec_thread_config
            .iter()
            .map(|cfg| {
                ExecHandle::initialize(
                    cfg.thread_cfg.clone(),
                    cfg.poll_thread_cfg.clone(),
                    Arc::clone(&shutdown),
                )
                .expect("Failed to initialize with provided parameters")
            })
            .collect::<Vec<_>>();
        Self {
            handles,
            index: AtomicUsize::new(0),
            shutdown,
        }
    }

    #[inline]
    pub fn spawn<F, T>(&self, fut: F, priority: Priority) -> ImputioTaskHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let index = (self.index.load(Ordering::SeqCst) + 1) % self.handles.len();
        self.index.store(index, Ordering::SeqCst);

        self.handles[index].spawn(fut, priority)
    }

    /// Skips enqueuing the future to task queue, creates a new task and blocks until
    /// it completes
    pub(crate) fn spawn_blocking<F, T>(&self, fut: F) -> T
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        // TODO spawn blocking only on local thread
        let spawn_fut: Pin<Box<dyn Future<Output = T> + Send>> = Box::pin(fut);
        // note: priority for blocking tasks does not apply
        let task = ImputioTask::new(spawn_fut, Priority::High);
        task.run()
    }

    #[inline]
    pub fn poll(&self) {
        let index = (self.index.load(Ordering::SeqCst) + 1) % self.handles.len();
        self.index.store(index, Ordering::SeqCst);

        self.handles[index].poll();
    }

    pub fn submit_io_op(&self, op: Operation) -> Result<()> {
        let index = (self.index.load(Ordering::SeqCst) + 1) % self.handles.len();
        self.index.store(index, Ordering::SeqCst);

        self.handles[index].submit_io_op(op)
    }

    pub fn shutdown(&self) {
        self.shutdown.swap(true, Ordering::SeqCst);
    }
}

/// Defines handle to the [`Executor`]
/// actor
#[derive(Clone)]
struct ExecHandle {
    tx: flume::Sender<Transaction>,
}

impl ExecHandle {
    pub(crate) fn new(tx: flume::Sender<Transaction>) -> Self {
        Self { tx }
    }

    #[inline]
    fn initialize(
        exec_cfg: ThreadConfig,
        poller_cfg: PollThreadConfig,
        shutdown: Arc<AtomicBool>,
    ) -> Result<Self> {
        let (tx, rx) = flume::unbounded();
        let handle = Self::new(tx);
        let exec = Executor::initialize(rx, poller_cfg, shutdown, exec_cfg.parking)?;

        thread::Builder::new()
            .name(exec_cfg.thread_name)
            .stack_size(exec_cfg.stack_size)
            .spawn(move || {
                if let Some(core) = exec_cfg.core_id {
                    core_affinity::set_for_current(CoreId { id: core });
                }
                exec.run()
            })?;
        Ok(handle)
    }

    #[inline]
    pub fn spawn<F, T>(&self, fut: F, priority: Priority) -> ImputioTaskHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = flume::bounded(1);

        let spawn_fut = Box::pin(async move {
            let res = fut.await;
            tx.send(res).ok();
        });

        let task = ImputioTask::new(spawn_fut, priority);

        let op = Transaction::Spawn { task };
        self.tx
            .send(op)
            .inspect_err(|e| tracing::error!("spawn op failure {e:}"))
            .ok();

        ImputioTaskHandle { receiver: rx }
    }

    /// Skips enqueuing the future to task queue, creates a new task and blocks until
    /// it completes
    #[allow(unused)] // TODO spawn blocking only on local thread
    pub(crate) fn spawn_blocking<F, T>(&self, fut: F) -> T
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let spawn_fut: Pin<Box<dyn Future<Output = T> + Send>> = Box::pin(fut);
        // note: priority for blocking tasks does not apply
        let task = ImputioTask::new(spawn_fut, Priority::High);
        task.run()
    }

    #[inline]
    pub fn poll(&self) {
        self.tx
            .send(Transaction::Poll)
            .inspect_err(|e| tracing::error!("poll op failure {e:}"))
            .ok();
    }

    pub fn submit_io_op(&self, op: Operation) -> Result<()> {
        let (reply, rx) = flume::bounded(1);

        self.tx
            .send(Transaction::SubmitIoOp { op, reply })
            .inspect_err(|e| tracing::error!("submit io op failure {e:}"))
            .ok();

        rx.recv()?
    }
}

/// Simple "priority" executor with higher priority
/// queues dequeuing tasks first. Simple io operations
/// can be separately handled by submitting registration
/// requests to the epoll-backed [`PollHandle`] object
#[derive(Debug)]
pub struct Executor {
    tasks: [VecDeque<crate::task::ImputioTask>; 5],
    rx: flume::Receiver<Transaction>,
    poll_handle: PollHandle,
    shutdown: Arc<AtomicBool>,
    parking: Option<u64>,
}

impl Executor {
    #[inline]
    fn initialize(
        rx: flume::Receiver<Transaction>,
        cfg: PollThreadConfig,
        shutdown: Arc<AtomicBool>,
        parking: Option<u64>,
    ) -> Result<Self> {
        let handle = if cfg!(feature = "io") {
            let io_thread_parking = cfg.clone().parking();
            let (actor, handle) =
                PollHandle::initialize(cfg.poll_cfg, Arc::clone(&shutdown), io_thread_parking)?;

            thread::Builder::new()
                .name(cfg.thread_cfg.thread_name)
                .stack_size(cfg.thread_cfg.stack_size)
                .spawn(move || {
                    if let Some(core) = cfg.thread_cfg.core_id {
                        core_affinity::set_for_current(CoreId { id: core });
                    }
                    actor.run()
                })?;
            handle
        } else {
            let (tx, _rx) = flume::bounded(1);
            PollHandle::new(tx)
        };

        Ok(Self {
            poll_handle: handle,
            rx,
            tasks: [(); 5].map(|_| VecDeque::new()),
            shutdown,
            parking,
        })
    }

    #[inline]
    fn spawn(&mut self, task: ImputioTask) {
        self.tasks[task.priority as usize].push_back(task);
    }

    /// Iterates through all of the task vectors (starting with highest priority)
    /// and returns the first one found where is_some is true. Otherwise returns None
    #[inline]
    pub fn get_task(&mut self) -> Option<ImputioTask> {
        #[cfg(feature = "fairness")]
        {
            // implement some randomized "fairness" by starting iteration for tasks
            // in the middle (roughly) of the priority queue randomly FIXME
            let mut rng = rand::rng();
            if rng.random() {
                return self.tasks[2..].iter_mut().find_map(|q| q.pop_front());
            }
        }

        self.tasks.iter_mut().find_map(|q| q.pop_front())
    }

    #[inline]
    pub fn poll(&mut self) {
        if cfg!(feature = "io") {
            if let Err(e) = self.poll_handle.poll() {
                tracing::error!("Error polling io poller {e:}");
            }
        }

        if let Some(mut task) = self.get_task() {
            match task.poll_task() {
                Poll::Ready(_val) => {}
                Poll::Pending => {
                    self.tasks[task.priority as usize].push_back(task);
                }
            };
        }
    }

    #[inline]
    fn push_to_poller(&self, token: Operation) -> Result<()> {
        self.poll_handle.submit_op(token)?;
        Ok(())
    }

    #[allow(unused)]
    fn poll_handle(&self) -> PollHandle {
        self.poll_handle.clone()
    }

    #[inline]
    fn run(mut self) {
        tracing::trace!(
            "Running executor actor thread id: {:?}, name  {:?}",
            thread::current().id(),
            thread::current().name()
        );

        loop {
            if let Ok(event) = self.rx.try_recv() {
                match event {
                    Transaction::Poll => self.poll(),
                    Transaction::Spawn { task } => self.spawn(task),
                    Transaction::SubmitIoOp { op, reply } => {
                        let _ = reply.send(self.push_to_poller(op));
                    }
                };
            } else if let Some(park) = self.parking {
                park_timeout(Duration::from_millis(park));
            }
            if self.shutdown.load(Ordering::SeqCst) {
                break;
            }
        }

        tracing::trace!("executor id {:?} shutting down", thread::current().id());
    }
}
