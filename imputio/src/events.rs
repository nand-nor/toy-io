use std::collections::{HashMap, HashSet, VecDeque};
use tracing::instrument;

use crate::{spawn, Priority};

#[derive(thiserror::Error, Debug)]
pub enum EventBusError {
    #[error("Resource exhaustion")]
    ResourceExhaustion,
    #[error("Actor error {0}")]
    ActorError(String),
    #[error("ChannelSendError")]
    ChannelSendError,
    #[error("ChannelRcvError")]
    ChannelRcvError(#[from] flume::RecvError),
    #[error("Handle Error {0}")]
    HandleError(String),
}

type Reply<T> = flume::Sender<std::result::Result<T, EventBusError>>;
type Result<T> = std::result::Result<T, EventBusError>;

pub enum EventBusMessage<T> {
    Subscribe {
        reply: Reply<u32>,
    },
    Unsubscribe {
        id: u32,
        reply: Reply<Option<VecDeque<T>>>,
    },
    PushEvent {
        event: T,
        reply: Reply<()>,
    },
    Poll {
        id: u32,
        reply: Reply<Option<T>>,
    },
    BatchPoll {
        id: u32,
        reply: Reply<Vec<T>>,
    },
    #[cfg(feature = "delay-delete")]
    CleanUpBgTask,
}

/// The `EventBus` is an actor that allows 
/// subscribing to and polling for events
pub struct EventBus<T: Send + Clone> {
    receiver: flume::Receiver<EventBusMessage<T>>,
    event_queues: HashMap<u32, VecDeque<T>>,
    ids: HashSet<u32>,
    #[cfg(feature = "delay-delete")]
    recycle: Vec<u32>,
}

impl<T: Send + Clone> std::fmt::Debug for EventBus<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus").finish()
    }
}

impl<T: Send + Clone> EventBus<T> {
    pub fn new(receiver: flume::Receiver<EventBusMessage<T>>) -> Self {
        tracing::debug!("New event bus created");
        let mut ids = HashSet::default();
        ids.extend((1..u16::MAX as u32).collect::<Vec<_>>());
        Self {
            event_queues: HashMap::new(),
            ids,
            receiver,
            #[cfg(feature = "delay-delete")]
            recycle: vec![],
        }
    }

    pub async fn new_with_handle() -> Result<&'static EventHandle<T>>
    where
        T: 'static,
    {
        let (tx, rx) = flume::unbounded();
        let bus = Self::new(rx);

        // This will run the bus actor as a detached task, when it drops out of scope
        let _task = spawn!(bus.run(), Priority::High);

        // create temp handle to get valid subscription ID (only need to do this once)
        // otherwise events wont be enqueued for the handle's unique ID
        let mut handle = EventHandle { tx, id: 0 };

        // reassign handle to what is returned from the subscribe future
        handle = handle
            .subscribe()
            .await
            .map_err(|e| EventBusError::ActorError(e.to_string()))?;

        // leak the handle to the 'static lifetime
        let handle: &'static EventHandle<T> = Box::leak(Box::new(handle));
        Ok(handle)
    }

    pub async fn new_with_unsubscribed_handle() -> Result<&'static EventHandle<T>>
    where
        T: 'static,
    {
        let (tx, rx) = flume::unbounded();
        let bus = Self::new(rx);
        // run the bus actor as a detached task
        let _task = spawn!(bus.run(), Priority::High);

        // ID of 0 indicates the handle is unsubscribed
        let handle = EventHandle { tx, id: 0 };

        // leak the handle to the 'static lifetime
        let handle: &'static EventHandle<T> = Box::leak(Box::new(handle));
        Ok(handle)
    }

    pub async fn run(mut self) {
        while let Ok(event) = self.receiver.recv_async().await {
            match event {
                EventBusMessage::Subscribe { reply } => reply.send(self.subscribe().await).ok(),
                EventBusMessage::Unsubscribe { id, reply } => {
                    reply.send(self.unsubscribe(id).await).ok()
                }
                EventBusMessage::PushEvent { event, reply } => {
                    reply.send(self.send(event).await).ok()
                }
                EventBusMessage::Poll { id, reply } => reply.send(self.poll(id).await).ok(),
                EventBusMessage::BatchPoll { id, reply } => {
                    reply.send(self.batch_poll(id).await).ok()
                }
                #[cfg(feature = "delay-delete")]
                EventBusMessage::CleanUpBgTask => Some(self.cleanup().await),
            };
        }
    }

    #[instrument]
    async fn subscribe(&mut self) -> Result<u32> {
        let new_id = *self
            .ids
            .iter()
            .next()
            .ok_or(EventBusError::ResourceExhaustion)?;
        self.event_queues.insert(new_id, VecDeque::new());
        self.ids.take(&new_id);
        Ok(new_id)
    }

    /// When delay-delete feature is not enabled, callers must unsubscribe
    /// before dropping their handle, otherwise events will keep going
    /// into their queue, which will never get drained if the handle is
    /// dropped
    #[cfg(not(feature = "delay-delete"))]
    async fn unsubscribe(&mut self, id: u32) -> Result<Option<VecDeque<T>>> {
        if let Some(vec) = self.event_queues.remove_entry(&id) {
            // replace the id if it was in the queue
            self.ids.insert(id);
            Ok(Some(vec.1))
        } else {
            Ok(None)
        }
    }

    /// delay-delete feature will mark an event subscriber for deletion
    /// which will then be removed after some amount of time by a
    /// background task, however that task must be run separately
    #[cfg(feature = "delay-delete")]
    async fn unsubscribe(&mut self, id: u32) -> Result<Option<VecDeque<T>>> {
        self.recycle.push(id);
        Ok(None)
    }

    #[cfg(feature = "delay-delete")]
    async fn cleanup(&mut self) {
        tracing::debug!("Running event bus cleanup bg task");
        let cleanup = self.recycle.clone();
        self.recycle.clear();
        for id in cleanup.iter() {
            self.event_queues.remove(id);
        }
    }

    async fn send(&mut self, event: T) -> Result<()> {
        tracing::debug!("Received event");
        for (_, v) in self.event_queues.iter_mut() {
            v.push_back(event.clone());
        }
        Ok(())
    }

    #[instrument]
    async fn poll(&mut self, id: u32) -> Result<Option<T>> {
        let item = self.event_queues.get_mut(&id).or(None);
        if let Some(item) = item {
            Ok(item.pop_front())
        } else {
            Ok(None)
        }
    }

    #[instrument]
    async fn batch_poll(&mut self, id: u32) -> Result<Vec<T>> {
        let item = self.event_queues.get_mut(&id).or(None);
        let mut items = vec![];
        if let Some(item) = item {
            while let Some(i) = item.pop_front() {
                items.push(i);
            }
        }
        Ok(items)
    }
}

#[derive(Clone)]
pub struct EventHandle<T: Send + Clone> {
    pub tx: flume::Sender<EventBusMessage<T>>,
    pub id: u32,
}

impl<T: Send + Clone> std::fmt::Debug for EventHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventHandle").field("id", &self.id).finish()
    }
}

impl<T: Send + Clone> EventHandle<T> {
    #[instrument]
    pub async fn poll(&self) -> Result<Option<T>> {
        let (tx, rx) = flume::bounded(1);

        self.tx
            .send(EventBusMessage::Poll {
                id: self.id,
                reply: tx,
            })
            .map_err(|e| {
                tracing::error!("Send error: {e:}");
                EventBusError::ChannelSendError
            })?;

        rx.recv_async().await?
    }

    #[instrument]
    pub async fn subscribe(&self) -> Result<Self> {
        let (tx, rx) = flume::bounded(1);
        self.tx
            .send(EventBusMessage::Subscribe { reply: tx })
            .map_err(|e| {
                tracing::error!("Send error: {e:}");
                EventBusError::ChannelSendError
            })?;
        let new_id = rx.recv_async().await??;
        Ok(Self {
            id: new_id,
            tx: self.tx.clone(),
        })
    }

    pub async fn push_event(&self, event: T) -> Result<()> {
        let (tx, rx) = flume::bounded(1);
        self.tx
            .send(EventBusMessage::PushEvent { event, reply: tx })
            .map_err(|e| {
                tracing::error!("Send error: {e:}");
                EventBusError::ChannelSendError
            })?;

        rx.recv_async().await?
    }

    #[instrument]
    pub async fn unsubscribe(&self, id: u32) -> Result<Option<VecDeque<T>>> {
        let (tx, rx) = flume::bounded(1);
        self.tx
            .send(EventBusMessage::Unsubscribe { id, reply: tx })
            .map_err(|e| {
                tracing::error!("Send error: {e:}");
                EventBusError::ChannelSendError
            })?;
        rx.recv_async().await?
    }

    #[cfg(feature = "delay-delete")]
    pub async fn cleanup(&self) {
        self.tx
            .send(EventBusMessage::CleanUpBgTask)
            .map_err(|e| {
                tracing::error!("Send error: {e:}");
            })
            .ok();
    }

    pub fn id(&self) -> u32 {
        self.id
    }
}

impl<T: Send + Clone> Drop for EventHandle<T> {
    #[instrument]
    fn drop(&mut self) {
        // handle with id 0 does not need to be unsubbed when dropped
        if self.id != 0 {
            tracing::debug!("Unsubbing for handle id {:?}", self.id);
            let (tx, _rx) = flume::bounded(1);
            self.tx
                .send(EventBusMessage::Unsubscribe {
                    id: self.id,
                    reply: tx,
                })
                .map_err(|e| {
                    tracing::error!("Send error: {e:}");
                })
                .ok();
        }
    }
}

/// Note: this background task must be run is a separate thread
/// for now.
#[cfg(feature = "delay-delete")]
#[instrument]
pub async fn cleanup_bg_task<T: Send + Clone>(handle: EventHandle<T>) {
    use std::{thread, time::Duration};
    loop {
        tracing::trace!("Cleaning up");
        handle.cleanup().await;
        thread::yield_now();
        thread::park_timeout(Duration::from_secs(2));
    }
}

/// Provides wrapper around event poll loop, checking for an events that a handle is
/// subscribed to, and breaking the loop when the event match condition is met. Optionally
/// will send a notice when the loop is broken
pub async fn event_poll_matcher<T: Send + Clone, S: Send>(
    handle: &EventHandle<T>,
    matcher: impl Fn(&T) -> bool,
    notify: Option<(flume::Sender<S>, S)>,
) -> Result<()> {
    let id = handle.id();
    tracing::debug!("event-consumer-{id:}: dropping into poll loop");
    loop {
        let event = handle.poll().await;
        match event {
            Ok(Some(event)) => {
                // on event match, exit poll loop
                if matcher(&event) {
                    break;
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("Error processing event bus poll {e:}, exiting");
                break;
            }
        }
    }

    tracing::trace!("event-consumer-{id:}: exited poll loop");
    if let Some((notify, send)) = notify {
        if let Err(e) = notify.send(send) {
            tracing::error!("Error sending shutdown notice {e:}");
        }
    }
    Ok(())
}
