//! Benchmarks comparing spawning on different runtime builds

use criterion::{criterion_group, criterion_main, Criterion};

use imputio::{spawn, ExecConfig, ExecThreadConfig, ImputioRuntime, PollThreadConfig};
use imputio_utils::event_bus::{event_poll_matcher, EventBusHandle, PubHandle, SubHandle};

#[derive(Clone)]
struct Packet<'a> {
    _bytes: &'a [u8],
    size: usize,
}

async fn event_bus_example() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    let (shutdown_tx, shutdown_rx) = flume::unbounded();
    let tx_1 = shutdown_tx.clone();
    let tx_2 = shutdown_tx.clone();

    let handle = EventBusHandle::<Packet<'_>>::new_with_handle().await?;
    let subscriber: SubHandle<Packet<'_>> = handle.get_subscriber().await?;
    let publisher_one = handle.get_publisher().await;
    let publisher_two = handle.get_publisher().await;

    simulate_packet_send_events(&publisher_one).await;

    std::thread::spawn(move || {
        imputio::spawn_blocking!(async move {
            sleep_and_send_more_events(&publisher_two).await;
        });
    });

    let matcher_one = |event: &Packet| event.size == 0;
    let matcher_two = |event: &Packet| event._bytes == [0xbe, 0xef, 0xfa, 0xce];

    event_poll_matcher(&subscriber, matcher_one, Some((tx_1, ())))
        .await
        .ok();

    event_poll_matcher(&subscriber, matcher_two, Some((tx_2, ())))
        .await
        .ok();

    shutdown_rx.recv().ok();
    shutdown_rx.recv().ok();

    let id = subscriber.id();
    subscriber
        .unsubscribe(id)
        .await
        .expect("EventBusError on unsubscribe: {e:}");
    let _ = handle;

    Ok(())
}

async fn simulate_packet_send_events(handle: &PubHandle<Packet<'_>>) {
    handle
        .publish_event(Packet {
            _bytes: &[0xb0, 0x00, 0x00],
            size: 3,
        })
        .await
        .ok();
    handle
        .publish_event(Packet {
            _bytes: &[0xb0, 0x00, 0x00, 0xee, 0xee, 0xff],
            size: 6,
        })
        .await
        .ok();

    handle
        .publish_event(Packet {
            _bytes: &[],
            size: 0,
        })
        .await
        .ok();
}

async fn sleep_and_send_more_events(handle: &PubHandle<Packet<'_>>) {
    std::thread::sleep(std::time::Duration::from_secs(1));
    handle
        .publish_event(Packet {
            _bytes: &[0xb0, 0x00, 0x00],
            size: 3,
        })
        .await
        .ok();

    handle
        .publish_event(Packet {
            _bytes: &[0x00],
            size: 1,
        })
        .await
        .ok();

    handle
        .publish_event(Packet {
            _bytes: &[0xbe, 0xef, 0xfa, 0xce],
            size: 4,
        })
        .await
        .ok();
}

fn test_simple_rt(criterion: &mut Criterion) {
    let mut rt = ImputioRuntime::new();
    rt.run();

    criterion.bench_function("simple_rt", |b| {
        b.iter(|| {
            rt.clone()
                .block_on(async move { spawn!(event_bus_example()) })
        })
    });
}

fn test_default_rt(criterion: &mut Criterion) {
    let mut rt = ImputioRuntime::builder()
        .build()
        .expect("Failed to build runtime");
    rt.run();

    criterion.bench_function("default_rt", |b| {
        b.iter(|| {
            rt.clone()
                .block_on(async move { spawn!(event_bus_example()) })
        })
    });
}

fn test_complex_rt(criterion: &mut Criterion) {
    let available_cores = core_affinity::get_core_ids().unwrap_or_default();
    let mut start = 0;
    if !available_cores.is_empty() {
        core_affinity::set_for_current(available_cores[0]);
        start = 1;
    }

    let execs = available_cores[start..]
        .iter()
        .map(|core| {
            let poller_cfg = PollThreadConfig::default()
                .with_core_affinity(core.id)
                .with_name(format!("imputio-io-{:?}", core.id));
            ExecThreadConfig::default()
                .with_core_affinity(core.id)
                .with_poll_thread_cfg(poller_cfg)
                .with_name(format!("imputio-exec-{:?}", core.id))
        })
        .collect::<Vec<_>>();

    let exec_cfg = if execs.is_empty() {
        ExecConfig::default()
    } else {
        ExecConfig::default().with_cfg(execs)
    };

    let (shutdown_tx, shutdown_rx) = flume::unbounded();
    let rx = shutdown_rx.clone();

    let mut rt = ImputioRuntime::builder()
        .with_exec_config(exec_cfg)
        .shutdown((shutdown_tx, rx))
        .build()
        .expect("Failed to create runtime");

    rt.run();

    criterion.bench_function("complex_rt", |b| {
        b.iter(|| {
            rt.clone()
                .block_on(async move { spawn!(event_bus_example()) })
        })
    });
}

criterion_group!(
    runtime_config,
    test_simple_rt,
    test_default_rt,
    test_complex_rt
);

criterion_main!(runtime_config);
