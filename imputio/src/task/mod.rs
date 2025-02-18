pub mod waker;

pub(crate) use waker::ImputioWaker;

use futures_lite::FutureExt;
use std::{
    fmt::Debug,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use tracing::instrument;

use crate::Priority;

pub struct ImputioTask<T> {
    pub(crate) future: Pin<Box<dyn Future<Output = T> + Send>>,
    pub(crate) waker: Waker,
    pub(crate) priority: Priority,
}

unsafe impl<T> Send for ImputioTask<T> {}
unsafe impl<T> Sync for ImputioTask<T> {}

impl<T> Debug for ImputioTask<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImputioTask")
            .field("priority", &self.priority)
            .finish()
    }
}

impl<T> ImputioTask<T> {
    pub fn new(future: Pin<Box<dyn Future<Output = T> + Send>>, priority: Priority) -> Self {
        Self {
            future,
            waker: ImputioWaker::new_waker(),
            priority,
        }
    }

    #[instrument]
    pub fn poll_task(&mut self) -> Poll<T> {
        let waker = self.waker.clone();
        let mut cx = Context::from_waker(&waker);
        self.poll(&mut cx)
    }

    /// run the future this task is holding to completion (blocking)
    #[instrument]
    pub fn run(mut self) -> T {
        tracing::debug!("running ImputioTask to completion");
        loop {
            match self.poll_task() {
                Poll::Ready(val) => return val,
                Poll::Pending => {}
            }
        }
    }
}

impl<T> Future for ImputioTask<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let s = self.get_mut();
        match Pin::new(&mut s.future).poll(cx) {
            Poll::Ready(output) => Poll::Ready(output),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub struct ImputioTaskHandle<T> {
    pub(crate) receiver: flume::Receiver<T>,
}

impl<T> ImputioTaskHandle<T> {
    pub fn spawn_await(self) -> Result<T, flume::RecvError> {
        self.receiver.recv()
    }

    pub fn receiver(self) -> flume::Receiver<T> {
        self.receiver
    }
}
