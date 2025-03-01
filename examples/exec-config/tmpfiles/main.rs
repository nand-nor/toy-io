//! Example of a bunch of tempfile fds spawned with
//! an executor/io-poller per core runtime configuration

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    os::fd::AsRawFd,
};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use imputio::{
    spawn, Event, ExecConfig, ExecThreadConfig, ImputioRuntime, PollThreadConfig, Priority,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let available_cores = core_affinity::get_core_ids().unwrap_or_default();
    let len = 5; //available_cores.len();

    let execs = available_cores[1..=6]
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
    let rt = ImputioRuntime::builder()
        .with_exec_config(ExecConfig::default().with_cfg(execs))
        .shutdown((shutdown_tx, rx))
        .build()?;

    let temp_dir = tempfile::tempdir_in(".").unwrap();

    std::thread::spawn(move || {
        tracing::info!("Press ENTER to delete all tmp files and exit");
        let _ = std::io::stdin().read(&mut [0]).unwrap();
        tx.send(())
    });

    let _ = rt.block_on(async move {
        spawn_fds(shutdown_rx, len, temp_dir).await;
    });

    tracing::info!("Shutting down");
    Ok(())
}

async fn spawn_fds(rx: flume::Receiver<()>, num_fds: usize, dir: tempfile::TempDir) {
    let mut fds = (0..num_fds)
        .map(|idx| {
            let path = dir.path().join(format!("spam-fds-{}.txt", idx));

            let mut tmpfile = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                // silence clippy (should never have to truncate a tmp file)
                .truncate(false)
                .open(path)
                .unwrap();

            writeln!(tmpfile, "persist the file").ok();
            let (notify_tx, notify_rx) = flume::unbounded();

            imputio::register_fd(tmpfile.as_raw_fd(), None, Some(notify_tx))
                .expect("Failed to register");
            (tmpfile, notify_rx)
        })
        .collect::<Vec<(File, flume::Receiver<Event>)>>();

    loop {
        fds.iter_mut().for_each(|(tmpfile, notify_rx)| {
            if let Ok(event) = notify_rx.try_recv() {
                if event.is_write_closed() || event.is_read_closed() {
                    tracing::info!("WriteClosed or ReadClosed");
                } else {
                    tracing::info!("Attempting top read and/or write to fd");
                    let _t = spawn!(
                        read_and_or_write_file(event, tmpfile.try_clone().unwrap()),
                        Priority::High
                    );
                }
            }
        });

        if let Ok(()) = rx.try_recv() {
            for (tmpfile, _notify_rx) in fds {
                drop(tmpfile);
            }
            break;
        }
    }
    dir.close().expect("Unable to close temp dir");
}

async fn read_and_or_write_file(event: Event, mut tmpfile: File) {
    let mut buf = String::new();
    if event.is_readable() {
        loop {
            match tmpfile.read_to_string(&mut buf) {
                Ok(0) => {
                    break;
                }
                Err(ref e) => {
                    if e.kind() != std::io::ErrorKind::WouldBlock {
                        tracing::error!("Error {e:}");
                    }
                    break;
                }
                _ => {}
            }
        }
        tracing::info!("Read string: {:?}", buf);
    }

    if event.is_writable() {
        tracing::info!("writing to file");
        tmpfile.seek(std::io::SeekFrom::End(0)).ok();

        write!(tmpfile, "Hello World!").unwrap();
    }

    tracing::info!("Event: {:?}", event);
}
