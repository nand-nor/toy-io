#[macro_export]
macro_rules! spawn {
    ($fut:expr) => {
        spawn!($fut, $crate::Priority::Low)
    };
    ($fut:expr, $priority:expr) => {
        $crate::spawn($fut, $priority)
    };
}

#[macro_export]
macro_rules! spawn_blocking {
    ($fut:expr) => {
        $crate::spawn_blocking($fut)
    };
}
