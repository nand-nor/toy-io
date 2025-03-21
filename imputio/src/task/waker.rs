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
}

impl Default for ImputioTaskHeader {
    fn default() -> Self {
        Self {
            parked: AtomicBool::new(false),
        }
    }
}

pub(crate) struct ImputioWaker;

impl ImputioWaker {
    pub(crate) const VTABLE: RawWakerVTable =
        RawWakerVTable::new(Self::clone, Self::wake, Self::wake_by_ref, Self::drop);

    #[inline]
    unsafe fn clone(raw_waker: *const ()) -> RawWaker {
        let cloned_waker = Arc::from_raw(raw_waker as *mut ImputioTaskHeader);
        std::mem::forget(cloned_waker.clone());
        RawWaker::new(Arc::into_raw(cloned_waker) as *const (), &Self::VTABLE)
    }

    #[inline]
    unsafe fn wake(raw_waker: *const ()) {
        Self::unpark(&Arc::from_raw(raw_waker as *const ImputioTaskHeader));
    }

    #[inline]
    unsafe fn wake_by_ref(raw_waker: *const ()) {
        Self::unpark(&*(raw_waker as *const ImputioTaskHeader));
    }

    #[inline]
    unsafe fn drop(raw_waker: *const ()) {
        let raw_waker = Arc::from_raw(raw_waker as *const ImputioTaskHeader);
        drop(raw_waker);
    }

    #[inline]
    pub fn unpark(inner: &ImputioTaskHeader) {
        inner.parked.store(false, Ordering::SeqCst);
    }

    #[allow(unused)]
    pub fn park(inner: &ImputioTaskHeader) {
        inner.parked.store(true, Ordering::SeqCst);
    }

    #[inline]
    pub fn new_waker() -> Waker {
        let inner = ImputioTaskHeader::default();
        let inner = Arc::new(inner);
        let inner_clone = Arc::into_raw(inner.clone());

        let raw_waker = RawWaker::new(inner_clone as *const _, &ImputioWaker::VTABLE);
        // FIXME: Safety docs
        unsafe { Waker::from_raw(raw_waker) }
    }

    #[inline]
    pub fn new_waker_inner_ptr() -> (Waker, *const ImputioTaskHeader) {
        (
            Self::new_waker(),
            Arc::into_raw(Arc::new(ImputioTaskHeader::default())),
        )
    }
}
