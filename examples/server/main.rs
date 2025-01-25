//! Example of a simple server using mio

use mio::{
    net::{TcpListener, TcpStream},
    Events, Interest, Poll as MioPoll, Token,
};
use std::{
    error::Error,
    future::Future,
    io::{Read, Write},
    pin::Pin,
    task::{Context, Poll},
    thread,
    time::Duration,
};
use tracing_subscriber::FmtSubscriber;

use imputio::{spawn_blocking, Executor, ImputioRuntime, ImputioTask, Priority};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::TRACE)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    ImputioRuntime::<Executor>::new().run();

    let (server_future, mut stream) = ServerFuture::new().unwrap();

    let mut client_poll: mio::Poll = mio::Poll::new().unwrap();
    client_poll
        .registry()
        .register(&mut stream, CLIENT, mio::Interest::WRITABLE)
        .unwrap();

    let mut events = Events::with_capacity(128);
    client_poll.poll(&mut events, None).ok();

    for event in events.iter() {
        if matches!((event.token(), event.is_writable()), (CLIENT, true)) {
            let message = "Client written bytes received";
            stream.write_all(message.as_bytes()).ok();
        }
    }

    std::thread::sleep(std::time::Duration::from_secs(1));

    let server_fut_result = spawn_blocking!(server_future, Priority::High);

    let server_fut_result = String::from_utf8_lossy(&server_fut_result);
    tracing::info!("Server future result returned: {server_fut_result:}");

    Ok(())
}

pub const SERVER: Token = Token(0);
pub const CLIENT: Token = Token(1);

pub struct Server {
    pub stream: TcpStream,
    pub task: ImputioTask<Vec<u8>>,
}

impl ServerFuture {
    pub fn new() -> Result<(Self, TcpStream), Box<dyn Error>> {
        // As simple as possible, just MIO polling a socket...
        let addr = "127.0.0.1:6676".parse()?;
        let mut server = TcpListener::bind(addr)?;
        let stream = TcpStream::connect(server.local_addr()?)?;

        let poll: mio::Poll = mio::Poll::new()?;

        poll.registry()
            .register(&mut server, SERVER, Interest::READABLE)?;

        Ok((ServerFuture::build(server, poll), stream))
    }
}

pub struct ServerFuture {
    server: TcpListener,
    poll: MioPoll,
}

impl ServerFuture {
    pub fn build(server: TcpListener, poll: MioPoll) -> Self {
        Self { server, poll }
    }
}

impl Future for ServerFuture {
    type Output = Vec<u8>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut events = Events::with_capacity(1);

        self.poll
            .poll(&mut events, Some(Duration::from_millis(100)))
            .ok();

        for event in events.iter() {
            if matches!((event.token(), event.is_readable()), (SERVER, true)) {
                let Ok((mut stream, _)) = self.server.accept() else {
                    return Poll::Pending;
                };

                let mut buffer = [0u8; 1024];
                let mut data = vec![];

                loop {
                    match stream.read(&mut buffer) {
                        Ok(n) if n > 0 => {
                            data.extend_from_slice(&buffer[..n]);
                        }
                        Ok(_) => {
                            break;
                        }
                        Err(ref e) => {
                            if e.kind() != std::io::ErrorKind::WouldBlock {
                                tracing::error!("Error {e:}");
                            }
                            break;
                        }
                    }
                }

                if !data.is_empty() {
                    // data is copied so we can continually poll the socket
                    // throughout the lifetime of the program
                    // spawn!(handle_server_data_received(data)).detach();
                    // return Poll::Pending

                    return Poll::Ready(data.clone());
                } else {
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
            }
        }

        thread::yield_now();
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}
