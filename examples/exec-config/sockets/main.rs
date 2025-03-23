//! Example of a bunch of sockets spawned with
//! an executor/io-poller per core runtime configuration

use std::io::{Read, Write};
use std::net::TcpListener;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use imputio::{
    Event, ExecConfig, ExecThreadConfig, ImputioRuntime, PollThreadConfig, Priority, spawn,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let available_cores = core_affinity::get_core_ids().unwrap_or_default();
    let len = available_cores.len();

    let execs = available_cores
        .iter()
        .map(|core| {
            let poller_cfg = PollThreadConfig::default()
                .with_core_affinity(core.id)
                .with_name(format!("imputio-io-{:?}", core.id));
            ExecThreadConfig::default()
                .with_core_affinity(core.id)
                .with_poll_thread_cfg(poller_cfg)
                .with_name(format!("imputio-exec-{:?}", core.id))
        })
        .collect::<Vec<_>>();

    let (shutdown_tx, shutdown_rx) = flume::unbounded();
    let rx = shutdown_rx.clone();
    let tx = shutdown_tx.clone();

    std::thread::spawn(move || {
        tracing::info!("Press ENTER to exit tcp server loop");
        let _ = std::io::stdin().read(&mut [0]).unwrap();
        tx.send(())
    });

    ImputioRuntime::builder()
        .with_exec_config(ExecConfig::default().with_cfg(execs))
        .shutdown((shutdown_tx, rx))
        .build()?
        .block_on(async move {
            spawn_tcp_socks(shutdown_rx, len).await;
        });
    Ok(())
}

async fn spawn_tcp_socks(shutdown_rx: flume::Receiver<()>, num_socks: usize) {
    let listeners = (0..num_socks)
        .map(|idx| {
            let port = 3456 + idx as u16;
            let listener =
                std::net::TcpListener::bind(("127.0.0.1", port)).expect("Failed to bind");
            listener
                .set_nonblocking(true)
                .expect("Failed to set non blocking");

            let listen_clone = listener.try_clone().expect("Failed to clone");

            let (notify_tx, notify_rx) = flume::unbounded();

            imputio::register_tcp_socket(listener, None, Some(notify_tx))
                .expect("Failed to register");
            (listen_clone, notify_rx)
        })
        .collect::<Vec<(TcpListener, flume::Receiver<Event>)>>();

    loop {
        listeners.iter().for_each(|(listener, notify_rx)| {
            if let Ok(event) = notify_rx.try_recv() {
                if event.is_write_closed() || event.is_read_closed() {
                    tracing::info!("WriteClosed or ReadClosed");
                } else {
                    let (stream, sock_addr) = listener.accept().expect("Failed to accept");
                    tracing::info!("SocketAddr: {:?}", sock_addr);
                    let _t = spawn!(read_and_or_write_socket(event, stream), Priority::High);
                }
            }
        });

        if let Ok(()) = shutdown_rx.try_recv() {
            break;
        }
    }
}

async fn read_and_or_write_socket(event: Event, mut stream: std::net::TcpStream) {
    tracing::info!("reading and/or writing to socket {:?}", stream);
    stream.set_nodelay(true).expect("Failed to set nodelay");

    let mut buffer = [0u8; 1024];
    let mut data: Vec<u8> = vec![];
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
        tracing::info!("Read string: {:?}", String::from_utf8_lossy(&data));
    }

    if event.is_writable() {
        tracing::info!("writing: {:?}", data);
        stream.write_all(&data).expect("Failed to write");
    }
}
