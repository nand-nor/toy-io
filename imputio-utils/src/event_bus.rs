/// Simple actor/handle for providing an event bus that
/// users of the imputio-utils crate can leverage for
/// asynchronous operations that require something like a
/// pub/sub architecture
use flume::SendError;
use std::collections::{HashMap, HashSet, VecDeque};
use tracing::instrument;

use imputio::{spawn, Priority};

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

impl<T> From<SendError<T>> for EventBusError {
    fn from(_value: SendError<T>) -> Self {
        EventBusError::ChannelSendError
    }
}

type SubId = u32;
const NULL_SUBID: SubId = 0;

type Result<T> = std::result::Result<T, EventBusError>;
type Reply<T> = flume::Sender<Result<T>>;

enum EventBusMessage<T> {
    Subscribe {
        reply: Reply<SubId>,
    },
    Unsubscribe {
        id: SubId,
    },
    PushEvent {
        event: T,
        reply: Reply<()>,
    },
    Poll {
        id: SubId,
        reply: Reply<Option<T>>,
    },
    BatchPoll {
        id: SubId,
        reply: Reply<Vec<T>>,
    },
    #[cfg(feature = "delay-delete")]
    CleanUpBgTask,
}

impl<T> std::fmt::Debug for EventBusMessage<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Subscribe { reply } => f.debug_struct("Subscribe").field("reply", reply).finish(),
            Self::Unsubscribe { id } => f.debug_struct("Unsubscribe").field("id", id).finish(),
            Self::PushEvent { reply, .. } => {
                f.debug_struct("PushEvent").field("reply", reply).finish()
            }
            Self::Poll { id, reply } => f
                .debug_struct("Poll")
                .field("id", id)
                .field("reply", reply)
                .finish(),
            Self::BatchPoll { id, reply } => f
                .debug_struct("BatchPoll")
                .field("id", id)
                .field("reply", reply)
                .finish(),
            #[cfg(feature = "delay-delete")]
            Self::CleanUpBgTask => write!(f, "CleanUpBgTask"),
        }
    }
}

/// The [`EventBus`] is an actor that allows
/// publishing and subscribing to events
pub struct EventBus<T: Send + Clone> {
    receiver: flume::Receiver<EventBusMessage<T>>,
    event_queues: HashMap<SubId, VecDeque<T>>,
    ids: HashSet<SubId>,
    #[cfg(feature = "delay-delete")]
    recycle: Vec<SubId>,
}

impl<T: Send + Clone> std::fmt::Debug for EventBus<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus").finish()
    }
}

impl<T: Send + Clone> EventBus<T> {
    fn new(receiver: flume::Receiver<EventBusMessage<T>>) -> Self {
        tracing::debug!("New event bus created");
        let mut ids = HashSet::default();
        ids.extend((1..u16::MAX as SubId).collect::<Vec<_>>());
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
        let mut handle = EventHandle { tx, id: NULL_SUBID };

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
        let handle = EventHandle { tx, id: NULL_SUBID };

        // leak the handle to the 'static lifetime
        let handle: &'static EventHandle<T> = Box::leak(Box::new(handle));
        Ok(handle)
    }

    pub async fn run(mut self) {
        while let Ok(event) = self.receiver.recv_async().await {
            match event {
                EventBusMessage::Subscribe { reply } => reply.send(self.subscribe().await).ok(),
                EventBusMessage::Unsubscribe { id } => {
                    self.unsubscribe(id).await;
                    None
                }
                EventBusMessage::PushEvent { event, reply } => {
                    reply.send(self.send(event).await).ok()
                }
                EventBusMessage::Poll { id, reply } => reply.send(self.poll(id).await).ok(),
                EventBusMessage::BatchPoll { id, reply } => {
                    reply.send(self.batch_poll(id).await).ok()
                }
                #[cfg(feature = "delay-delete")]
                EventBusMessage::CleanUpBgTask => {
                    self.cleanup().await;
                    None
                }
            };
        }
    }

    #[instrument]
    async fn subscribe(&mut self) -> Result<SubId> {
        let new_id = *self
            .ids
            .iter()
            .next()
            .ok_or(EventBusError::ResourceExhaustion)?;
        self.event_queues.insert(new_id, VecDeque::new());
        self.ids.take(&new_id);
        Ok(new_id)
    }

    /// When delay-delete feature is not enabled, callers should either
    /// unsubscribe or directly drop the handle when event subscription
    /// is no longer needed. This will ensure that events do not continue
    /// to accumulate in the event queue allocated for the handle
    #[cfg(not(feature = "delay-delete"))]
    async fn unsubscribe(&mut self, id: SubId) {
        if let Some(_vec) = self.event_queues.remove_entry(&id) {
            // replace the id if it was in the queue
            self.ids.insert(id);
        }
    }

    /// delay-delete feature will mark an event subscriber for deletion
    /// which will then be removed after some amount of time by a
    /// background task, however that task must be run separately
    #[cfg(feature = "delay-delete")]
    async fn unsubscribe(&mut self, id: SubId) {
        tracing::debug!("Pushing ID {id:} to unsubscribe list");
        self.recycle.push(id);
    }

    #[cfg(feature = "delay-delete")]
    async fn cleanup(&mut self) {
        let cleanup = self.recycle.clone();
        self.recycle.clear();
        for id in cleanup.iter() {
            tracing::info!("Cleaning up events from id {id:}");
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
    /// poll to return the top of the event queue for the subscriber, if
    /// there is an event
    async fn poll(&mut self, id: SubId) -> Result<Option<T>> {
        let item = self.event_queues.get_mut(&id).or(None);
        if let Some(item) = item {
            Ok(item.pop_front())
        } else {
            Ok(None)
        }
    }

    #[instrument]
    /// Batch poll will return all of the currently accumulated events in a
    /// single call
    async fn batch_poll(&mut self, id: SubId) -> Result<Vec<T>> {
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

/// Provides the handle to the [`EventBus`] actor
#[derive(Clone)]
pub struct EventHandle<T: Send + Clone> {
    tx: flume::Sender<EventBusMessage<T>>,
    id: SubId,
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
    pub async fn unsubscribe(&self, id: SubId) -> Result<()> {
        self.tx
            .send(EventBusMessage::Unsubscribe { id })
            .map_err(|e| {
                tracing::error!("Send error: {e:}");
                EventBusError::ChannelSendError
            })?;
        Ok(())
    }

    #[instrument]
    pub async fn batch_poll(&self) -> Result<Vec<T>> {
        let (tx, rx) = flume::bounded(1);

        self.tx
            .send(EventBusMessage::BatchPoll {
                id: self.id,
                reply: tx,
            })
            .map_err(|e| {
                tracing::error!("Send error: {e:}");
                EventBusError::ChannelSendError
            })?;

        rx.recv_async().await?
    }

    pub fn id(&self) -> SubId {
        self.id
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
}

impl<T: Send + Clone> Drop for EventHandle<T> {
    #[instrument]
    fn drop(&mut self) {
        // handle with id 0 does not need to be unsubbed when dropped
        if self.id != NULL_SUBID {
            tracing::debug!("Unsubbing for handle id {:?}", self.id);
            self.tx
                .send(EventBusMessage::Unsubscribe { id: self.id })
                .map_err(|e| {
                    tracing::error!("Send error: {e:}");
                })
                .ok();
        }
    }
}

/// when the delay-delete feature flag is enabled during compilation,
/// this background task must be executed and run in a separate thread
/// so was to not block the main runtime thread
#[cfg(feature = "delay-delete")]
#[instrument]
pub async fn cleanup_task<T: Send + Clone>(handle: EventHandle<T>) {
    use std::{thread, time::Duration};
    loop {
        tracing::trace!("Executing clean up logic on event bus");
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
