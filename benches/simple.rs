//! Benchmarks of simple future

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use std::{
    future::{Future, IntoFuture},
    pin::Pin,
    task::{Context, Poll},
};

use imputio::{spawn, ImputioRuntime, Priority};

pub struct ExampleTask {
    pub count: usize,
}

#[tracing::instrument]
pub async fn async_fn() -> usize {
    let task: ExampleTask = ExampleTask { count: 1 };
    task.into_future().await
}

impl Future for ExampleTask {
    type Output = usize;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.count += 1;
        if self.count >= 5 {
            Poll::Ready(self.count)
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

async fn counter1() -> usize {
    let t = ExampleTask { count: 2 };
    let t = spawn!(t, Priority::High);
    let res = t.receiver().recv().expect("Failed to recv fut res");
    black_box(res)
}

async fn counter2() -> usize {
    let t = spawn!(async { async_fn().await }, Priority::High);
    let res = t.receiver().recv().expect("Failed to recv fut res");
    black_box(res)
}

fn test_simple1(criterion: &mut Criterion) {
    let mut rt = ImputioRuntime::builder()
        .pin_main_runtime_to_core(1)
        .exec_thread_id(std::thread::current().id())
        .build()
        .expect("Expected to succeed build");

    rt.run();

    criterion.bench_function("simple1", |b| {
        b.iter(|| rt.clone().block_on(async move { spawn!(counter1()) }))
    });
}

fn test_simple2(criterion: &mut Criterion) {
    let mut rt = ImputioRuntime::builder()
        .pin_main_runtime_to_core(1)
        .exec_thread_id(std::thread::current().id())
        .build()
        .expect("Expected to succeed build");

    rt.run();

    criterion.bench_function("simple2", |b| {
        b.iter(|| rt.clone().block_on(async move { spawn!(counter2()) }))
    });
}

criterion_group!(simple, test_simple1, test_simple2);

criterion_main!(simple);
