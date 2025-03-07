pub mod waker;

pub(crate) use waker::ImputioWaker;

use futures_lite::FutureExt;
use std::{
    fmt::Debug,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};

use crate::Priority;

pub struct InnerTask<T> {
    pub(crate) future: Pin<Box<dyn Future<Output = T> + Send>>,
}

unsafe impl<T> Send for InnerTask<T> {}
unsafe impl<T> Sync for InnerTask<T> {}

impl<T> InnerTask<T> {
    pub fn new(future: Pin<Box<dyn Future<Output = T> + Send>>) -> Self {
        Self { future }
    }

    pub fn run(&mut self, cx: &mut Context) -> T {
        loop {
            match self.poll(cx) {
                Poll::Ready(val) => return val,
                Poll::Pending => {}
            }
        }
    }
}

pub struct ImputioTask<T = ()> {
    inner_task: std::ptr::NonNull<()>,
    phantom: PhantomData<T>,
    pub(crate) waker: Waker,
    pub(crate) priority: Priority,
}
unsafe impl Send for ImputioTask {}
unsafe impl Sync for ImputioTask {}

impl Debug for ImputioTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImputioTask")
            .field("priority", &self.priority)
            .finish()
    }
}

impl<T> ImputioTask<T> {
    #[inline]
    pub fn new(future: Pin<Box<dyn Future<Output = T> + Send>>, priority: Priority) -> Self {
        let (waker, _inner) = ImputioWaker::new_waker_inner_ptr();
        let inner_task = Arc::into_raw(Arc::new(InnerTask::<T>::new(future)));
        Self {
            inner_task: unsafe { std::ptr::NonNull::new_unchecked(inner_task as *mut ()) },
            phantom: PhantomData,
            waker,
            priority,
        }
    }

    #[inline]
    pub fn poll_task(&mut self) -> Poll<T> {
        let waker = self.waker.clone();
        let mut cx = Context::from_waker(&waker);
        let inner_task = unsafe { &mut *(self.inner_task.as_ptr() as *mut InnerTask<_>) };
        inner_task.poll(&mut cx)
    }

    /// run the future this task is holding to completion (blocking)
    #[inline]
    pub fn run(self) -> T {
        tracing::debug!("running ImputioTask to completion");
        let waker = self.waker.clone();
        let mut cx = Context::from_waker(&waker);
        let inner_task = unsafe { &mut *(self.inner_task.as_ptr() as *mut InnerTask<_>) };
        inner_task.run(&mut cx)
    }
}

impl<T> Future for ImputioTask<T>
where
    T: Send + 'static + std::marker::Unpin,
{
    type Output = <InnerTask<T> as Future>::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let s = self.get_mut();
        let inner_task = unsafe { &mut *(s.inner_task.as_ptr() as *mut InnerTask<Self::Output>) };
        inner_task.poll(cx)
    }
}

impl<T> Future for InnerTask<T> {
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
    pub fn blocking_await(self) -> Result<T, flume::RecvError> {
        self.receiver.recv()
    }

    pub async fn async_await(&self) -> Result<T, flume::RecvError> {
        self.receiver.recv_async().await
    }

    pub fn receiver(self) -> flume::Receiver<T> {
        self.receiver
    }
}
