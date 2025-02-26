use mio::{event::Event, net::TcpListener, Events, Interest, Poll as EPoller, Token};
use slab::Slab;
use std::{
    collections::HashMap,
    net::TcpListener as StdTcpListener,
    os::fd::{AsRawFd, RawFd},
};

use super::PollError;

#[derive(Debug, Clone)]
pub struct PollCfg {
    event_size: usize,
}

impl Default for PollCfg {
    fn default() -> Self {
        Self { event_size: 256 }
    }
}

type Result<T> = std::result::Result<T, PollError>;

#[derive(Debug)]
pub enum Operation {
    /// Register a file descriptor as a
    /// [`mio::unix::SourceFd`]
    RegistrationFdAdd {
        fd: RawFd,
        interest: Interest,
        notify: Option<flume::Sender<Event>>,
    },
    /// Register a std lib [`std::net::TcpListener`]
    /// with the epoll actor
    RegistrationTcpAdd {
        fd: StdTcpListener,
        interest: Interest,
        notify: Option<flume::Sender<Event>>,
    },
    /// Use the raw file descriptor to modify
    /// either a registered TCP socket or a
    /// unix FD
    ModifyRegistration {
        fd: RawFd,
        interest: Interest,
        notify: Option<flume::Sender<Event>>,
    },
    DeleteRegistration {
        fd: RawFd,
    },
}

unsafe impl Sync for Operation {}
unsafe impl Send for Operation {}

const RW_INTERESTS: Interest = Interest::READABLE
    .add(Interest::WRITABLE)
    .add(Interest::PRIORITY);

pub struct Poller {
    poller: EPoller,
    events: Events,
    ids: HashMap<RawFd, (usize, Option<flume::Sender<Event>>)>,
    tokens: Slab<Operation>,
}

impl Poller {
    pub fn new(cfg: PollCfg) -> Result<Self> {
        let poller: mio::Poll = mio::Poll::new()?;
        let events = Events::with_capacity(cfg.event_size);

        Ok(Self {
            poller,
            events,
            tokens: Slab::new(),
            ids: HashMap::new(),
        })
    }

    pub fn poll(&mut self) -> Result<()> {
        self.poller
            .poll(&mut self.events, Some(std::time::Duration::from_millis(10)))?;

        for event in self.events.iter() {
            if let Some(op) = self.tokens.get(event.token().0) {
                match op {
                    Operation::RegistrationFdAdd { notify, .. } => {
                        tracing::debug!("Adding modified event! {event:?}");

                        if let Some(notify) = notify {
                            if let Err(e) = notify.send(event.clone()) {
                                tracing::error!("Error sending to user-supplied notifier {e:}");
                            }
                        }
                    }
                    Operation::ModifyRegistration { .. } => todo!(),
                    Operation::DeleteRegistration { .. } => todo!(),
                    Operation::RegistrationTcpAdd { notify, .. } => {
                        tracing::debug!("Adding tcp socket! {event:?}");

                        if let Some(notify) = notify {
                            if let Err(e) = notify.send(event.clone()) {
                                tracing::error!("Error sending to user-supplied notifier {e:}");
                            }
                        }
                    }
                };
            }
        }

        Ok(())
    }

    pub fn push_token_entry(&mut self, op: Operation) -> Result<()> {
        match op {
            Operation::RegistrationFdAdd {
                fd,
                interest,
                notify,
            } => {
                let id = if let Some((id, existing_notify)) = self.ids.get(&fd) {
                    // FIXME!! dont use direct index access lest this panic
                    let token = &mut self.tokens[*id];
                    *token = Operation::RegistrationFdAdd {
                        fd,
                        interest,
                        notify: existing_notify.clone(),
                    };
                    *id
                } else {
                    let new_id = self.tokens.vacant_entry().key();
                    self.ids.insert(fd, (new_id, None));
                    self.tokens.insert(Operation::RegistrationFdAdd {
                        fd,
                        interest,
                        notify,
                    });
                    new_id
                };

                self.poller.registry().register(
                    &mut mio::unix::SourceFd(&fd),
                    Token(id),
                    RW_INTERESTS,
                )?;
            }
            Operation::ModifyRegistration {
                fd,
                interest,
                notify,
            } => {
                let id = if let Some((id, existing_notify)) = self.ids.get(&fd) {
                    // FIXME!! dont use direct index access lest this panic
                    let token = &mut self.tokens[*id];
                    *token = Operation::ModifyRegistration {
                        fd,
                        interest,
                        notify: existing_notify.clone(),
                    };
                    *id
                } else {
                    let new_id = self.tokens.vacant_entry().key();
                    self.ids.insert(fd, (new_id, None));
                    self.tokens.insert(Operation::ModifyRegistration {
                        fd,
                        interest,
                        notify: notify.clone(),
                    });
                    new_id
                };

                if let Some(notify) = notify {
                    self.ids.insert(fd, (id, Some(notify)));
                }
            }
            Operation::DeleteRegistration { .. } => todo!(),
            Operation::RegistrationTcpAdd {
                fd,
                interest,
                notify,
            } => {
                let raw_fd = fd.as_raw_fd();
                let new_event_id = if let Some((id, existing_notify)) = self.ids.get(&raw_fd) {
                    // TODO!! dont use direct index access left this panic
                    let token = &mut self.tokens[*id];
                    *token = Operation::RegistrationTcpAdd {
                        fd: fd.try_clone()?,
                        interest,
                        notify: existing_notify.clone(),
                    };
                    *id
                } else {
                    let new_id = self.tokens.vacant_entry().key();
                    self.ids.insert(raw_fd, (new_id, notify.clone()));
                    self.tokens.insert(Operation::RegistrationTcpAdd {
                        fd: fd.try_clone()?,
                        interest,
                        notify,
                    });
                    new_id
                };

                let mut mio_tcp = TcpListener::from_std(fd);

                self.poller
                    .registry()
                    .register(&mut mio_tcp, Token(new_event_id), RW_INTERESTS)?;
            }
        }

        Ok(())
    }
}
