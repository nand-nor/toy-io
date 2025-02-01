use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{RawWaker, RawWakerVTable, Waker},
};

#[derive(Debug)]
pub(crate) struct ImputioTaskHeader {
    pub(crate) parked: AtomicBool,
    pub(crate) state: Arc<AtomicBool>,
}

impl ImputioTaskHeader {
    pub(crate) fn new(state: Arc<AtomicBool>) -> Self {
        Self {
            parked: AtomicBool::new(false),
            state,
        }
    }
}

pub(crate) struct ImputioWaker;

impl ImputioWaker {
    pub(crate) const VTABLE: RawWakerVTable =
        RawWakerVTable::new(Self::clone, Self::wake, Self::wake_by_ref, Self::drop);

    unsafe fn clone(raw_waker: *const ()) -> RawWaker {
        tracing::trace!("clone");
        let cloned_waker = Arc::from_raw(raw_waker as *mut ImputioTaskHeader);
        std::mem::forget(cloned_waker.clone());
        RawWaker::new(Arc::into_raw(cloned_waker) as *const (), &Self::VTABLE)
    }

    unsafe fn wake(raw_waker: *const ()) {
        tracing::trace!("wake");
        let inner = unsafe { Arc::from_raw(raw_waker as *const ImputioTaskHeader) };

        if !inner.state.swap(true, Ordering::AcqRel) {
            Self::unpark(&inner);
        }
        std::mem::forget(inner);
    }

    unsafe fn wake_by_ref(raw_waker: *const ()) {
        tracing::trace!("wake by ref");
        let inner = &*(raw_waker as *const ImputioTaskHeader);
        if !inner.state.swap(true, Ordering::AcqRel) {
            Self::unpark(inner);
        }
        let _ = inner;
    }

    unsafe fn drop(raw_waker: *const ()) {
        tracing::trace!("drop");
        let raw_waker = Arc::from_raw(raw_waker as *const ImputioTaskHeader);
        drop(raw_waker);
    }

    #[allow(unused)]
    pub fn clone_inner(raw_waker: *const ()) -> *const ImputioTaskHeader {
        let cloned_waker = unsafe { Arc::from_raw(raw_waker as *mut ImputioTaskHeader) };
        std::mem::forget(cloned_waker.clone());
        Arc::into_raw(cloned_waker)
    }

    pub fn unpark(inner: &ImputioTaskHeader) {
        tracing::trace!("unpark");
        inner
            .parked
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    #[allow(unused)]
    pub fn park(inner: &ImputioTaskHeader) {
        tracing::trace!("park");
        inner
            .parked
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn new_with_inner_ptr(state: Arc<AtomicBool>) -> (Arc<Waker>, *const ImputioTaskHeader) {
        let inner = ImputioTaskHeader::new(state);
        let inner = Arc::new(inner);
        let inner_clone = Arc::into_raw(inner.clone());
        let raw_waker = RawWaker::new(inner_clone as *const _, &ImputioWaker::VTABLE);
        let waker = Arc::new(unsafe { Waker::from_raw(raw_waker) });
        (waker, Arc::into_raw(inner))
    }
}
