//! Example of a simple TCP server

use std::io::{Read, Write};

use flume::TryRecvError;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use imputio::ImputioRuntime;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let listener = std::net::TcpListener::bind(("127.0.0.1", 3456)).unwrap();

    let (shutdown_tx, shutdown_rx) = flume::unbounded();
    let tx = shutdown_tx.clone();

    std::thread::spawn(move || {
        tracing::info!("Press ENTER to exit tcp server loop");
        let _ = std::io::stdin().read(&mut [0]).unwrap();
        tx.send(())
    });

    let (notify_tx, notify_rx) = flume::unbounded();
    let listen_clone = listener.try_clone()?;

    let rx = shutdown_rx.clone();
    let mut rt = ImputioRuntime::builder()
        .shutdown((shutdown_tx, rx))
        .build()?;
    rt.run();

    imputio::register_tcp_socket(listener, None, Some(notify_tx))?;

    loop {
        if let Ok(event) = notify_rx.try_recv() {
            if event.is_write_closed() || event.is_read_closed() {
                tracing::info!("WriteClosed or ReadClosed");
                break;
            } else {
                let mut buffer = [0u8; 1024];
                let mut data: Vec<u8> = vec![];

                let (mut stream, sock_addr) = listen_clone.accept()?;
                tracing::info!("SocketAddr: {:?}", sock_addr);

                if event.is_readable() {
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
                    let str = String::from_utf8_lossy(&data);
                    tracing::info!("Read string: {str:}");
                }

                if event.is_writable() {
                    tracing::info!("writing: {:?}", data);
                    stream.write_all(&data)?;
                }
            }
        } else if notify_rx.is_disconnected() {
            break;
        }

        // check for shutdown notice
        match shutdown_rx.try_recv() {
            Err(TryRecvError::Disconnected) => {
                // this should only happen if the other end of the
                // channel is closed
                tracing::error!("Error receiving shutdown notice, disconnected");
                break;
            }
            Err(TryRecvError::Empty) => {}
            Ok(()) => {
                tracing::info!("Shutdown notice received!");
                break;
            }
        }
    }

    tracing::info!("Exiting");
    Ok(())
}
