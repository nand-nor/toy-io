use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
};

use crate::{
    task::{ImputioTask, ImputioTaskHandle},
    Priority,
};

/// Simple "priority" executor with higher priority
/// queues dequeuing tasks first
pub struct Executor {
    tasks: [VecDeque<crate::task::ImputioTask<()>>; 5],
}

unsafe impl Sync for Executor {}
unsafe impl Send for Executor {}

impl Default for Executor {
    fn default() -> Self {
        Executor::initialize()
    }
}

impl Executor {
    pub(crate) fn initialize() -> Self {
        Self {
            tasks: [(); 5].map(|_| VecDeque::new()),
        }
    }

    fn spawn<T: Send + 'static>(
        &mut self,
        fut: impl Future<Output = T> + Send + 'static,
        priority: Priority,
    ) -> ImputioTaskHandle<T> {
        let (tx, rx) = flume::unbounded();

        let spawn_fut: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async move {
            tx.send(fut.await).ok();
        });

        let task = ImputioTask::new(spawn_fut, priority);

        self.tasks[priority as usize].push_back(task);
        ImputioTaskHandle { receiver: rx }
    }

    /// Skips enqueuing the future to task queue, creates a new task and blocks until
    /// it completes
    pub(crate) fn spawn_blocking<T>(
        &mut self,
        fut: impl Future<Output = T> + Send + 'static,
        priority: Priority,
    ) -> T
    where
        T: Send + 'static,
    {
        let spawn_fut: Pin<Box<dyn Future<Output = T> + Send>> = Box::pin(fut);
        let task = ImputioTask::new(spawn_fut, priority);
        task.run()
    }

    /// Iterates through all of the task vectors (starting with highest priority)
    /// and returns the first one found where is_some is true. Otherwise returns None
    pub fn get_task(&mut self) -> Option<ImputioTask<()>> {
        self.tasks.iter_mut().find_map(|q| q.pop_front())
    }

    pub fn poll(&mut self) {
        // check first if any non-io specific futures have been enqueued
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
        .unwrap_or_else(|_| panic!("Unable to obtain executor lock for spawn"));
    exec.spawn(fut, priority)
}

pub fn imputio_spawn_blocking<F, T>(fut: F, priority: crate::Priority) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let mut exec = crate::EXECUTOR
        .lock()
        .unwrap_or_else(|_| panic!("Unable to obtain executor lock for spawn_blocking"));
    exec.spawn_blocking(fut, priority)
}
