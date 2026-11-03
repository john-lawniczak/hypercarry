//! Loopback-only fault harness compiled exclusively into the test executable.
use super::*;
use hypercarry_execution::RiskSnapshotSource;
use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    process::{Command as ProcessCommand, Stdio},
    sync::{
        Condvar,
        atomic::{AtomicI64, AtomicUsize},
    },
    thread::{self, JoinHandle},
};
use tokio_tungstenite::tungstenite::{self, Message};

#[derive(Default)]
pub(super) struct Publication {
    active: AtomicBool,
    pub arrived: AtomicUsize,
    permits: Mutex<usize>,
    changed: Condvar,
}
impl Publication {
    pub fn checkpoint(&self) {
        if !self.active.load(Ordering::SeqCst) {
            return;
        }
        let mut permits = self.permits.lock().unwrap();
        self.arrived.fetch_add(1, Ordering::SeqCst);
        while *permits == 0 && self.active.load(Ordering::SeqCst) {
            permits = self.changed.wait(permits).unwrap();
        }
        if *permits > 0 {
            *permits -= 1;
        }
    }
    fn release(&self) {
        *self.permits.lock().unwrap() += 1;
        self.changed.notify_all();
    }
    fn disable(&self) {
        let _guard = self.permits.lock().unwrap();
        self.active.store(false, Ordering::SeqCst);
        self.changed.notify_all();
    }
}

#[derive(Clone)]
pub(super) struct Control {
    pub http: String,
    pub ws: String,
    pub clock: Arc<AtomicI64>,
    pub private: stream::Shared,
    pub publication: Arc<Publication>,
}

#[derive(Default)]
struct Venue {
    fault: AtomicUsize, // 1 = builder position, 2 = builder order, 3 = 429.
    suppress_pongs: AtomicBool,
    connections: AtomicUsize,
    messages: Mutex<VecDeque<Message>>,
    stop: AtomicBool,
}

struct Servers {
    venue: Arc<Venue>,
    control: Control,
    threads: Vec<JoinHandle<()>>,
}
impl Servers {
    fn start() -> Self {
        let http = TcpListener::bind("127.0.0.1:0").unwrap();
        let ws = TcpListener::bind("127.0.0.1:0").unwrap();
        let control = Control {
            http: format!("http://{}/info", http.local_addr().unwrap()),
            ws: format!("ws://{}/ws", ws.local_addr().unwrap()),
            clock: Arc::new(AtomicI64::new(now_ms().unwrap())),
            private: Arc::new(Mutex::new(stream::Observation::default())),
            publication: Arc::default(),
        };
        let venue = Arc::new(Venue::default());
        let threads = [(http, false), (ws, true)]
            .into_iter()
            .map(|(listener, websocket)| {
                let venue = venue.clone();
                listener.set_nonblocking(true).unwrap();
                thread::spawn(move || {
                    let mut clients = Vec::new();
                    while !venue.stop.load(Ordering::Relaxed) {
                        if let Ok((socket, _)) = listener.accept() {
                            socket
                                .set_read_timeout(Some(Duration::from_millis(100)))
                                .unwrap();
                            socket
                                .set_write_timeout(Some(Duration::from_secs(1)))
                                .unwrap();
                            let venue = venue.clone();
                            clients.push(thread::spawn(move || {
                                if websocket {
                                    serve_ws(socket, &venue);
                                } else {
                                    let _ = serve_http(socket, &venue);
                                }
                            }));
                        } else {
                            thread::sleep(Duration::from_millis(5));
                        }
                    }
                    for client in clients {
                        client.join().unwrap();
                    }
                })
            })
            .collect();
        Self {
            venue,
            control,
            threads,
        }
    }
}
impl Drop for Servers {
    fn drop(&mut self) {
        self.venue.stop.store(true, Ordering::Relaxed);
        self.control.publication.disable();
        for thread in self.threads.drain(..) {
            thread.join().unwrap();
        }
    }
}

fn serve_http(socket: TcpStream, venue: &Venue) -> Result<()> {
    let mut reader = BufReader::new(socket);
    let mut length = 0;
    loop {
        let mut line = String::new();
        ensure!(reader.read_line(&mut line)? > 0, "fixture request closed");
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse::<usize>()?;
        }
    }
    ensure!(length <= 65536, "fixture request exceeds bound");
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let request: Value = serde_json::from_slice(&body)?;
    let fault = venue.fault.load(Ordering::SeqCst);
    let builder = request["dex"] == "builder";
    let payload = match request["type"].as_str() {
        Some("perpDexs") => json!([null, {"name":"builder"}]),
        Some("clearinghouseState") => json!({"marginSummary":{"accountValue":"1000"},"assetPositions":if builder && fault == 1 {json!([{"position":{"szi":"1"}}])} else {json!([])}}),
        Some("openOrders") if builder && fault == 2 => json!([{"limitPx":"100000","sz":"0.0001"}]),
        Some("openOrders" | "userFills" | "historicalOrders") => json!([]),
        Some("allMids") => json!({"BTC":"100000"}),
        Some("orderStatus") => json!({"status":"unknownOid"}),
        _ => anyhow::bail!("unexpected fixture query"),
    }.to_string();
    let status = if fault == 3 {
        "429 Too Many Requests"
    } else {
        "200 OK"
    };
    write!(
        reader.get_mut(),
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    )?;
    Ok(())
}

fn serve_ws(socket: TcpStream, venue: &Venue) {
    let Ok(mut socket) = tungstenite::accept(socket) else {
        return;
    };
    venue.connections.fetch_add(1, Ordering::SeqCst);
    while !venue.stop.load(Ordering::Relaxed) {
        if let Some(message) = venue.messages.lock().unwrap().pop_front() {
            let close = matches!(message, Message::Close(_));
            if socket.send(message).is_err() || close {
                break;
            }
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let request: Value = serde_json::from_str(&text).unwrap();
                let response = if request["method"] == "subscribe" {
                    Some(json!({"channel":"subscriptionResponse","data":request}))
                } else if !venue.suppress_pongs.load(Ordering::SeqCst) {
                    Some(json!({"channel":"pong"}))
                } else {
                    None
                };
                if let Some(response) = response
                    && socket
                        .send(Message::Text(response.to_string().into()))
                        .is_err()
                {
                    break;
                }
                if request["subscription"]["type"] == "userFills" {
                    let snapshot = json!({"channel":"userFills","data":{"user":request["subscription"]["user"],"isSnapshot":true,"fills":[]}});
                    if socket
                        .send(Message::Text(snapshot.to_string().into()))
                        .is_err()
                    {
                        break;
                    }
                }
            }
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Ok(Message::Ping(data)) => {
                if socket.send(Message::Pong(data)).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "fault harness condition timed out"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn fixture(root: &std::path::Path) -> (RuntimeConfig, config::SupervisorConfig) {
    use sha2::Digest;
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    let mut runtime: RuntimeConfig = serde_json::from_str(include_str!(
        "../../../docs/mainnet-runtime-config.example.json"
    ))
    .unwrap();
    runtime.journal = root.join("journal");
    runtime.health_file = root.join("health");
    runtime.signer_socket = root.join("signer.sock");
    runtime.heartbeat = root.join("heartbeat");
    runtime.kill_switch = root.join("STOP");
    runtime.risk.max_market_data_age_ms = 60_000;
    let executable = root.join("probe");
    // Synthetic infrastructure evidence, restricted to test fixtures.
    let script = r#"#!/bin/sh
printf '{"schema_version":1,"challenge":"%s","integration_config_digest":"%s","alerts_delivered":true,"alert_delivery_id":"TEST-ONLY","rollback_ready":true,"rollback_artifact_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}' "$2" "$4"
"#;
    fs::write(&executable, script).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let supervisor = config::SupervisorConfig {
        schema_version: 1,
        executor_config: root.join("config.json"),
        rolling_pnl_window_ms: 60_000,
        sample_interval_ms: 100,
        operational_probe: Some(config::ProbeConfig {
            executable,
            sha256: config::hex(&sha2::Sha256::digest(script.as_bytes())),
            timeout_ms: 1000,
            config: None,
            config_sha256: None,
        }),
    };
    (runtime, supervisor)
}

struct Daemon {
    stop: Arc<AtomicBool>,
    publication: Arc<Publication>,
    thread: Option<JoinHandle<Result<()>>>,
}
impl Daemon {
    fn start(runtime: RuntimeConfig, config: config::SupervisorConfig, control: Control) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let publication = control.publication.clone();
        let worker_stop = stop.clone();
        let thread = thread::spawn(move || {
            run_observer(
                &config,
                &runtime,
                &worker_stop,
                &ObservationIo {
                    control: Some(control),
                },
            )
        });
        Self {
            stop,
            publication,
            thread: Some(thread),
        }
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.publication.disable();
        self.thread.take().unwrap().join().unwrap().unwrap();
    }
}

#[test]
fn real_loop_reconciles_faults_and_executor_rejects_stale_replication() {
    let servers = Servers::start();
    let temp = tempfile::tempdir().unwrap();
    let (runtime, config) = fixture(temp.path());
    let daemon = Daemon::start(runtime.clone(), config.clone(), servers.control.clone());
    let envelope =
        || hypercarry_mainnet_config::read_json::<HealthEnvelope>(&runtime.health_file).ok();
    let ready = || envelope().is_some_and(|e| authorizes(&e));
    wait_for(ready);
    let consumer = hypercarry_executor::safety::SafetySource(runtime.clone(), 60_000);
    assert!(consumer.snapshot(&runtime.order).is_ok());

    // Deterministically inject an event after REST/probes, before publication.
    let publication = &servers.control.publication;
    publication.active.store(true, Ordering::SeqCst);
    wait_for(|| publication.arrived.load(Ordering::SeqCst) > 0);
    let epoch = servers.control.private.lock().unwrap().epoch;
    servers
        .venue
        .messages
        .lock()
        .unwrap()
        .push_back(Message::Text(
            json!({"channel":"orderUpdates","data":[]})
                .to_string()
                .into(),
        ));
    wait_for(|| servers.control.private.lock().unwrap().epoch != epoch);
    assert!(!runtime.health_file.exists());
    publication.release();
    wait_for(|| envelope().is_some_and(|e| !authorizes(&e)));
    publication.disable();
    wait_for(ready);

    for fault in [1, 2, 3] {
        servers.venue.fault.store(fault, Ordering::SeqCst);
        wait_for(|| match fault {
            1 => envelope().is_some_and(|e| !e.account_flat),
            2 => envelope().is_some_and(|e| e.health.unmanaged_orders == 1),
            _ => !runtime.health_file.exists(),
        });
        assert!(!ready());
        if fault == 1 {
            assert!(consumer.snapshot(&runtime.order).is_err());
        }
        servers.venue.fault.store(0, Ordering::SeqCst);
        wait_for(ready);
    }
    fs::write(&runtime.journal, b"{\"partial\":").unwrap();
    wait_for(|| !runtime.health_file.exists());
    fs::remove_file(&runtime.journal).unwrap();
    wait_for(ready);

    for offset in [-120_000, 120_000] {
        let at = now_ms().unwrap() + offset;
        servers.control.clock.store(at, Ordering::SeqCst);
        wait_for(|| envelope().is_some_and(|e| e.health.observed_at_ms == at));
        // Replacing a file/its mtime cannot refresh the observed payload time.
        assert!(consumer.snapshot(&runtime.order).is_err());
    }
    servers
        .control
        .clock
        .store(now_ms().unwrap(), Ordering::SeqCst);
    wait_for(|| consumer.snapshot(&runtime.order).is_ok());
    let connections = servers.venue.connections.load(Ordering::SeqCst);
    servers
        .venue
        .messages
        .lock()
        .unwrap()
        .push_back(Message::Close(None));
    wait_for(|| !ready());
    wait_for(|| servers.venue.connections.load(Ordering::SeqCst) > connections && ready());
    servers.venue.suppress_pongs.store(true, Ordering::SeqCst);
    wait_for(|| !ready());
    servers.venue.suppress_pongs.store(false, Ordering::SeqCst);
    wait_for(ready);

    drop(daemon);
    assert!(!runtime.health_file.exists());
    // Restart with an incomplete journal must not reuse the prior readiness.
    fs::write(&runtime.journal, b"{\"partial\":").unwrap();
    let restarted = Daemon::start(runtime.clone(), config, servers.control.clone());
    thread::sleep(Duration::from_millis(300));
    assert!(!ready());
    drop(restarted);
    assert!(!runtime.signer_socket.exists());
}

#[test]
fn signal_child() {
    let Ok(spec) = std::env::var("HYPERCARRY_TEST_SIGNAL_SPEC") else {
        return;
    };
    let value: Value = hypercarry_mainnet_config::read_json(std::path::Path::new(&spec)).unwrap();
    let runtime: RuntimeConfig = serde_json::from_value(value["runtime"].clone()).unwrap();
    let config: config::SupervisorConfig =
        serde_json::from_value(value["supervisor"].clone()).unwrap();
    let control = Control {
        http: value["http"].as_str().unwrap().into(),
        ws: value["ws"].as_str().unwrap().into(),
        clock: Arc::new(AtomicI64::new(now_ms().unwrap())),
        private: Arc::default(),
        publication: Arc::default(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone()).unwrap();
    run_observer(
        &config,
        &runtime,
        &stop,
        &ObservationIo {
            control: Some(control),
        },
    )
    .unwrap();
}

#[test]
fn sigterm_withdraws_health_and_releases_the_writer_lock() {
    let servers = Servers::start();
    let temp = tempfile::tempdir().unwrap();
    let (runtime, supervisor) = fixture(temp.path());
    let spec = temp.path().join("signal-spec.json");
    fs::write(&spec, serde_json::to_vec(&json!({"runtime":runtime,"supervisor":supervisor,"http":servers.control.http,"ws":servers.control.ws})).unwrap()).unwrap();
    let mut child = hypercarry_supervisor::process::HelperProcess::spawn(
        ProcessCommand::new(std::env::current_exe().unwrap())
            .args(["--exact", "fault_harness::signal_child", "--nocapture"])
            .env("HYPERCARRY_TEST_SIGNAL_SPEC", spec)
            .stdin(Stdio::null())
            .stdout(Stdio::null()),
    )
    .unwrap();
    wait_for(|| {
        hypercarry_mainnet_config::read_json::<HealthEnvelope>(&runtime.health_file)
            .is_ok_and(|e| authorizes(&e))
    });
    assert!(lock_health(&runtime).is_err());
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(i32::try_from(child.child.id()).unwrap()).unwrap(),
        rustix::process::Signal::TERM,
    )
    .unwrap();
    wait_for(|| child.child.try_wait().unwrap().is_some());
    assert!(child.child.wait().unwrap().success());
    assert!(!runtime.health_file.exists());
    assert!(lock_health(&runtime).is_ok());
}
