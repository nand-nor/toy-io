use std::{collections::VecDeque, future::Future, pin::Pin};

#[cfg(feature = "fairness")]
use rand::{self, Rng};

use crate::{
    io::{Operation, PollCfg, PollError, PollHandle},
    task::{ImputioTask, ImputioTaskHandle},
    Priority,
};

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

impl crate::RuntimeScheduler for ExecHandle {
    fn spawn<F, T>(&self, fut: F) -> ImputioTaskHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        ExecHandle::spawn(self, fut, Priority::Medium)
    }

    fn poll(&self) {
        ExecHandle::poll(self)
    }

    fn priority_spawn<F, T>(&self, fut: F, priority: Priority) -> ImputioTaskHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        ExecHandle::spawn(self, fut, priority)
    }

    fn poller_handle(&self) -> PollHandle {
        ExecHandle::get_io_poll_handler(self)
    }
}

type Result<T> = std::result::Result<T, ExecError>;
type Reply<T> = flume::Sender<T>;

/// Simple "priority" executor with higher priority
/// queues dequeuing tasks first. Simple io operations
/// can be separately handled by submitting registration
/// requests to the epoll-backed [`PollHandle`] object
#[derive(Debug)]
pub struct Executor {
    tasks: [VecDeque<crate::task::ImputioTask>; 5],
    rx: flume::Receiver<Transaction>,
    poll_handle: PollHandle,
}

impl Executor {
    fn initialize(rx: flume::Receiver<Transaction>, poll_cfg: Option<PollCfg>) -> Self {
        tracing::debug!("Running executor thread");

        // FIXME: allow runtime with build options to specify size of poll cfg event queue
        let (actor, handle) = PollHandle::initialize(poll_cfg);
        std::thread::spawn(move || actor.run());
        Self {
            poll_handle: handle,
            rx,
            tasks: [(); 5].map(|_| VecDeque::new()),
        }
    }

    fn spawn(&mut self, task: ImputioTask) {
        self.tasks[task.priority as usize].push_back(task);
    }

    /// Iterates through all of the task vectors (starting with highest priority)
    /// and returns the first one found where is_some is true. Otherwise returns None
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

    pub fn poll(&mut self) {
        if let Some(mut task) = self.get_task() {
            match task.poll_task() {
                std::task::Poll::Ready(_val) => {}
                std::task::Poll::Pending => {
                    self.tasks[task.priority as usize].push_back(task);
                }
            };
        }

        if let Err(e) = self.poll_handle.poll() {
            tracing::error!("Error polling io poller {e:}");
        } else {
            self.poll_handle.process().ok();
        }
    }

    fn push_to_poller(&self, token: Operation) -> Result<()> {
        self.poll_handle.submit_op(token)?;
        Ok(())
    }

    fn poll_handle(&self) -> PollHandle {
        self.poll_handle.clone()
    }

    #[tracing::instrument]
    fn run(mut self) {
        while let Ok(event) = self.rx.recv() {
            match event {
                Transaction::Poll => self.poll(),
                Transaction::Spawn { task } => self.spawn(task),
                Transaction::GetPollHandle { reply } => {
                    let _ = reply.send(self.poll_handle());
                }
                Transaction::SubmitIoOp { op, reply } => {
                    let _ = reply.send(self.push_to_poller(op));
                }
            };
        }
    }
}

pub enum Transaction {
    Spawn {
        task: ImputioTask,
    },
    Poll,
    GetPollHandle {
        reply: Reply<PollHandle>,
    },
    SubmitIoOp {
        op: Operation,
        reply: Reply<Result<()>>,
    },
}

#[derive(Clone)]
pub struct ExecHandle {
    tx: flume::Sender<Transaction>,
}

impl ExecHandle {
    fn new(tx: flume::Sender<Transaction>) -> Self {
        Self { tx }
    }

    pub fn initialize(poll_cfg: Option<PollCfg>) -> ExecHandle {
        let (tx, rx) = flume::unbounded();
        let exec = Executor::initialize(rx, poll_cfg);
        let handle = Self::new(tx);
        std::thread::spawn(move || exec.run());
        handle
    }

    pub fn spawn<F, T>(&self, fut: F, priority: Priority) -> ImputioTaskHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = flume::unbounded();

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
    pub(crate) fn spawn_blocking<F, T>(&self, fut: F, priority: Priority) -> T
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let spawn_fut: Pin<Box<dyn Future<Output = T> + Send>> = Box::pin(fut);
        let task = ImputioTask::new(spawn_fut, priority);
        task.run()
    }

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

        rx.recv().expect("Unable to submit io op")
    }

    pub fn get_io_poll_handler(&self) -> PollHandle {
        let (tx, rx) = flume::bounded(1);

        self.tx
            .send(Transaction::GetPollHandle { reply: tx })
            .inspect_err(|e| tracing::error!("get io handle op failure {e:}"))
            .ok();

        rx.recv().expect("Unable to get io poller handle")
    }
}
