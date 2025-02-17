use std::{
    sync::{atomic::AtomicBool, Arc},
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

    unsafe fn clone(raw_waker: *const ()) -> RawWaker {
        let cloned_waker = Arc::from_raw(raw_waker as *mut ImputioTaskHeader);
        std::mem::forget(cloned_waker.clone());
        RawWaker::new(Arc::into_raw(cloned_waker) as *const (), &Self::VTABLE)
    }

    unsafe fn wake(raw_waker: *const ()) {
        Self::unpark(&Arc::from_raw(raw_waker as *const ImputioTaskHeader));
    }

    unsafe fn wake_by_ref(raw_waker: *const ()) {
        Self::unpark(&*(raw_waker as *const ImputioTaskHeader));
    }

    unsafe fn drop(raw_waker: *const ()) {
        let raw_waker = Arc::from_raw(raw_waker as *const ImputioTaskHeader);
        drop(raw_waker);
    }

    #[allow(unused)]
    pub fn new_raw_waker() -> RawWaker {
        let inner = ImputioTaskHeader::default();
        let inner = Box::into_raw(Box::new(inner));
        RawWaker::new(inner as *const (), &Self::VTABLE)
    }

    #[allow(unused)]
    pub fn new_waker() -> Waker {
        unsafe { Waker::from_raw(Self::new_raw_waker()) }
    }

    #[allow(unused)]
    pub fn clone_inner(raw_waker: *const ()) -> *const ImputioTaskHeader {
        let cloned_waker = unsafe { Arc::from_raw(raw_waker as *mut ImputioTaskHeader) };
        std::mem::forget(cloned_waker.clone());
        Arc::into_raw(cloned_waker)
    }

    pub fn unpark(inner: &ImputioTaskHeader) {
        inner
            .parked
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    #[allow(unused)]
    pub fn park(inner: &ImputioTaskHeader) {
        inner
            .parked
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn new_with_inner_ptr() -> (Arc<Waker>, *const ImputioTaskHeader) {
        let inner = ImputioTaskHeader::default();

        let inner = Arc::new(inner);
        let inner_clone = Arc::into_raw(inner.clone());

        let raw_waker = RawWaker::new(inner_clone as *const _, &ImputioWaker::VTABLE);
        let waker = Arc::new(unsafe { Waker::from_raw(raw_waker) });
        (waker, Arc::into_raw(inner))
    }
}
