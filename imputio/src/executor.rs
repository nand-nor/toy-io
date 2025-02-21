use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(feature = "fairness")]
use rand::{self, Rng};

use crate::{
    runtime::RuntimeScheduler,
    task::{ImputioTask, ImputioTaskHandle},
    Priority,
};

/// Simple "priority" executor with higher priority
/// queues dequeuing tasks first
pub struct Executor {
    tasks: [VecDeque<crate::task::ImputioTask>; 5],
}

unsafe impl Sync for Executor {}
unsafe impl Send for Executor {}

impl Default for Executor {
    fn default() -> Self {
        Executor::initialize()
    }
}

impl crate::RuntimeScheduler for Executor {
    fn spawn<F, T>(&mut self, fut: F) -> ImputioTaskHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        Executor::spawn(self, fut, Priority::Medium)
    }

    fn poll(&mut self) {
        Executor::poll(self)
    }

    fn priority_spawn<F, T>(&mut self, fut: F, priority: Priority) -> ImputioTaskHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        Executor::spawn(self, fut, priority)
    }
}

impl Executor {
    pub(crate) fn initialize() -> Self {
        Self {
            tasks: [(); 5].map(|_| VecDeque::new()),
        }
    }

    fn spawn<F, T>(&mut self, fut: F, priority: Priority) -> ImputioTaskHandle<T>
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
        self.tasks[priority as usize].push_back(task);

        ImputioTaskHandle { receiver: rx }
    }

    /// Skips enqueuing the future to task queue, creates a new task and blocks until
    /// it completes
    pub(crate) fn spawn_blocking<F, T>(&mut self, fut: F, priority: Priority) -> T
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let spawn_fut: Pin<Box<dyn Future<Output = T> + Send>> = Box::pin(fut);
        let task = ImputioTask::new(spawn_fut, priority);
        task.run()
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
    }
}

pub fn imputio_spawn<F, T>(fut: F, priority: crate::Priority) -> crate::ImputioTaskHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let mut exec = crate::EXECUTOR
        .lock()
        .unwrap_or_else(|_| panic!("Unable to obtain lock"));
    exec.priority_spawn(fut, priority)
}

/// #Panics
///
/// If spawn_blocking is called in a blocking context, the thread will
/// panic
pub fn imputio_spawn_blocking<F, T>(fut: F, priority: crate::Priority) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    // FLAG is used to trigger panics (to avoid blocking forever)
    static FLAG: AtomicBool = AtomicBool::new(false);

    if !FLAG.swap(true, Ordering::SeqCst) {
        let mut exec = crate::EXECUTOR
            .lock()
            .unwrap_or_else(|_| panic!("Unable to obtain lock"));
        let res = exec.spawn_blocking(fut, priority);
        FLAG.store(false, Ordering::SeqCst);
        res
    } else {
        panic!("Blocking twice on same thread will block forever")
    }
}
