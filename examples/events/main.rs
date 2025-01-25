//! Simple example of using the event bus actor
//! with imputio runtime

use tracing_subscriber::{EnvFilter, FmtSubscriber};

use imputio::{
    event_poll_matcher, spawn, EventBus, EventHandle, Executor, ImputioRuntime, Priority,
};

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
    ImputioRuntime::<Executor>::new().run();

    let fut = async move {
        event_bus_example().await?;
        Ok(())
    };

    let task: imputio::ImputioTaskHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>> =
        spawn!(async move { fut.await }, Priority::High);
    task.receiver().recv()??;

    Ok(())
}

async fn event_bus_example() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    let (shutdown_tx, shutdown_rx) = flume::unbounded();

    // used to block shutdown waiting for event matcher functions to fire
    let tx_1 = shutdown_tx.clone();
    let tx_2 = shutdown_tx.clone();

    // creates a subscribed handle so events will enqueue onto the
    // queue associated with this handle's ID as soon as handle
    // is created. Use new_with_unsubscribed_handle for different behavior
    let handle: &'static EventHandle<Packet<'_>> =
        EventBus::<Packet<'_>>::new_with_handle().await?;

    // Show how to get new subscribed handled
    let handle_one = handle.subscribe().await?;

    // Simulate some events; make sure there are events on the bus
    // before blocking waiting for them, otherwise the events will
    // never get onto the bus (because currently single threaded)
    simulate_packet_send_events(&handle).await;

    #[cfg(feature = "delay-delete")]
    {
        // for now anything that may block the main thread needs to be spawned onto
        // a new thread
        let garbage_bus = handle.subscribe().await?;
        std::thread::spawn(move || {
            let fut = async move {
                imputio::events::cleanup_bg_task(garbage_bus).await;
            };

            imputio::spawn_blocking!(fut, Priority::BestEffort);
        });
    }

    // spawn two different matcher function examples that will send shutdown notice
    // once the event matches
    let matcher_one = |event: &Packet| event.size == 0;
    let matcher_two = |event: &Packet| event.size == 6;

    event_poll_matcher(&handle_one, matcher_one, Some((tx_1, ())))
        .await
        .ok();
    event_poll_matcher(&handle, matcher_two, Some((tx_2, ())))
        .await
        .ok();

    // drop the handles, this should unsubscribe them
    let _ = handle_one;
    let _ = handle;
    // expect two shut down notices from the event_poll_matcher methods
    // this will block from returning from main until received & processed
    // via the matcher function
    shutdown_rx.recv().ok();
    shutdown_rx.recv().ok();

    #[cfg(feature = "delay-delete")]
    {
        // user must terminate program manually when this feature
        // is enabled for this example. The loop is used to allow
        // logs showing the periodic cleanup task running
        loop {}
    }
    Ok(())
}

async fn simulate_packet_send_events(handle: &EventHandle<Packet<'_>>) {
    handle
        .push_event(Packet {
            _bytes: &[0xb0, 0x00, 0x00],
            size: 3,
        })
        .await
        .ok();
    handle
        .push_event(Packet {
            _bytes: &[0xb0, 0x00, 0x00, 0xee, 0xee, 0xff],
            size: 3,
        })
        .await
        .ok();

    handle
        .push_event(Packet {
            _bytes: &[0xbe, 0xee, 0xef],
            size: 6,
        })
        .await
        .ok();

    handle
        .push_event(Packet {
            _bytes: &[0x00],
            size: 0,
        })
        .await
        .ok();
}
