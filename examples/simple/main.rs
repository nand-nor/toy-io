use std::{
    future::{Future, IntoFuture},
    pin::Pin,
    task::{Context, Poll},
};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use imputio::{spawn, spawn_blocking, Executor, ImputioRuntime, Priority};

#[derive(Debug)]
pub struct ExampleTask {
    pub count: usize,
}

#[tracing::instrument]
pub async fn async_fn() -> usize {
    let task: ExampleTask = ExampleTask { count: 1 };
    task.into_future().await
}

#[derive(Debug, Default)]
pub struct OtherExampleTask {
    pub count: usize,
}

impl Future for ExampleTask {
    type Output = usize;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.count += 1;
        // in this example the future will always
        // return pending until count is at least 5
        if self.count >= 5 {
            Poll::Ready(self.count)
        } else {
            tracing::debug!("Waking task...");
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

impl OtherExampleTask {
    pub fn new() -> Self {
        Self { count: 0 }
    }
}

// Provides toy example of another method for implementing
// the Future trait for a struct
impl IntoFuture for OtherExampleTask {
    type Output = usize;

    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;
    #[tracing::instrument]
    fn into_future(mut self) -> Self::IntoFuture {
        Box::pin(async move {
            self.count += 1;
            self.count
        })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    ImputioRuntime::<Executor>::new().run();

    let one = ExampleTask { count: 2 };

    let two = ExampleTask { count: 1 };

    let three = OtherExampleTask { count: 10 };

    let t_one = spawn!(one);

    let async_fn_task = spawn!(async { async_fn().await }, Priority::Medium);

    let t_three = spawn!(async move { three.await }, Priority::Low);

    let res_vec = vec![
        t_one.receiver().recv(),
        t_three.receiver().recv(),
        async_fn_task.receiver().recv(),
    ];

    res_vec.into_iter().for_each(|r| {
        tracing::info!("Result: {:?}", r);
    });

    let t_two = spawn_blocking!(two, Priority::BestEffort);

    tracing::info!("Blocking result: {:?}", t_two);

    Ok(())
}
