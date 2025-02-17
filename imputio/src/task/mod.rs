pub mod waker;
pub(crate) use waker::{ImputioTaskHeader, ImputioWaker};

use std::{
    fmt::Debug,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, RawWaker, Waker},
};

use tracing::instrument;

use crate::Priority;

pub struct ImputioTask<T> {
    pub(crate) future: Pin<Box<dyn Future<Output = T> + Send>>,
    pub(crate) waker: Arc<Waker>,
    pub(crate) priority: Priority,
    inner: std::ptr::NonNull<ImputioTaskHeader>,
}

unsafe impl<T> Send for ImputioTask<T> {}
unsafe impl<T> Sync for ImputioTask<T> {}

//impl<T> std::panic::UnwindSafe for ImputioTask<T> {}
//unsafe impl<T> Sync for ImputioTask<T>{}

impl<T> Debug for ImputioTask<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImputioTask")
            .field("priority", &self.priority)
            .finish()
    }
}

impl<T> ImputioTask<T> {
    pub fn new(future: Pin<Box<dyn Future<Output = T> + Send>>, priority: Priority) -> Self {
        let (waker, inner) = ImputioWaker::new_with_inner_ptr();

        Self {
            future,
            waker,
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

    //pub fn detach(&self) {}

    #[instrument]
    pub fn poll(&mut self) -> Poll<T> {
        let waker = self.waker.clone();
        let mut cx = Context::from_waker(&waker);
        self.future.as_mut().poll(&mut cx)
    }

    // runt the future this task is holding to completion (blocking)
    #[instrument]
    pub fn run(mut self) -> T {
        tracing::debug!("running ImputioTask to completion");

        let waker = self.waker.clone();
        let mut cx = Context::from_waker(&waker);

        loop {
            match self.future.as_mut().poll(&mut cx) {
                std::task::Poll::Ready(val) => return val,
                std::task::Poll::Pending => {
                    //
                    // Poll::Pending
                    // park it then wake it?
                    //  ImputioWaker::park( unsafe {&*(self.inner.as_ptr())});
                    //  cx.waker().wake_by_ref();
                    unsafe {
                        while (*self.inner.as_ptr())
                            .parked
                            .load(std::sync::atomic::Ordering::SeqCst)
                        {
                            // loop until its not parked
                            tracing::debug!("Parked?");
                        }
                        (*self.inner.as_ptr())
                            .parked
                            .store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            }
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
