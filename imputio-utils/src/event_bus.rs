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

/// The [`EventBus`] object is an actor that allows
/// publishing and subscribing to events
struct EventBus<T: Send + Clone> {
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
        tracing::trace!("New event bus created");
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

    pub async fn run(mut self) {
        while let Ok(event) = self.receiver.recv_async().await {
            match event {
                EventBusMessage::Subscribe { reply } => reply.send(self.subscribe().await).ok(),
                EventBusMessage::Unsubscribe { id } => {
                    self.unsubscribe(id).await;
                    None
                }
                EventBusMessage::PushEvent { event, reply } => {
                    reply.send(self.push_to_event_queues(event).await).ok()
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
        tracing::trace!("Pushing ID {id:} to unsubscribe list");
        self.recycle.push(id);
    }

    #[cfg(feature = "delay-delete")]
    async fn cleanup(&mut self) {
        tracing::trace!("Calling cleanup for all unsubbed handles");
        let cleanup = self.recycle.clone();
        self.recycle.clear();
        for id in cleanup.iter() {
            tracing::trace!("Cleaning up events from id {id:}");
            self.event_queues.remove(id);
        }
    }

    async fn push_to_event_queues(&mut self, event: T) -> Result<()> {
        for (_, v) in self.event_queues.iter_mut() {
            v.push_back(event.clone());
        }
        Ok(())
    }

    /// poll to return the top of the event queue for the subscriber, if
    /// there is an event
    #[instrument]
    async fn poll(&mut self, id: SubId) -> Result<Option<T>> {
        let item = self.event_queues.get_mut(&id).or(None);
        if let Some(item) = item {
            Ok(item.pop_front())
        } else {
            Ok(None)
        }
    }

    /// Batch poll will return all of the currently accumulated events in a
    /// single call
    #[instrument]
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

/// Provides a subscribe handle to the [`EventBus`] actor
#[derive(Clone)]
pub struct SubHandle<T: Send + Clone> {
    tx: flume::Sender<EventBusMessage<T>>,
    id: SubId,
}

impl<T: Send + Clone> std::fmt::Debug for SubHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubHandle")
            .field("tx", &self.tx)
            .field("id", &self.id)
            .finish()
    }
}

impl<T: Send + Clone> SubHandle<T> {
    #[instrument]
    pub async fn subscribe(&self) -> Result<SubHandle<T>> {
        let (tx, rx) = flume::bounded(1);
        self.tx
            .send(EventBusMessage::Subscribe { reply: tx })
            .map_err(|e| {
                tracing::error!("Send error: {e:}");
                EventBusError::ChannelSendError
            })?;
        let new_id = rx.recv_async().await??;

        Ok(SubHandle {
            id: new_id,
            tx: self.tx.clone(),
        })
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
}

/// Provides a publisher handle to the [`EventBus`] actor
#[derive(Clone)]
pub struct PubHandle<T: Send + Clone> {
    tx: flume::Sender<EventBusMessage<T>>,
}

impl<T: Send + Clone> PubHandle<T> {
    pub async fn publish_event(&self, event: T) -> Result<()> {
        let (tx, rx) = flume::bounded(1);
        self.tx
            .send(EventBusMessage::PushEvent { event, reply: tx })
            .map_err(|e| {
                tracing::error!("Send error: {e:}");
                EventBusError::ChannelSendError
            })?;

        rx.recv_async().await?
    }
}

impl<T: Send + Clone> std::fmt::Debug for PubHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PubHandle").field("tx", &self.tx).finish()
    }
}

/// Provides the handle to the [`EventBus`] actor
#[derive(Clone)]
pub struct EventBusHandle<T: Send + Clone> {
    tx: flume::Sender<EventBusMessage<T>>,
    id: SubId,
}

impl<T: Send + Clone> std::fmt::Debug for EventBusHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBusHandle")
            .field("id", &self.id)
            .finish()
    }
}

impl<T: Send + Clone> EventBusHandle<T> {
    pub async fn new_with_handle() -> Result<EventBusHandle<T>>
    where
        T: 'static,
    {
        let (tx, rx) = flume::unbounded();
        let bus = EventBus::new(rx);

        std::thread::spawn(move || {
            let task = spawn!(bus.run(), Priority::High);
            task.receiver().recv().ok();
        });

        let handle = EventBusHandle { tx, id: NULL_SUBID };
        Ok(handle)
    }

    pub async fn get_publisher(&self) -> PubHandle<T> {
        let tx = self.tx.clone();
        PubHandle { tx }
    }

    /// This method creates a SubHandle that is subscribed to events
    /// when created
    #[instrument]
    pub async fn get_subscriber(&self) -> Result<SubHandle<T>> {
        let (tx, rx) = flume::bounded(1);
        self.tx
            .send(EventBusMessage::Subscribe { reply: tx })
            .map_err(|e| {
                tracing::error!("Send error: {e:}");
                EventBusError::ChannelSendError
            })?;
        let new_id = rx.recv_async().await??;

        Ok(SubHandle {
            id: new_id,
            tx: self.tx.clone(),
        })
    }

    #[cfg(feature = "delay-delete")]
    pub fn cleanup(&self) {
        self.tx
            .send(EventBusMessage::CleanUpBgTask)
            .map_err(|e| {
                tracing::error!("Send error: {e:}");
            })
            .ok();
    }
}

impl<T: Send + Clone> Drop for SubHandle<T> {
    #[instrument]
    fn drop(&mut self) {
        // handle with id 0 does not need to be unsubbed when dropped
        if self.id != NULL_SUBID {
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
pub fn cleanup_task<T: Send + Clone + 'static>(handle: EventBusHandle<T>) {
    use std::{thread, time::Duration};
    let handle: &'static EventBusHandle<T> = Box::leak(Box::new(handle));
    std::thread::spawn(move || loop {
        tracing::trace!("Executing clean up logic on event bus");
        let handle_c = handle.clone();
        let handle = std::thread::spawn(move || {
            handle_c.cleanup();
            thread::yield_now();
            thread::park_timeout(Duration::from_secs(5));
        });
        handle.join().ok();
    });
}

/// Provides wrapper around event poll loop, checking for an events
/// that a handle is subscribed to, and breaking the loop when the
/// event match condition is met. Optionally will send a notice when
/// the loop is broken
pub async fn event_poll_matcher<T: Send + Clone, S: Send>(
    handle: &SubHandle<T>,
    matcher: impl Fn(&T) -> bool,
    notify: Option<(flume::Sender<S>, S)>,
) -> Result<()> {
    let handler = |event: &Option<T>| {
        if let Some(ev) = event {
            if matcher(ev) {
                return true;
            }
        }
        false
    };
    // generic over the polling function used
    poll_matcher(handle, SubHandle::poll, handler, notify).await
}

/// Provides the same wrapper as [`event_poll_matcher`] but
/// using the handle API to call [`SubHandle::batch_poll`]
/// instead of [`SubHandle::poll`]
pub async fn event_batch_poll_matcher<T: Send + Clone, S: Send>(
    handle: &SubHandle<T>,
    matcher: impl Fn(&T) -> bool,
    notify: Option<(flume::Sender<S>, S)>,
) -> Result<()> {
    let handler = |events: &Vec<T>| {
        for ev in events {
            if matcher(ev) {
                return true;
            }
        }
        false
    };

    poll_matcher(handle, SubHandle::batch_poll, handler, notify).await
}

/// Generic method for calling either the [`SubHandle::poll`] or
/// [`SubHandle::batch_poll`] method
pub async fn poll_matcher<'a, T: Send + Clone, S: Send, F, O>(
    handle: &'a SubHandle<T>,
    poll_fn: fn(&'a SubHandle<T>) -> F,
    handler: impl Fn(&O) -> bool,
    notify: Option<(flume::Sender<S>, S)>,
) -> Result<()>
where
    F: futures_lite::Future<Output = Result<O>> + 'a,
{
    let id = handle.id();
    tracing::trace!("event-consumer-{id:}: dropping into poll loop");
    loop {
        let event = poll_fn(handle).await;
        if let Ok(event) = event {
            if handler(&event) {
                break;
            }
        } else {
            tracing::error!("Error processing event bus poll, exiting");
            break;
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
