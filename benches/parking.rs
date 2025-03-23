//! Benchmarks comparing different parking configs

use std::{
    io::{Read, Write},
    net::TcpStream,
};

use core_affinity::CoreId;
use criterion::{Criterion, criterion_group, criterion_main};
use imputio::{ExecConfig, ExecThreadConfig, ImputioRuntime, PollThreadConfig, spawn};

async fn do_some_tcp_io() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let (notify_tx, notify_rx) = flume::unbounded();
    let listen_clone = listener.try_clone().expect("failed to clone tcp sock");
    let addr = listen_clone.local_addr().unwrap();
    let mut rt = ImputioRuntime::builder().build().unwrap();
    rt.run();

    imputio::register_tcp_socket(listener, None, Some(notify_tx)).unwrap();

    // spawn a thread to act as client side of the TCP conn
    std::thread::spawn(move || {
        let mut stream = TcpStream::connect(addr).unwrap();
        let message = "Future benchmarks will vary the size written :)";
        stream.write_all(message.as_bytes()).ok();
    });

    loop {
        if let Ok(event) = notify_rx.try_recv() {
            if event.is_write_closed() || event.is_read_closed() {
                break;
            } else {
                let mut buffer = [0u8; 1024];
                let mut data: Vec<u8> = vec![];

                let (mut stream, _sock_addr) = listen_clone.accept().unwrap();
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
                    let _str = String::from_utf8_lossy(&data);
                    break;
                }
            }
        }
    }
}

struct BenchSetup {
    start_core: Option<usize>,
    bench_core: Option<usize>,
    /// if none, no io parking
    with_io_parking: Option<u64>,
    /// if none, no exec parking
    with_exec_parking: Option<u64>,
    with_core_affin: bool,
}

fn thread_setup(setup: BenchSetup) -> Vec<ExecThreadConfig> {
    let mut start = if setup.start_core.is_none() {
        0
    } else {
        setup.start_core.unwrap()
    };

    let num_cores = num_cpus::get();

    if setup.with_core_affin && setup.bench_core.is_some() {
        core_affinity::set_for_current(CoreId {
            id: setup.bench_core.unwrap(),
        });
        if start == setup.bench_core.unwrap() {
            start += 1;
        }
    }

    if start >= num_cores {
        start = 0;
    }

    // TODO: better design
    (start..num_cores)
        .into_iter()
        .map(|id| {
            match (
                setup.with_core_affin,
                setup.with_exec_parking,
                setup.with_io_parking,
            ) {
                (true, Some(exec_timeout), Some(io_timeout)) => {
                    let poller_cfg = PollThreadConfig::default()
                        .with_parking(io_timeout)
                        .with_core_affinity(id)
                        .with_name(format!("imputio-io-{:?}", id));
                    ExecThreadConfig::default()
                        .with_core_affinity(id)
                        .with_parking(exec_timeout)
                        .with_poll_thread_cfg(poller_cfg)
                        .with_name(format!("imputio-exec-{:?}", id))
                }
                (false, Some(exec_timeout), Some(io_timeout)) => {
                    let poller_cfg = PollThreadConfig::default()
                        .with_parking(io_timeout)
                        .with_name(format!("imputio-io-{:?}", id));
                    ExecThreadConfig::default()
                        .with_parking(exec_timeout)
                        .with_poll_thread_cfg(poller_cfg)
                        .with_name(format!("imputio-exec-{:?}", id))
                }
                (true, None, Some(io_timeout)) => {
                    let poller_cfg = PollThreadConfig::default()
                        .with_core_affinity(id)
                        .with_parking(io_timeout)
                        .with_name(format!("imputio-io-{:?}", id));
                    ExecThreadConfig::default()
                        .with_core_affinity(id)
                        .with_poll_thread_cfg(poller_cfg)
                        .with_name(format!("imputio-exec-{:?}", id))
                }
                (true, Some(exec_timeout), None) => {
                    let poller_cfg = PollThreadConfig::default()
                        .with_core_affinity(id)
                        .with_name(format!("imputio-io-{:?}", id));
                    ExecThreadConfig::default()
                        .with_core_affinity(id)
                        .with_parking(exec_timeout)
                        .with_poll_thread_cfg(poller_cfg)
                        .with_name(format!("imputio-exec-{:?}", id))
                }
                (true, None, None) => {
                    let poller_cfg = PollThreadConfig::default()
                        .with_core_affinity(id)
                        .with_name(format!("imputio-io-{:?}", id));
                    ExecThreadConfig::default()
                        .with_core_affinity(id)
                        .with_poll_thread_cfg(poller_cfg)
                        .with_name(format!("imputio-exec-{:?}", id))
                }
                (false, None, None) => {
                    let poller_cfg =
                        PollThreadConfig::default().with_name(format!("imputio-io-{:?}", id));
                    ExecThreadConfig::default()
                        .with_poll_thread_cfg(poller_cfg)
                        .with_name(format!("imputio-exec-{:?}", id))
                }
                (false, Some(exec_timeout), None) => {
                    let poller_cfg =
                        PollThreadConfig::default().with_name(format!("imputio-io-{:?}", id));
                    ExecThreadConfig::default()
                        .with_parking(exec_timeout)
                        .with_poll_thread_cfg(poller_cfg)
                        .with_name(format!("imputio-exec-{:?}", id))
                }
                (false, None, Some(io_timeout)) => {
                    let poller_cfg = PollThreadConfig::default()
                        .with_parking(io_timeout)
                        .with_name(format!("imputio-io-{:?}", id));
                    ExecThreadConfig::default()
                        .with_poll_thread_cfg(poller_cfg)
                        .with_name(format!("imputio-exec-{:?}", id))
                }
            }
        })
        .collect::<Vec<_>>()
}

fn rt_setup(execs: Vec<ExecThreadConfig>) -> ImputioRuntime {
    let exec_cfg = if execs.is_empty() {
        ExecConfig::default()
    } else {
        ExecConfig::default().with_cfg(execs)
    };

    let (shutdown_tx, shutdown_rx) = flume::unbounded();
    let rx = shutdown_rx.clone();

    let mut rt = ImputioRuntime::builder()
        .with_exec_config(exec_cfg)
        .shutdown((shutdown_tx, rx))
        .build()
        .expect("Failed to create runtime");

    rt.run();
    rt
}

fn setup_default(
    with_io_parking: Option<u64>,
    with_exec_parking: Option<u64>,
    start_core: Option<usize>,
    bench_core: Option<usize>,
) -> ImputioRuntime {
    let setup = BenchSetup {
        start_core,
        bench_core,
        with_core_affin: true,
        with_io_parking,
        with_exec_parking,
    };
    let execs = thread_setup(setup);

    rt_setup(execs)
}

fn test_default_parking_10millis(criterion: &mut Criterion) {
    let mut rt = setup_default(Some(10), Some(10), Some(1), Some(0));
    let mut group = criterion.benchmark_group("default-parking");
    group.bench_function("default_parking", |b| {
        b.iter(|| rt.block_on(async move { spawn!(do_some_tcp_io()) }))
    });
    group.finish();
}

fn test_parking_50millis(criterion: &mut Criterion) {
    let mut rt = setup_default(Some(50), Some(50), Some(1), Some(0));
    let mut group = criterion.benchmark_group("50mills-parking");
    group.bench_function("50millis_parking", |b| {
        b.iter(|| rt.block_on(async move { spawn!(do_some_tcp_io()) }))
    });
    group.finish();
}

fn test_no_io_parking(criterion: &mut Criterion) {
    let mut rt = setup_default(None, Some(10), Some(5), Some(0));
    let mut group = criterion.benchmark_group("no-io-parking");

    group.bench_function("no_io_parking", |b| {
        b.iter(|| rt.block_on(async move { spawn!(do_some_tcp_io()) }))
    });
    group.finish();
}

fn test_no_exec_parking(criterion: &mut Criterion) {
    let mut rt = setup_default(Some(10), None, Some(5), Some(0));
    let mut group = criterion.benchmark_group("no-exec-parking");

    group.bench_function("no_exec_parking", |b| {
        b.iter(|| rt.block_on(async move { spawn!(do_some_tcp_io()) }))
    });
    group.finish();
    drop(rt)
}

criterion_group! {
    name = no_io_parking;
    config = Criterion::default().significance_level(0.1).sample_size(50);
    targets = test_default_parking_10millis, test_no_io_parking
}

criterion_group! {
    name = no_exec_parking;
    config = Criterion::default().significance_level(0.1).sample_size(50);
    targets = test_default_parking_10millis, test_no_exec_parking
}

criterion_group! {
    name = longer_parking;
    config = Criterion::default().significance_level(0.1).sample_size(50);
    targets = test_default_parking_10millis, test_parking_50millis
}

criterion_group! {
    name = parking_config;
    config = Criterion::default().significance_level(0.1).sample_size(50);
    targets = test_default_parking_10millis
}

criterion_main!(
    parking_config,
    no_exec_parking,
    no_io_parking,
    longer_parking,
);
