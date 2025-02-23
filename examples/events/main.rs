//! Simple example of using the event bus actor, provided in the
//! imputio-utils crate, with the imputio runtime

use tracing_subscriber::{EnvFilter, FmtSubscriber};

use imputio::{spawn, ImputioRuntime, Priority};

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
    ImputioRuntime::new().run();

    let task: imputio::ImputioTaskHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>> =
        spawn!(event_bus_example(), Priority::High);
    task.receiver().recv()??;

    Ok(())
}

async fn event_bus_example() -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    let (shutdown_tx, shutdown_rx) = flume::unbounded();

    // used to block shutdown waiting for event matcher functions to fire
    let tx_1 = shutdown_tx.clone();
    let tx_2 = shutdown_tx.clone();

    // creates a handle to the actor from which publisher
    // or subscriber handles can be created
    let handle = EventBusHandle::<Packet<'_>>::new_with_handle().await?;
    #[cfg(feature = "delay-delete")]
    {
        // if the example is compiled with the delay-delete feature flag, then
        // spawn a new thread to run the cleanup background task
        let garbage_bus = handle.clone();
        std::thread::spawn(move || imputio_utils::event_bus::cleanup_task(garbage_bus));
    }
    // create a subscriber handle
    let subscriber: SubHandle<Packet<'_>> = handle.get_subscriber().await?;
    // create publisher handles
    let publisher_one = handle.get_publisher().await;
    let publisher_two = handle.get_publisher().await;

    // Simulate some events; make sure there are events on the bus
    // before blocking waiting for them, otherwise the events will
    // never get onto the bus (because currently single threaded)
    simulate_packet_send_events(&publisher_one).await;

    // spawn a separate thread to simulate delayed packet send events
    std::thread::spawn(move || {
        let fut = async move {
            sleep_and_send_more_events(&publisher_two).await;
        };
        let task = imputio::spawn!(fut, Priority::High);
        task.spawn_await().ok();
    });

    // These matcher function examples are what will be used to trigger
    // exiting the poll loop that the spawned task drops into as part of
    // the execution of event_poll_matcher. The task, once it breaks from
    // the loop, will send a shutdown notice to indicate to any threads
    // blocking that an event match has been received
    let matcher_one = |event: &Packet| event.size == 0;
    let matcher_two = |event: &Packet| event._bytes == [0xbe, 0xef, 0xfa, 0xce];

    event_poll_matcher(&subscriber, matcher_one, Some((tx_1, ())))
        .await
        .ok();

    event_poll_matcher(&subscriber, matcher_two, Some((tx_2, ())))
        .await
        .ok();

    // expect two shut down notices from the event_poll_matcher methods
    // this will block from returning from main until received & processed
    // via the matcher function
    shutdown_rx.recv().ok();
    shutdown_rx.recv().ok();

    // unsubscribe from the handle when returned from event poll loop
    let id = subscriber.id();
    tracing::info!("Calling unsubscribe for subscriber id {id:}");
    if let Err(e) = subscriber.unsubscribe(id).await {
        tracing::error!("EventBusError on unsubscribe: {e:}");
    }

    // drop the handle to exit from the actor thread
    let _ = handle;

    Ok(())
}

/// Sends a few objects on the event bus, the last event will be used in a
/// match function to exit the event polling loop a spawned task is
/// executing
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

/// Sleeps and then sends a few objects on the event bus, one object is
/// known to provide a match for a match function that will be used to exit
/// the event polling loop a spawned task is executing
async fn sleep_and_send_more_events(handle: &PubHandle<Packet<'_>>) {
    std::thread::sleep(std::time::Duration::from_secs(5));
    tracing::info!("Waking up and publishing events...");
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
