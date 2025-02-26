//! actor / handle impl backed by
//! the epoll kernel datastructure
//! as provided by mio dependency

mod epoll;

pub use epoll::{Operation, PollCfg, Poller};

use tracing::instrument;

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

    pub fn initialize(poll_cfg: Option<PollCfg>) -> (PollerActor, PollHandle) {
        let (tx, rx) = flume::unbounded();
        let poll_ring = PollerActor::new(rx, poll_cfg);
        let handle = Self::new(tx);
        (poll_ring, handle)
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
}

unsafe impl Send for PollerActor {}
unsafe impl Sync for PollerActor {}

impl std::fmt::Debug for PollerActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PollerActor").finish()
    }
}

impl PollerActor {
    #[instrument]
    pub fn new(receiver: flume::Receiver<IoOp>, poll_cfg: Option<PollCfg>) -> Self {
        tracing::debug!("New io actor created");
        Self {
            poller: Poller::new(poll_cfg.unwrap_or_default()).expect("Unable to create new poller"),
            receiver,
        }
    }

    #[instrument]
    pub fn run(mut self) {
        while let Ok(event) = self.receiver.recv() {
            match event {
                IoOp::Submit { op } => self.submit(op).ok(),
                IoOp::Poll => self.poll().ok(),
                IoOp::Process => self.process().ok(),
                IoOp::PollAndProcess => self.poll_and_process().ok(),
            };
        }
    }

    #[instrument]
    fn submit(&mut self, op: Operation) -> Result<()> {
        tracing::debug!("Received event");
        self.poller.push_token_entry(op)?;
        Ok(())
    }

    #[instrument]
    fn poll_and_process(&mut self) -> Result<()> {
        self.poll()?;
        self.process()?;
        Ok(())
    }

    #[instrument]
    fn poll(&mut self) -> Result<()> {
        self.poller.poll()?;
        Ok(())
    }

    #[instrument]
    fn process(&mut self) -> Result<()> {
        // FIXME: process step only needed for io_uring
        Ok(())
    }
}
