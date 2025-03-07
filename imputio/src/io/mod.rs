//! actor / handle impl backed by
//! the epoll kernel datastructure
//! as provided by mio dependency

mod epoll;
pub use epoll::{Operation, Poller, PollerCfg};

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(thiserror::Error, Debug)]
pub enum PollError {
    #[error("Resource exhaustion")]
    ResourceExhaustion,
    #[error("Actor error {0}")]
    ActorError(String),
    #[error("Actor receive error {0}")]
    ActorRcvError(#[from] flume::RecvError),
    #[error("Actor send error {0}")]
    ActorSendError(#[from] flume::SendError<IoOp>),
    #[error("Io error")]
    IoError(#[from] std::io::Error),
}

type Result<T> = std::result::Result<T, PollError>;

#[derive(Debug)]
pub enum IoOp {
    Submit { op: Operation },
    Poll,
    Process,
    PollAndProcess,
}

unsafe impl Sync for IoOp {}
unsafe impl Send for IoOp {}

#[derive(Clone)]
pub struct PollHandle {
    tx: flume::Sender<IoOp>,
}

impl std::fmt::Debug for PollHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PollHandle").finish()
    }
}

impl PollHandle {
    pub fn new(tx: flume::Sender<IoOp>) -> Self {
        Self { tx }
    }

    pub fn initialize(
        poll_cfg: PollerCfg,
        shutdown: Arc<AtomicBool>,
    ) -> Result<(PollerActor, PollHandle)> {
        let (tx, rx) = flume::unbounded();
        let poll_ring = PollerActor::new(rx, shutdown, poll_cfg)?;
        let handle = Self::new(tx);
        Ok((poll_ring, handle))
    }

    pub fn submit_op(&self, op: Operation) -> Result<()> {
        let op = IoOp::Submit { op };
        self.tx
            .send(op)
            .inspect_err(|e| tracing::error!("submit op failure {e:}"))?;

        Ok(())
    }

    pub fn poll(&self) -> Result<()> {
        self.tx
            .send(IoOp::Poll)
            .inspect_err(|e| tracing::error!("Poll op failure {e:}"))?;

        Ok(())
    }

    // FIXME: process is only needed for io_uring
    #[allow(unused)]
    pub fn poll_and_process(&self) -> Result<()> {
        self.tx
            .send(IoOp::PollAndProcess)
            .inspect_err(|e| tracing::error!("PollAndProcess op failure {e:}"))?;

        Ok(())
    }

    // FIXME: process is only needed for io_uring
    #[allow(unused)]
    pub fn process(&self) -> Result<()> {
        self.tx
            .send(IoOp::Process)
            .inspect_err(|e| tracing::error!("Process op failure {e:}"))?;

        Ok(())
    }
}

pub struct PollerActor {
    receiver: flume::Receiver<IoOp>,
    poller: Poller,
    shutdown: Arc<AtomicBool>,
}

unsafe impl Send for PollerActor {}
unsafe impl Sync for PollerActor {}

impl std::fmt::Debug for PollerActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PollerActor").finish()
    }
}

impl PollerActor {
    pub fn new(
        receiver: flume::Receiver<IoOp>,
        shutdown: Arc<AtomicBool>,
        poll_cfg: PollerCfg,
    ) -> Result<Self> {
        Ok(Self {
            poller: Poller::new(poll_cfg)?,
            receiver,
            shutdown,
        })
    }

    #[inline]
    pub fn run(mut self) {
        tracing::trace!(
            "Run io poller thread id: {:?}, name: {:?}",
            std::thread::current().id(),
            std::thread::current().name()
        );

        self.poller.set_id(std::thread::current().id());

        loop {
            if let Ok(event) = self.receiver.try_recv() {
                match event {
                    IoOp::Submit { op } => self.submit(op).ok(),
                    IoOp::Poll => self.poll().ok(),
                    IoOp::Process => self.process().ok(),
                    IoOp::PollAndProcess => self.poll_and_process().ok(),
                };
            }

            if self.shutdown.load(Ordering::SeqCst) {
                break;
            }
        }

        self.shutdown();
    }

    fn shutdown(self) {
        tracing::trace!(
            "IO poller actor id {:?} shutting down",
            std::thread::current().id()
        );
        self.poller.shutdown();
    }

    #[inline]
    fn submit(&mut self, op: Operation) -> Result<()> {
        self.poller.push_token_entry(op)?;
        Ok(())
    }

    #[inline]
    fn poll_and_process(&mut self) -> Result<()> {
        self.poll()?;
        self.process()?;
        Ok(())
    }

    #[inline]
    fn poll(&mut self) -> Result<()> {
        self.poller.poll()?;
        Ok(())
    }

    #[inline]
    fn process(&mut self) -> Result<()> {
        // FIXME: process step only needed for io_uring
        Ok(())
    }
}
