//! Simple example of using advanced config options
//! for building the ImputioRuntime. This example
//! extends the existing event bus example by spawning
//! threads intentionally spamming futures onto the
//! runtime

use std::{num::NonZero, thread::sleep, time::Duration};

use tracing_subscriber::{EnvFilter, FmtSubscriber};

use imputio::{spawn, ExecConfig, ExecThreadConfig, ImputioRuntime, PollThreadConfig, Priority};

use imputio_utils::event_bus::{event_poll_matcher, EventBusHandle, PubHandle, SubHandle};

#[derive(Clone)]
struct Packet<'a> {
    _bytes: &'a [u8],
    size: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let num_cores = std::thread::available_parallelism()
        .unwrap_or(NonZero::new(1usize).unwrap())
        .get();

    let available_cores = core_affinity::get_core_ids().unwrap_or_default();

    tracing::info!(
        "{num_cores:} cores on machine, {:?} available",
        available_cores.len()
    );

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

    let mut rt = ImputioRuntime::builder()
        .with_exec_config(exec_cfg)
        .build()?;

    rt.run();

    let task: imputio::ImputioTaskHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>> =
        spawn!(future_spam_with_event_bus(), Priority::High);
    task.receiver().recv()??;

    Ok(())
}

async fn future_spam_with_event_bus(
) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    let (shutdown_tx, shutdown_rx) = flume::unbounded();

    let tx_1 = shutdown_tx.clone();
    let tx_2 = shutdown_tx.clone();
    let tx_3 = shutdown_tx.clone();

    let handle = EventBusHandle::<Packet<'_>>::new_with_handle().await?;
    #[cfg(feature = "delay-delete")]
    {
        // if the example is compiled with the delay-delete feature flag, then
        // spawn a new thread to run the cleanup background task
        let garbage_bus = handle.clone();
        std::thread::spawn(move || imputio_utils::event_bus::cleanup_task(garbage_bus));
    }

    let subscriber: SubHandle<Packet<'_>> = handle.get_subscriber().await?;
    let subscriber_two: SubHandle<Packet<'_>> = handle.get_subscriber().await?;
    let publisher_one = handle.get_publisher().await;
    let publisher_two = handle.get_publisher().await;
    let pub_one: &'static _ = Box::leak(Box::new(publisher_one.clone()));

    simulate_events(pub_one).await;

    let matcher = |event: &Packet| event._bytes == [0xca, 0xfe, 0xb0, 0xba];
    let sub_two = subscriber_two.clone();
    let fut = async move { event_poll_matcher(&sub_two, matcher, Some((tx_3, ()))).await };

    let task = spawn!(fut, Priority::BestEffort);

    let pub_two: &'static _ = Box::leak(Box::new(publisher_two.clone()));

    // spam another future that should exit the first poller
    std::thread::spawn(move || {
        sleep(Duration::from_secs(5));
        let task = spawn!(
            pub_two.publish_event(Packet {
                _bytes: &[0xca, 0xfe, 0xb0, 0xba],
                size: 4,
            }),
            Priority::High
        );

        task.spawn_await().ok();
    });

    let fut = async move {
        task.spawn_await().ok();
        // unsubscribe after the event matcher exists
        let id = subscriber_two.id();
        tracing::info!("Calling unsubscribe for subscriber id {id:}");
        if let Err(e) = subscriber_two.unsubscribe(id).await {
            tracing::error!("EventBusError on unsubscribe: {e:}");
        }
    };

    // spam a bunch of futures onto the runtime
    spam_event_futures(std::time::Duration::from_millis(100), pub_one).await;

    let _task = spawn!(fut, Priority::BestEffort);

    // spam some more
    simulate_events(pub_one).await;

    // spam yet more futures
    std::thread::spawn(move || {
        let fut = async move {
            simulate_more_events(std::time::Duration::from_secs(6), pub_two).await;
        };
        let task = spawn!(fut, Priority::High);
        task.spawn_await().ok();
    });

    let matcher_one = |event: &Packet| event.size == 0;
    let matcher_two = |event: &Packet| event._bytes == [0xbe, 0xef, 0xfa, 0xce];

    event_poll_matcher(&subscriber, matcher_one, Some((tx_1, ())))
        .await
        .ok();

    // spam a nother future that should exit the last poller
    std::thread::spawn(move || {
        sleep(Duration::from_secs(15));
        let task = spawn!(
            pub_two.publish_event(Packet {
                _bytes: &[0xbe, 0xef, 0xfa, 0xce],
                size: 4,
            }),
            Priority::High
        );

        task.spawn_await().ok();

        let task = spawn!(
            pub_two.publish_event(Packet {
                _bytes: &[0xca, 0xfe, 0xb0, 0xba],
                size: 4,
            }),
            Priority::High
        );

        task.spawn_await().ok();
    });

    event_poll_matcher(&subscriber, matcher_two, Some((tx_2, ())))
        .await
        .ok();

    // expect three shut down notices from the event_poll_matcher methods
    shutdown_rx.recv().ok();
    shutdown_rx.recv().ok();
    shutdown_rx.recv().ok();

    let id = subscriber.id();
    tracing::info!("Calling unsubscribe for subscriber id {id:}");
    if let Err(e) = subscriber.unsubscribe(id).await {
        tracing::error!("EventBusError on unsubscribe: {e:}");
    }

    // drop the handle to exit from the actor thread
    let _ = handle;

    Ok(())
}

/// Simulate some events as packets published on the bus
async fn simulate_events(handle: &'static PubHandle<Packet<'_>>) {
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

/// Introduces some delays in spawned event futures
async fn simulate_more_events(sleep_time: Duration, handle: &PubHandle<Packet<'_>>) {
    sleep(sleep_time);
    tracing::info!("spam thread publishing events...");
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
}

/// Spam a bunch of spawned futures
async fn spam_event_futures(sleep_time: Duration, handle: &'static PubHandle<Packet<'_>>) {
    sleep(sleep_time);

    for _i in 0..5 {
        let fut = async move {
            simulate_events(handle).await;
        };
        let _task = spawn!(fut, Priority::High);
    }

    sleep(sleep_time / 2);
    for _i in 0..5 {
        let fut = async move {
            simulate_events(handle).await;
        };
        let _task = spawn!(fut, Priority::High);
    }
}
