//! Simple example of using the event bus actor, provided in the
//! imputio-utils crate, with the imputio runtime

use tracing_subscriber::{EnvFilter, FmtSubscriber};

use imputio::{spawn, Executor, ImputioRuntime, Priority};

use imputio_utils::event_bus::{event_poll_matcher, EventBus, EventHandle};

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
        spawn!(fut, Priority::High);
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
    simulate_packet_send_events(handle).await;

    // Used to simulate additional, delayed packet send events on the event bus
    let handle_two: &'static EventHandle<Packet<'_>> = Box::leak(Box::new(handle.clone()));

    #[cfg(feature = "delay-delete")]
    {
        // if the example is compiled with the delay-delete feature flag, then
        // spawn a new thread to run the cleanup background task
        let garbage_bus = handle.subscribe().await?;
        std::thread::spawn(move || {
            let fut = async move {
                imputio_utils::event_bus::cleanup_task(garbage_bus).await;
            };
            // spawn it at best effort priority
            imputio::spawn_blocking!(fut, Priority::BestEffort);
        });
    }

    // These matcher function examples are what will be used to trigger
    // exiting the poll loop that the spawned task drops into as part of
    // the execution of event_poll_matcher. The task, once it breaks from
    // the loop, will send a shutdown notice to indicate to any threads
    // blocking that an event match has been received
    let matcher_one = |event: &Packet| event.size == 0;
    let matcher_two = |event: &Packet| event._bytes == [0xbe, 0xef, 0xfa, 0xce];

    event_poll_matcher(&handle_one, matcher_one, Some((tx_1, ())))
        .await
        .ok();

    // unsubscribe from the handle when returned from event poll loop
    let id = handle_one.id();
    if let Err(e) = handle_one.unsubscribe(id).await {
        tracing::error!("EventBusError on unsubscribe: {e:}");
    }

    // spawn a separate thread to simulate delayed packet send events
    std::thread::spawn(move || {
        let fut = async move {
            sleep_and_send_more_events(handle_two).await;
        };
        imputio::spawn_blocking!(fut, Priority::BestEffort);
    });

    event_poll_matcher(handle, matcher_two, Some((tx_2, ())))
        .await
        .ok();

    // expect two shut down notices from the event_poll_matcher methods
    // this will block from returning from main until received & processed
    // via the matcher function
    shutdown_rx.recv().ok();
    shutdown_rx.recv().ok();

    // dropping the handle will unsubscribe them the same as calling
    // unsubscribe
    let _ = handle;

    Ok(())
}

/// Sends a few objects on the event bus, the last event will be used in a
/// match function to exit the event polling loop a spawned task is
/// executing
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
            size: 6,
        })
        .await
        .ok();

    handle
        .push_event(Packet {
            _bytes: &[],
            size: 0,
        })
        .await
        .ok();
}

/// Sleeps and then sends a few objects on the event bus, one object is
/// known to provide a match for a match function that will be used to exit
/// the event polling loop a spawned task is executing
async fn sleep_and_send_more_events(handle: &'static EventHandle<Packet<'_>>) {
    std::thread::sleep(std::time::Duration::from_secs(5));
    tracing::info!("Waking up and publishing events...");
    handle
        .push_event(Packet {
            _bytes: &[0xb0, 0x00, 0x00],
            size: 3,
        })
        .await
        .ok();

    handle
        .push_event(Packet {
            _bytes: &[0x00],
            size: 1,
        })
        .await
        .ok();

    handle
        .push_event(Packet {
            _bytes: &[0xbe, 0xef, 0xfa, 0xce],
            size: 4,
        })
        .await
        .ok();
}
