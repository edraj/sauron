//! The load engine: an open-model rate scheduler with semaphore-gated
//! concurrency.
//!
//! A single scheduler loop ticks every [`SCHED_TICK`] and asks the pure rate
//! math (`schedule::*`) how many Identify / Event / Issue items are DUE by now.
//! For each due item it captures the item's *scheduled* instant and tries to
//! acquire a concurrency permit:
//!
//! * permit granted → a short-lived task builds the envelope, encodes it, POSTs
//!   it, and records a [`Sample`] whose latency is measured from the scheduled
//!   instant (coordinated-omission correction), then releases the permit;
//! * no permit free → the item is SHED and counted in `behind` — honest
//!   open-model load shedding rather than an unbounded queue.
//!
//! Scheduling stops at the deadline; the in-flight sends (≤ one each) finish and
//! keep the metrics channel open until they drop their sender clones, so the
//! aggregator returns only after every outstanding send has been accounted for.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, Semaphore};
use tokio::time::MissedTickBehavior;

use crate::cli::{self, RunConfig};
use crate::dsn::Target;
use crate::generator::{self, ItemCounts};
use crate::metrics::{self, Sample, Summary};
use crate::transport::{self, OutcomeKind, SendOutcome};
use crate::user::VirtualUser;
use crate::{netlimit, schedule};

/// How often the scheduler wakes to top up due items.
const SCHED_TICK: Duration = Duration::from_millis(5);

/// In `--live-sockets` mode, how often each held-open connection sends a slow
/// keep-alive trickle. The point of that mode is the SOCKET COUNT, not a rate,
/// so this stays deliberately lazy — just enough to keep the connection warm.
const HOLD_INTERVAL: Duration = Duration::from_secs(2);

/// RAII decrement of the in-flight counter: fires on ANY task exit, including a
/// panic, so a panicking send can never leave `inflight` (and thus the reported
/// peak) permanently inflated.
struct InflightGuard(Arc<AtomicUsize>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Which signal an emitted item carries — selects the envelope builder.
#[derive(Clone, Copy)]
enum Kind {
    Identify,
    Event,
    Issue,
}

/// Build the transport the engine sends through: UDS needs no source-IP
/// fan-out (a Unix-domain socket has no ephemeral-port wall), so it's just a
/// path + credentials; TCP wraps a [`transport::ReqwestPool`] fanned out
/// across `plan.source_ips`.
fn build_sender(
    cfg: &RunConfig,
    target: &Target,
    plan: &netlimit::FanoutPlan,
) -> anyhow::Result<transport::Sender> {
    match cfg.transport {
        cli::Transport::Uds => {
            let path = cfg
                .uds_path
                .clone()
                .ok_or_else(|| anyhow::anyhow!("uds transport requires a uds path"))?;
            Ok(transport::Sender::Uds {
                path,
                app_id: target.app_id.clone(),
                key: target.public_key.clone(),
                gzip: cfg.gzip,
            })
        }
        cli::Transport::Tcp => Ok(transport::Sender::Reqwest(transport::ReqwestPool::new(
            &target.base_url,
            &target.app_id,
            &target.public_key,
            cfg.gzip,
            &plan.source_ips,
        )?)),
    }
}

/// Run the load to completion and return the aggregated [`Summary`].
///
/// Dispatches on the run's reading of "concurrency": the default open-model
/// [`run_request_rate`] generator drives a REQUEST RATE, while
/// [`run_live_sockets`] instead holds `plan.effective` sockets open at once for
/// the whole run and reports PEAK CONNECTIONS — a connection-capacity demo, not
/// a request rate.
pub async fn run(
    cfg: &RunConfig,
    target: &Target,
    target_pid: Option<u32>,
    plan: &netlimit::FanoutPlan,
) -> anyhow::Result<Summary> {
    if cfg.live_sockets {
        run_live_sockets(cfg, target, target_pid, plan).await
    } else {
        run_request_rate(cfg, target, target_pid, plan).await
    }
}

/// The default open-model REQUEST-RATE engine: a scheduler ticks every
/// [`SCHED_TICK`], asks the rate math how many items are due, and spawns
/// semaphore-gated sends for each. Latency is measured from each item's
/// scheduled instant (coordinated-omission correction) and over-offer is shed.
async fn run_request_rate(
    cfg: &RunConfig,
    target: &Target,
    target_pid: Option<u32>,
    plan: &netlimit::FanoutPlan,
) -> anyhow::Result<Summary> {
    let sender = Arc::new(build_sender(cfg, target, plan)?);

    // The effective concurrency the port budget allows is the permit count.
    let w = plan.effective.max(1);
    let sem = Arc::new(Semaphore::new(w));

    // --- metrics wiring ---
    let (mtx, mrx) = mpsc::unbounded_channel::<Sample>();
    let behind = Arc::new(AtomicU64::new(0));
    let inflight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let start = Instant::now();
    let aggregator = tokio::spawn(metrics::aggregate(
        mrx,
        cfg.users,
        start,
        target_pid,
        behind.clone(),
        peak.clone(),
    ));

    // --- per-second offered rates across the whole user pool (items/sec) ---
    let mut events_rate = if cfg.event_interval.is_some() {
        cfg.users as f64 * cfg.events_per_min as f64 / 60.0
    } else {
        0.0
    };
    let mut issues_rate = if cfg.issue_interval.is_some() {
        cfg.users as f64 * cfg.issues_per_min as f64 / 60.0
    } else {
        0.0
    };
    if let Some(r) = cfg.rps {
        // Rescale so the two streams sum to the requested aggregate rate, split
        // by the events:issues per-minute ratio. A disabled stream (interval
        // None) keeps a zero weight; if both weights are zero, all of `r` goes
        // to events.
        let ew = if cfg.event_interval.is_some() {
            cfg.events_per_min as f64
        } else {
            0.0
        };
        let iw = if cfg.issue_interval.is_some() {
            cfg.issues_per_min as f64
        } else {
            0.0
        };
        let total = ew + iw;
        if total > 0.0 {
            events_rate = r * ew / total;
            issues_rate = r * iw / total;
        } else {
            events_rate = r;
            issues_rate = 0.0;
        }
    }

    // Emit one item: increment the caller's counters first, then try to acquire
    // a permit. On success spawn a send task that owns the permit; on failure
    // shed the item (count it as `behind`). All captures are cloned/copied so
    // the spawned task is 'static.
    let emit = |kind: Kind, index: usize, seq: u64| {
        // The item's scheduled instant — latency is measured from here.
        let scheduled = Instant::now();
        match sem.clone().try_acquire_owned() {
            Ok(permit) => {
                let now_inflight = inflight.fetch_add(1, Ordering::Relaxed) + 1;
                peak.fetch_max(now_inflight, Ordering::Relaxed);
                // Decrements `inflight` on any exit (incl. panic) once moved in.
                let _inflight = InflightGuard(inflight.clone());
                let sender = sender.clone();
                let mtx = mtx.clone();
                let gzip = cfg.gzip;
                tokio::spawn(async move {
                    let _inflight = _inflight;
                    let _permit = permit;
                    let mut u = VirtualUser::new(index);
                    let env = match kind {
                        Kind::Identify => generator::identify_envelope(&u),
                        Kind::Event => {
                            u.advance_screen(seq);
                            generator::event_envelope(&u, seq)
                        }
                        Kind::Issue => generator::issue_envelope(&u, seq),
                    };
                    let counts = ItemCounts::of(&env);
                    let body = match transport::encode(&env, gzip) {
                        Ok(b) => b,
                        Err(_) => {
                            let _ = mtx.send(Sample {
                                outcome: SendOutcome {
                                    kind: OutcomeKind::Transport,
                                    status: None,
                                },
                                counts,
                                latency: scheduled.elapsed(),
                            });
                            return;
                        }
                    };
                    let outcome = sender.send(index, &body).await;
                    let latency = scheduled.elapsed();
                    let _ = mtx.send(Sample {
                        outcome,
                        counts,
                        latency,
                    });
                });
            }
            Err(_) => {
                behind.fetch_add(1, Ordering::Relaxed);
            }
        }
    };

    // --- scheduler loop ---
    let n = cfg.users.max(1);
    let ramp = cfg.ramp.as_secs_f64();
    let mut emitted_id = 0u64;
    let mut emitted_ev = 0u64;
    let mut emitted_is = 0u64;
    let mut next_id = 0usize;
    let mut next_ev = 0usize;
    let mut next_is = 0usize;

    let mut ticker = tokio::time::interval(SCHED_TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let deadline = tokio::time::Instant::now() + cfg.duration;

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = ticker.tick() => {
                let elapsed = start.elapsed().as_secs_f64();

                let due_id =
                    schedule::ramp_identifies_due(cfg.users as u64, ramp, elapsed, emitted_id);
                for _ in 0..due_id {
                    let index = next_id;
                    next_id = (next_id + 1) % n;
                    emitted_id += 1;
                    emit(Kind::Identify, index, emitted_id);
                }

                let due_ev = schedule::items_due(events_rate, elapsed, emitted_ev);
                for _ in 0..due_ev {
                    let index = next_ev;
                    next_ev = (next_ev + 1) % n;
                    emitted_ev += 1;
                    emit(Kind::Event, index, emitted_ev);
                }

                let due_is = schedule::items_due(issues_rate, elapsed, emitted_is);
                for _ in 0..due_is {
                    let index = next_is;
                    next_is = (next_is + 1) % n;
                    emitted_is += 1;
                    emit(Kind::Issue, index, emitted_is);
                }
            }
        }
    }

    // Drop our own metrics-sender clone (the `emit` closure's borrow of it ends
    // with the loop's last use). Spawned sends still hold their own clones until
    // they finish, so the aggregator waits for every outstanding send to be
    // accounted for before it returns.
    drop(mtx);

    let mut summary = aggregator.await?;
    summary.effective_concurrency = plan.effective;
    summary.source_ips = plan.source_ips.len();
    Ok(summary)
}

/// How a single holder task opens its connection. Built once per holder on the
/// spawn loop so the task owns a `'static` copy (the source IP is picked here so
/// the TCP fan-out spreads holders across `plan.source_ips`).
#[derive(Clone)]
enum OpenSpec {
    Tcp {
        addr: std::net::SocketAddr,
        src: Option<std::net::Ipv4Addr>,
    },
    Uds {
        path: std::path::PathBuf,
    },
}

/// One held-open connection. Opens the socket, marks it in-flight for the whole
/// time it's held (so `peak_inflight` reflects SOCKETS OPEN AT ONCE), then sits
/// on a slow [`HOLD_INTERVAL`] keep-alive trickle until the deadline. A dead
/// connection is reported once and the holder stops — it is not reopened.
#[allow(clippy::too_many_arguments)]
async fn hold_one(
    i: usize,
    open: OpenSpec,
    key: String,
    app_id: String,
    gzip: bool,
    deadline: tokio::time::Instant,
    inflight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    mtx: mpsc::UnboundedSender<Sample>,
) {
    let mut conn = match &open {
        OpenSpec::Tcp { addr, src } => transport::RawConn::connect_tcp(*addr, *src).await,
        OpenSpec::Uds { path } => transport::RawConn::connect_uds(path).await,
    };
    let conn = match &mut conn {
        Ok(c) => c,
        Err(_) => {
            // Couldn't even open the socket — count it as a transport failure so
            // the summary reflects that this holder never contributed a socket.
            let _ = mtx.send(Sample {
                outcome: SendOutcome {
                    kind: OutcomeKind::Transport,
                    status: None,
                },
                counts: ItemCounts::default(),
                latency: Duration::ZERO,
            });
            return;
        }
    };

    // Mark this socket held for the whole time it stays open: bump the in-flight
    // gauge and peak BEFORE arming the RAII guard (the guard only decrements),
    // mirroring `run_request_rate`.
    let now_inflight = inflight.fetch_add(1, Ordering::Relaxed) + 1;
    peak.fetch_max(now_inflight, Ordering::Relaxed);
    let _g = InflightGuard(inflight.clone());

    let path = format!("/api/{app_id}/envelope");
    let mut seq = 0u64;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = tokio::time::sleep(HOLD_INTERVAL) => {
                seq += 1;
                let mut user = VirtualUser::new(i);
                user.advance_screen(seq);
                let env = generator::event_envelope(&user, seq);
                let counts = ItemCounts::of(&env);
                let body = match transport::encode(&env, gzip) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                let scheduled = Instant::now();
                let outcome = conn.post(&path, "localhost", &key, &body, gzip).await;
                let dead = outcome.kind == OutcomeKind::Transport;
                let _ = mtx.send(Sample {
                    outcome,
                    counts,
                    latency: scheduled.elapsed(),
                });
                // The connection died mid-run; stop holding a dead socket.
                if dead {
                    break;
                }
            }
        }
    }
}

/// The `--live-sockets` engine: open `plan.effective` connections, hold them ALL
/// open simultaneously for the run, and send only a slow keep-alive trickle on
/// each. This is a connection-CAPACITY demo — the headline is peak concurrent
/// sockets (surfaced as `peak_inflight`), never a request rate. Works over TCP
/// (fanned out across `plan.source_ips`) and UDS.
async fn run_live_sockets(
    cfg: &RunConfig,
    target: &Target,
    target_pid: Option<u32>,
    plan: &netlimit::FanoutPlan,
) -> anyhow::Result<Summary> {
    // --- metrics wiring (identical to run_request_rate; `behind` stays 0) ---
    let (mtx, mrx) = mpsc::unbounded_channel::<Sample>();
    let behind = Arc::new(AtomicU64::new(0));
    let inflight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let start = Instant::now();
    let aggregator = tokio::spawn(metrics::aggregate(
        mrx,
        cfg.users,
        start,
        target_pid,
        behind.clone(),
        peak.clone(),
    ));

    let n = plan.effective;
    let deadline = tokio::time::Instant::now() + cfg.duration;

    // Resolve the connection factory once. TCP parses the socket addr out of the
    // base URL; UDS carries the socket path.
    enum Factory {
        Tcp(std::net::SocketAddr),
        Uds(std::path::PathBuf),
    }
    let factory = match cfg.transport {
        cli::Transport::Tcp => {
            let addr: std::net::SocketAddr = target
                .base_url
                .split("://")
                .nth(1)
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| anyhow::anyhow!("bad target addr for live-sockets"))?;
            Factory::Tcp(addr)
        }
        cli::Transport::Uds => {
            let path = cfg
                .uds_path
                .clone()
                .ok_or_else(|| anyhow::anyhow!("uds transport requires a uds path for live-sockets"))?;
            Factory::Uds(path)
        }
    };

    let key = target.public_key.clone();
    let app_id = target.app_id.clone();
    let gzip = cfg.gzip;

    // Open up to `n` holders BATCHED across the ramp window: every 5ms tick asks
    // the ramp math how many opens are due by now and spawns that whole batch at
    // once. A per-open sleep would be throttled to tokio's ~1ms timer
    // granularity — for large n that caps opens at ~1000/s, so a big fan-out
    // never reaches `effective` within the ramp. Batching per tick sidesteps that
    // granularity wall. `--ramp 0` returns all `n` on the first tick (open all at
    // once); the deadline arm stops opening once the run window closes.
    let mut holders = Vec::with_capacity(n);
    let mut spawned = 0usize;
    let ramp_secs = cfg.ramp.as_secs_f64();
    let ramp_start = std::time::Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_millis(5));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = ticker.tick() => {
                let due = schedule::ramp_identifies_due(
                    n as u64,
                    ramp_secs,
                    ramp_start.elapsed().as_secs_f64(),
                    spawned as u64,
                ) as usize;
                for _ in 0..due {
                    let i = spawned;
                    let open = match &factory {
                        Factory::Tcp(addr) => {
                            let src = if plan.source_ips.is_empty() {
                                None
                            } else {
                                Some(plan.source_ips[i % plan.source_ips.len()])
                            };
                            OpenSpec::Tcp { addr: *addr, src }
                        }
                        Factory::Uds(path) => OpenSpec::Uds { path: path.clone() },
                    };
                    holders.push(tokio::spawn(hold_one(
                        i,
                        open,
                        key.clone(),
                        app_id.clone(),
                        gzip,
                        deadline,
                        inflight.clone(),
                        peak.clone(),
                        mtx.clone(),
                    )));
                    spawned += 1;
                }
                if spawned >= n {
                    break;
                }
            }
        }
    }

    // Hold everything open until the deadline, then drop our own metrics sender.
    // Each holder keeps its own `mtx` clone until its select breaks at the
    // deadline (and its in-flight post returns), so the aggregator only sees the
    // channel close — and returns — after every socket has been accounted for.
    tokio::time::sleep_until(deadline).await;
    drop(mtx);
    for h in holders {
        let _ = h.await;
    }

    let mut summary = aggregator.await?;
    summary.effective_concurrency = plan.effective;
    summary.source_ips = plan.source_ips.len();
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::run;
    use crate::cli::{RunConfig, Transport};
    use crate::dsn::Target;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Minimal raw-TCP mock ingest: replies 202 to every request and records the
    /// peak number of connections it was serving concurrently.
    async fn mock_ingest(peak: Arc<AtomicUsize>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cur = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let (mut sock, _peer) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let (cur, peak) = (cur.clone(), peak.clone());
                tokio::spawn(async move {
                    let n = cur.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(n, Ordering::SeqCst);
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 8192];
                    let _ = sock.read(&mut buf).await;
                    // Small hold so concurrent sends actually overlap on the wire.
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    let _ = sock
                        .write_all(b"HTTP/1.1 202 Accepted\r\ncontent-length: 2\r\n\r\nok")
                        .await;
                    cur.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn run_hits_mock_and_bounds_inflight() {
        let mock_peak = Arc::new(AtomicUsize::new(0));
        let base = mock_ingest(mock_peak.clone()).await;

        let cfg = RunConfig {
            users: 200,
            duration: Duration::from_millis(800),
            event_interval: Some(Duration::from_millis(100)),
            issue_interval: Some(Duration::from_millis(100)),
            gzip: false,
            events_per_min: 600,
            issues_per_min: 60,
            report_path: None,
            max_inflight: 4,
            ramp: Duration::from_millis(100),
            source_ips: None,
            transport: Transport::Tcp,
            uds_path: None,
            live_sockets: false,
            rps: None,
        };
        let target = Target {
            base_url: base,
            app_id: "app".into(),
            public_key: "k".into(),
        };

        let plan = crate::netlimit::plan_fanout(4, true, crate::netlimit::ephemeral_port_budget(), 512, None);
        let s = run(&cfg, &target, None, &plan).await.unwrap();

        assert!(s.requests > 0, "expected some requests, got {}", s.requests);
        assert!(s.accepted > 0, "expected some accepted, got {}", s.accepted);
        assert!(
            s.peak_inflight <= 4,
            "engine peak_inflight {} exceeded cap 4",
            s.peak_inflight
        );
        let observed = mock_peak.load(Ordering::SeqCst);
        assert!(
            observed <= 4,
            "mock observed {observed} concurrent connections, cap is 4"
        );
    }

    /// A raw-TCP mock whose per-connection handler LOOPS: it serves repeated
    /// keep-alive requests on the same socket until the client hangs up, and
    /// records the PEAK number of connections it held open at the same time
    /// (fetch_add on accept, fetch_max into `peak`, fetch_sub on disconnect).
    /// Unlike `mock_ingest`, a connection is counted for as long as the client
    /// holds it open — which is exactly what `--live-sockets` exercises.
    async fn mock_ingest_hold(peak: Arc<AtomicUsize>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cur = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let (mut sock, _peer) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let (cur, peak) = (cur.clone(), peak.clone());
                tokio::spawn(async move {
                    let n = cur.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(n, Ordering::SeqCst);
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = Vec::new();
                    'conn: loop {
                        // Read until a full request head shows up, then answer it.
                        loop {
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                            let mut chunk = [0u8; 1024];
                            match sock.read(&mut chunk).await {
                                Ok(0) | Err(_) => break 'conn, // client hung up
                                Ok(m) => buf.extend_from_slice(&chunk[..m]),
                            }
                        }
                        buf.clear();
                        if sock
                            .write_all(b"HTTP/1.1 202 Accepted\r\ncontent-length: 2\r\nconnection: keep-alive\r\n\r\nok")
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    cur.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn live_sockets_holds_connections_open() {
        let mock_peak = Arc::new(AtomicUsize::new(0));
        let base = mock_ingest_hold(mock_peak.clone()).await;

        let cfg = RunConfig {
            users: 40,
            duration: Duration::from_millis(900),
            event_interval: None,
            issue_interval: None,
            gzip: false,
            events_per_min: 0,
            issues_per_min: 0,
            report_path: None,
            max_inflight: 40,
            ramp: Duration::from_millis(100),
            source_ips: None,
            transport: Transport::Tcp,
            uds_path: None,
            live_sockets: true,
            rps: None,
        };
        let target = Target {
            base_url: base,
            app_id: "app".into(),
            public_key: "k".into(),
        };
        let plan = crate::netlimit::FanoutPlan {
            source_ips: vec![],
            effective: 40,
            warning: None,
        };

        let s = run(&cfg, &target, None, &plan).await.unwrap();

        // The mock should have seen ~40 sockets open AT THE SAME TIME — proving
        // the connections were held simultaneously, not churned one at a time.
        let observed = mock_peak.load(Ordering::SeqCst);
        assert!(
            observed >= 35,
            "expected ~40 simultaneous connections at the mock, saw peak {observed}"
        );
        // And the engine's own in-flight gauge (each held socket counts once) must
        // reflect the same peak.
        assert!(
            s.peak_inflight >= 35,
            "expected engine peak_inflight ~40, got {}",
            s.peak_inflight
        );
    }

    /// A raw-UDS mock ingest server: one connection per request, replies
    /// `202 Accepted` with a 2-byte body, then closes.
    #[cfg(unix)]
    async fn mock_ingest_uds(path: &std::path::Path) {
        let listener = tokio::net::UnixListener::bind(path).unwrap();
        tokio::spawn(async move {
            loop {
                let (mut sock, _peer) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 202 Accepted\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                        )
                        .await;
                });
            }
        });
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_over_uds_accepts() {
        let path = std::env::temp_dir().join(format!(
            "crebain-engine-test-{}.sock",
            uuid::Uuid::new_v4().simple()
        ));
        let _ = std::fs::remove_file(&path);
        mock_ingest_uds(&path).await;

        let cfg = RunConfig {
            users: 50,
            duration: Duration::from_millis(500),
            event_interval: Some(Duration::from_millis(100)),
            issue_interval: Some(Duration::from_millis(100)),
            gzip: false,
            events_per_min: 600,
            issues_per_min: 60,
            report_path: None,
            max_inflight: 4,
            ramp: Duration::from_millis(50),
            source_ips: None,
            transport: Transport::Uds,
            uds_path: Some(path.clone()),
            live_sockets: false,
            rps: None,
        };
        let target = Target {
            base_url: "http://unused".into(),
            app_id: "app".into(),
            public_key: "k".into(),
        };
        let plan = crate::netlimit::FanoutPlan {
            source_ips: vec![],
            effective: 4,
            warning: None,
        };

        let s = run(&cfg, &target, None, &plan).await.unwrap();

        assert!(s.requests > 0, "expected some requests, got {}", s.requests);
        assert!(s.accepted > 0, "expected some accepted, got {}", s.accepted);

        let _ = std::fs::remove_file(&path);
    }
}
