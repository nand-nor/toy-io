use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use imputio::ImputioRuntime;

#[derive(Debug)]
pub struct ExampleTask {
    pub count: usize,
    pub start: usize,
}

impl ExampleTask {
    pub fn new(start: usize) -> Self {
        Self {
            count: start,
            start,
        }
    }
}

impl Future for ExampleTask {
    type Output = usize;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // in this example the future will always
        // return pending until count is 0
        if self.count == 0 {
            Poll::Ready(self.start)
        } else {
            self.count = self.count.checked_sub(1).unwrap_or_default();
            tracing::info!("Waking task...");
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    ImputioRuntime::new().block_on(async move {
        block_on_example().await;
    });

    Ok(())
}

async fn block_on_example() {
    let one = ExampleTask::new(2);
    let two = ExampleTask::new(10);
    let result = one.await;
    tracing::info!("Result one: {result:}");
    let result = two.await;
    tracing::info!("Result two: {result:}");
}
