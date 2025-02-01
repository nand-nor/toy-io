pub mod waker;

pub(crate) use waker::{ImputioTaskHeader, ImputioWaker};

use futures_lite::FutureExt;
use std::{
    fmt::Debug,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, RawWaker, Waker},
};

use tracing::instrument;

use crate::Priority;

pub struct ImputioTask<T: Send + 'static> {
    pub(crate) future: Pin<Box<dyn Future<Output = T> + Send>>,
    pub(crate) waker: Arc<Waker>,
    pub(crate) priority: Priority,
    state: Arc<AtomicBool>,
    inner: std::ptr::NonNull<ImputioTaskHeader>,
}

unsafe impl<T: Send + 'static> Send for ImputioTask<T> {}
unsafe impl<T: Send + 'static> Sync for ImputioTask<T> {}

impl<T: Send + 'static> Debug for ImputioTask<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImputioTask")
            .field("priority", &self.priority)
            .finish()
    }
}

use std::sync::atomic::{AtomicBool, Ordering};

impl<T: Send + 'static> ImputioTask<T> {
    pub fn new(future: Pin<Box<dyn Future<Output = T> + Send>>, priority: Priority) -> Self {
        let state = Arc::new(AtomicBool::new(false));
        let (waker, inner) = ImputioWaker::new_with_inner_ptr(Arc::clone(&state));
        Self {
            future,
            waker,
            state,
            priority,
            inner: unsafe { std::ptr::NonNull::new_unchecked(inner as *mut ImputioTaskHeader) },
        }
    }

    pub fn waker(&self) -> Arc<Waker> {
        self.waker.clone()
    }

    pub fn raw_waker(&self) -> RawWaker {
        let inner_clone = self.inner.as_ptr();
        RawWaker::new(inner_clone as *const _, &ImputioWaker::VTABLE)
    }

    #[instrument]
    pub fn poll_task(&mut self) -> Poll<T> {
        let waker = self.waker.clone();
        let mut cx = Context::from_waker(&waker);

        self.poll(&mut cx)
    }

    // run the future this task is holding to completion (blocking)
    #[instrument]
    pub fn run(mut self) -> T {
        tracing::debug!("running ImputioTask to completion");
        loop {
            let waker = self.waker.clone();
            let mut cx = Context::from_waker(&waker);
            match self.poll(&mut cx) {
                Poll::Ready(val) => return val,
                Poll::Pending => {}
            }
        }
    }
}

impl<T: Send + 'static> Future for ImputioTask<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let s = self.get_mut();
        match Pin::new(&mut s.future).poll(cx) {
            Poll::Ready(output) => {
                s.state.store(true, Ordering::Release);
                Poll::Ready(output)
            }
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
