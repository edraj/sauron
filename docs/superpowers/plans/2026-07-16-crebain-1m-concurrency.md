# crebain 1M-Concurrency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let crebain genuinely drive 1,000,000 concurrent users — both the portable "1M distinct identities at aggregate load" reading (default) and the localhost "1M sockets literally open at once" reading (`--live-sockets`, incl. a Unix-domain-socket transport) — while honestly reporting the real peak achieved.

**Architecture:** Replace the one-tokio-task-per-user engine with an **open-model rate scheduler → bounded worker pool** over a pluggable **transport** (reqwest TCP pool with `127.0.0.0/8` source-IP fan-out; a hand-rolled raw HTTP/1.1 sender over TCP or Unix sockets for the literal-sockets and UDS paths). A pure `netlimit` module plans the ephemeral-port budget / source-IP count and raises `RLIMIT_NOFILE`. Latency is timed from each item's **scheduled** send time (coordinated-omission correction), and the report surfaces offered-vs-accepted req/s, transport-error %, and peak concurrent connections.

**Tech Stack:** Rust, tokio (`full`), reqwest 0.12 (rustls-tls + gzip), flate2, libc (new, for `setrlimit`), axum 0.8 / tokio `UnixListener` (ingest side).

## Global Constraints

- Work stays on `main`; do NOT create branches. Do NOT commit unless the user explicitly asks (user's standing rule). Steps below include `commit` actions — treat them as **checkpoints**: run them ONLY if the user has opted into commits; otherwise skip the commit step and keep going.
- Rust edition/toolchain: workspace defaults (`version.workspace`, `edition.workspace`). Do not bump crate versions.
- New dependency policy: only `libc` (crebain + sauron-ingest), already present transitively in the lockfile. No `hyper`/`hyper-util`/`socket2`/`http2` additions — the raw sender is hand-rolled; the server backlog uses `tokio::net::TcpSocket`.
- Source-IP fan-out (`127.0.0.x`) is enabled ONLY when the resolved target host is loopback; against a non-loopback target it must NOT be used (a 127.x source cannot route to a remote host).
- Every mode must report the **real peak achieved** and the binding resource, and must never present a held-socket count as a req/s result nor an offered rate as a served rate.
- Preserve existing behavior: `--dsn` and `--isolated` modes, self-cleaning teardown, five signal types, deterministic identity generation (`crebain-user-{index}`), Ctrl-C/SIGTERM handling.
- Build/test commands run from repo root: `cargo build -p crebain -p sauron-ingest`, `cargo test -p crebain`, `cargo clippy -p crebain -p sauron-ingest --all-targets`.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `backend/bins/crebain/src/netlimit.rs` | Pure port-budget/source-IP planner + `raise_nofile` (libc) + loopback detection | **Create** |
| `backend/bins/crebain/src/transport.rs` | `SendOutcome`/`OutcomeKind`, envelope encode (json/gzip), `Transport` enum: `ReqwestPool` (TCP + src-IP fan-out) and `RawSender` (HTTP/1.1 over TCP/UDS) | **Create** (absorbs `client.rs`) |
| `backend/bins/crebain/src/client.rs` | — | **Delete** (moved to `transport.rs`) |
| `backend/bins/crebain/src/schedule.rs` | Pure `items_due()` rate math + the async work-generating scheduler | **Create** |
| `backend/bins/crebain/src/engine.rs` | Wire scheduler + worker pool + metrics; deadline; CO-corrected latency | **Rewrite** |
| `backend/bins/crebain/src/cli.rs` | New flags → `RunConfig`; validation | **Modify** |
| `backend/bins/crebain/src/metrics.rs` | Add `behind`, peak-inflight, peak-connections, offered-vs-accepted | **Modify** |
| `backend/bins/crebain/src/procstat.rs` | Add `/proc/<pid>/fd` count sampling | **Modify** |
| `backend/bins/crebain/src/report.rs` | Print new fields + honesty notes | **Modify** |
| `backend/bins/crebain/src/report_html.rs` | Report meta + cards for new fields | **Modify** |
| `backend/bins/crebain/src/main.rs` | Raise nofile early; banner lines; plumb plan | **Modify** |
| `backend/bins/crebain/src/harness.rs` | Spawn ingest with transport env (UDS) + inherited rlimit | **Modify** |
| `backend/bins/crebain/Cargo.toml` | `+ libc` | **Modify** |
| `backend/bins/sauron-ingest/src/main.rs` | Optional UDS listen (`INGEST_UDS_PATH`); `TcpSocket` backlog; `setrlimit` | **Modify** |
| `backend/bins/sauron-ingest/Cargo.toml` | `+ libc` | **Modify** |
| `backend/crates/sauron-core/src/config.rs` | `+ ingest_uds_path`, `+ ingest_backlog` | **Modify** |
| `backend/bins/crebain/README.md` | Document flags + the physics + fd/limit behavior | **Modify** |

---

# MILESTONE 1 — Tier 1: portable bounded-pool engine (the actual fix)

Delivers a correct, portable 1M-*identity* run against any target: open-model scheduler, bounded worker pool over a keep-alive reqwest pool with loopback source-IP fan-out, phased identify, CO-corrected latency, honest reporting. Independently shippable.

### Task 1: `netlimit` — port budget, loopback detection, source-IP planning (pure)

**Files:**
- Create: `backend/bins/crebain/src/netlimit.rs`
- Modify: `backend/bins/crebain/src/main.rs` (add `mod netlimit;`)

**Interfaces:**
- Produces:
  - `pub fn ephemeral_port_budget() -> u32` — usable ports/tuple from `/proc/sys/net/ipv4/ip_local_port_range` (fallback `28232`).
  - `pub fn is_loopback_host(host: &str) -> bool`
  - `pub fn nth_loopback_ip(i: usize) -> std::net::Ipv4Addr` — 0-indexed walk of `127.0.0.0/8` starting at `127.0.0.1`, skipping `.0`/`.255` in each low octet.
  - `pub struct FanoutPlan { pub source_ips: Vec<std::net::Ipv4Addr>, pub effective: usize, pub warning: Option<String> }`
  - `pub fn plan_fanout(requested: usize, loopback: bool, per_ip_budget: u32, max_ips: usize, source_ips_override: Option<usize>) -> FanoutPlan`

- [ ] **Step 1: Write failing tests**

```rust
// in netlimit.rs, #[cfg(test)] mod tests
use super::*;
use std::net::Ipv4Addr;

#[test]
fn loopback_detection() {
    assert!(is_loopback_host("127.0.0.1"));
    assert!(is_loopback_host("localhost"));
    assert!(is_loopback_host("::1"));
    assert!(!is_loopback_host("example.com"));
    assert!(!is_loopback_host("10.0.0.5"));
}

#[test]
fn nth_loopback_walks_127_8_skipping_edges() {
    assert_eq!(nth_loopback_ip(0), Ipv4Addr::new(127, 0, 0, 1));
    assert_eq!(nth_loopback_ip(1), Ipv4Addr::new(127, 0, 0, 2));
    // 0-index 253 -> 127.0.0.254 (last of first low block, skipping .0/.255)
    assert_eq!(nth_loopback_ip(253), Ipv4Addr::new(127, 0, 0, 254));
    // wraps into the next 'c' octet, again starting at .1
    assert_eq!(nth_loopback_ip(254), Ipv4Addr::new(127, 0, 1, 1));
}

#[test]
fn plan_single_ip_when_under_budget() {
    let p = plan_fanout(1000, true, 28232, 512, None);
    assert_eq!(p.source_ips.len(), 1);
    assert_eq!(p.effective, 1000);
    assert!(p.warning.is_none());
}

#[test]
fn plan_fans_out_to_cover_requested_on_loopback() {
    // 1,000,000 / 28232 = ceil 36
    let p = plan_fanout(1_000_000, true, 28232, 512, None);
    assert_eq!(p.source_ips.len(), 36);
    assert_eq!(p.effective, 1_000_000);
}

#[test]
fn plan_caps_effective_on_remote_single_ip_with_warning() {
    let p = plan_fanout(1_000_000, false, 28232, 512, None);
    assert_eq!(p.source_ips.len(), 1);
    assert_eq!(p.effective, 28232);
    assert!(p.warning.is_some());
}

#[test]
fn plan_honors_max_ips_cap_and_warns() {
    let p = plan_fanout(100_000_000, true, 28232, 8, None);
    assert_eq!(p.source_ips.len(), 8);
    assert_eq!(p.effective, 8 * 28232);
    assert!(p.warning.is_some());
}
```

- [ ] **Step 2: Run to confirm fail** — `cargo test -p crebain netlimit::` → FAIL (module missing).

- [ ] **Step 3: Implement**

```rust
//! Ephemeral-port budget planning, loopback detection, and RLIMIT_NOFILE raising.
//! Pure functions are unit-tested; the libc calls are best-effort and reported.

use std::net::Ipv4Addr;

pub const FALLBACK_PORT_BUDGET: u32 = 28_232;
/// Fraction of a tuple's raw port range we treat as usable (TIME_WAIT headroom).
const USABLE_FRACTION: f64 = 0.9;

/// Usable simultaneous connections per (src_ip, dst_ip, dst_port) tuple, read from
/// `/proc/sys/net/ipv4/ip_local_port_range` and de-rated for TIME_WAIT headroom.
pub fn ephemeral_port_budget() -> u32 {
    let raw = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|s| {
            let mut it = s.split_whitespace();
            let lo: u32 = it.next()?.parse().ok()?;
            let hi: u32 = it.next()?.parse().ok()?;
            (hi >= lo).then_some(hi - lo + 1)
        })
        .unwrap_or(FALLBACK_PORT_BUDGET);
    ((raw as f64 * USABLE_FRACTION) as u32).max(1)
}

pub fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(v4) = host.parse::<Ipv4Addr>() {
        return v4.is_loopback();
    }
    if let Ok(v6) = host.parse::<std::net::Ipv6Addr>() {
        return v6.is_loopback();
    }
    false
}

/// The i-th (0-indexed) usable loopback source address, walking 127.0.0.0/8 as
/// 127.0.0.1, 127.0.0.2, … 127.0.0.254, 127.0.1.1, … (skip .0 and .255 in the low octet).
pub fn nth_loopback_ip(i: usize) -> Ipv4Addr {
    let per_block = 254usize; // .1 ..= .254
    let block = i / per_block; // increments the third octet
    let within = (i % per_block) as u8; // 0 ..= 253
    let c = (block % 256) as u8;
    let b = ((block / 256) % 256) as u8;
    Ipv4Addr::new(127, b, c, within + 1)
}

#[derive(Debug, Clone)]
pub struct FanoutPlan {
    pub source_ips: Vec<Ipv4Addr>,
    pub effective: usize,
    pub warning: Option<String>,
}

/// Decide how many loopback source IPs to bind and the effective concurrency the
/// port budget allows. Non-loopback → single IP (127.x can't reach a remote), so
/// effective is capped at one tuple's budget with a warning.
pub fn plan_fanout(
    requested: usize,
    loopback: bool,
    per_ip_budget: u32,
    max_ips: usize,
    source_ips_override: Option<usize>,
) -> FanoutPlan {
    let per = per_ip_budget as usize;
    let mut warning = None;

    let want_ips = if !loopback {
        1
    } else if let Some(n) = source_ips_override {
        n.max(1)
    } else {
        requested.div_ceil(per).max(1)
    };
    let ips = want_ips.min(max_ips.max(1));
    if loopback && ips < want_ips {
        warning = Some(format!(
            "capped at {ips} source IPs (max-ips); need {want_ips} for {requested} concurrent"
        ));
    }
    if !loopback && requested > per {
        warning = Some(format!(
            "non-loopback target: one source IP allows ~{per} concurrent, but {requested} requested; \
             excess would exhaust ephemeral ports (use distributed workers for more)"
        ));
    }

    let capacity = ips.saturating_mul(per);
    let effective = requested.min(capacity);
    let source_ips = (0..ips).map(nth_loopback_ip).collect();
    FanoutPlan {
        source_ips,
        effective,
        warning,
    }
}
```

- [ ] **Step 4: Run tests** — `cargo test -p crebain netlimit::` → PASS.
- [ ] **Step 5: Commit** (checkpoint) — `git add -A && git commit -m "feat(crebain): netlimit port-budget + source-IP fan-out planner"`

---

### Task 2: `netlimit::raise_nofile` — raise the fd soft limit (libc)

**Files:**
- Modify: `backend/bins/crebain/src/netlimit.rs`
- Modify: `backend/bins/crebain/Cargo.toml` (add `libc = "0.2"`)

**Interfaces:**
- Produces: `pub struct NofileStatus { pub requested: u64, pub soft: u64, pub hard: u64, pub capped: bool }` and `pub fn raise_nofile(desired: u64) -> NofileStatus`.

- [ ] **Step 1: Add dep** — in `[dependencies]` add `libc = "0.2"`.

- [ ] **Step 2: Write failing test**

```rust
#[cfg(target_os = "linux")]
#[test]
fn raise_nofile_reports_soft_at_least_min_of_desired_and_hard() {
    let before = raise_nofile(0); // no-op read of current
    // Ask for +1024 over current soft, bounded by hard.
    let want = before.soft + 1024;
    let st = raise_nofile(want);
    assert!(st.hard >= st.soft, "soft must not exceed hard");
    assert!(st.soft >= before.soft.min(st.hard), "soft should not drop");
    assert_eq!(st.requested, want);
    assert_eq!(st.capped, want > st.hard);
}
```

- [ ] **Step 3: Run to confirm fail** — `cargo test -p crebain raise_nofile` → FAIL.

- [ ] **Step 4: Implement**

```rust
/// Result of a best-effort RLIMIT_NOFILE raise.
#[derive(Debug, Clone, Copy)]
pub struct NofileStatus {
    pub requested: u64,
    pub soft: u64,
    pub hard: u64,
    pub capped: bool, // true when `requested` exceeded the hard cap
}

/// Raise the open-file soft limit toward `desired` (clamped to the hard cap). A
/// process may raise soft up to hard but cannot raise the hard cap unprivileged;
/// pass `0` to just read the current limits. Non-Linux / failure → reports what it
/// could read (or zeros) and never panics.
pub fn raise_nofile(desired: u64) -> NofileStatus {
    #[cfg(unix)]
    unsafe {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return NofileStatus { requested: desired, soft: 0, hard: 0, capped: false };
        }
        let hard = lim.rlim_max as u64;
        if desired > 0 {
            let target = desired.min(hard);
            if (target as libc::rlim_t) > lim.rlim_cur {
                let new = libc::rlimit { rlim_cur: target as libc::rlim_t, rlim_max: lim.rlim_max };
                let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &new);
                let _ = libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim);
            }
        }
        NofileStatus {
            requested: desired,
            soft: lim.rlim_cur as u64,
            hard,
            capped: desired > hard,
        }
    }
    #[cfg(not(unix))]
    {
        NofileStatus { requested: desired, soft: 0, hard: 0, capped: false }
    }
}
```

- [ ] **Step 5: Run tests** — `cargo test -p crebain netlimit::` → PASS.
- [ ] **Step 6: Commit** (checkpoint) — `git commit -am "feat(crebain): raise RLIMIT_NOFILE via libc"`

---

### Task 3: CLI flags + `RunConfig` + validation

**Files:**
- Modify: `backend/bins/crebain/src/cli.rs`

**Interfaces:**
- Produces (added to `RunConfig`): `pub max_inflight: usize`, `pub ramp: std::time::Duration`, `pub source_ips: Option<usize>`, `pub transport: Transport`, `pub uds_path: Option<PathBuf>`, `pub live_sockets: bool`, `pub rps: Option<f64>`.
- New enum: `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Transport { Tcp, Uds }` (clap `ValueEnum`).

- [ ] **Step 1: Write failing tests** (extend `cli::tests`)

```rust
#[test]
fn defaults_max_inflight_and_transport() {
    let args = Args::try_parse_from(["crebain", "--isolated", "--database-url", "postgres://x/y"]).unwrap();
    let (cfg, _m) = args.resolve().unwrap();
    assert_eq!(cfg.max_inflight, 8192);
    assert_eq!(cfg.transport, Transport::Tcp);
    assert!(!cfg.live_sockets);
    assert_eq!(cfg.ramp, std::time::Duration::from_secs(5));
}

#[test]
fn parses_concurrency_and_uds() {
    let args = Args::try_parse_from([
        "crebain", "--isolated", "--database-url", "postgres://x/y",
        "--max-inflight", "50000", "--transport", "uds", "--live-sockets", "--ramp", "10",
    ]).unwrap();
    let (cfg, _m) = args.resolve().unwrap();
    assert_eq!(cfg.max_inflight, 50000);
    assert_eq!(cfg.transport, Transport::Uds);
    assert!(cfg.live_sockets);
    assert_eq!(cfg.ramp, std::time::Duration::from_secs(10));
}

#[test]
fn rejects_max_inflight_zero() {
    let args = Args::try_parse_from([
        "crebain", "--isolated", "--database-url", "postgres://x/y", "--max-inflight", "0",
    ]).unwrap();
    assert!(args.resolve().is_err());
}
```

- [ ] **Step 2: Run to confirm fail** — `cargo test -p crebain cli::` → FAIL.

- [ ] **Step 3: Implement** — add to `Args`:

```rust
/// Max concurrent in-flight requests (= worker-pool size). The connection ceiling.
#[arg(long = "max-inflight", default_value_t = 8192)]
pub max_inflight: usize,

/// Seconds over which the initial per-user identify is spread (no t=0 herd).
#[arg(long, default_value_t = 5)]
pub ramp: u64,

/// Explicit aggregate request rate (req/s). Overrides the users×rates derivation.
#[arg(long)]
pub rps: Option<f64>,

/// Number of loopback source IPs to fan out across (default: auto from --max-inflight).
#[arg(long = "source-ips")]
pub source_ips: Option<usize>,

/// Transport for requests.
#[arg(long, value_enum, default_value_t = Transport::Tcp)]
pub transport: Transport,

/// Unix-domain-socket path (isolated mode auto-picks one when --transport uds).
#[arg(long = "uds-path")]
pub uds_path: Option<PathBuf>,

/// Hold connections open for a literal-concurrency (peak-sockets) demo instead of a request loop.
#[arg(long = "live-sockets")]
pub live_sockets: bool,
```

Add the enum + `use clap::ValueEnum;` and `use std::path::PathBuf;`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Transport { Tcp, Uds }
```

Thread new fields into `RunConfig` (add the fields listed in Interfaces), set them in `resolve()`, and validate:

```rust
if self.max_inflight == 0 {
    anyhow::bail!("--max-inflight must be at least 1");
}
```

- [ ] **Step 4: Run tests** — `cargo test -p crebain cli::` → PASS.
- [ ] **Step 5: Commit** (checkpoint) — `git commit -am "feat(crebain): CLI knobs for concurrency, ramp, transport, live-sockets"`

---

### Task 4: `transport.rs` — `SendOutcome` + encode + `ReqwestPool` with source-IP fan-out

**Files:**
- Create: `backend/bins/crebain/src/transport.rs` (move `SendOutcome`/`OutcomeKind`/encode from `client.rs`)
- Delete: `backend/bins/crebain/src/client.rs`
- Modify: `main.rs` (`mod transport;` replaces `mod client;`), `metrics.rs`/`engine.rs` imports (`crate::transport::` instead of `crate::client::`)

**Interfaces:**
- Produces:
  - `pub enum OutcomeKind { Accepted, RateLimited, HttpError, Transport }`, `pub struct SendOutcome { pub kind: OutcomeKind, pub status: Option<u16> }` (latency now measured by the engine from scheduled time — dropped from `SendOutcome`).
  - `pub fn encode(env: &Envelope, gzip: bool) -> anyhow::Result<Vec<u8>>`
  - `pub struct ReqwestPool { /* Vec<reqwest::Client>, urls, key, gzip */ }` with `pub fn new(base_url: &str, app_id: &str, key: &str, gzip: bool, source_ips: &[std::net::Ipv4Addr]) -> anyhow::Result<Self>`, `pub fn conns(&self) -> usize`, and `pub async fn send(&self, slot: usize, body: &[u8]) -> SendOutcome`.

- [ ] **Step 1: Write failing test — mock server proves cap + fan-out**

```rust
// transport.rs #[cfg(test)] mod tests — a raw TCP mock server that records peak
// concurrent connections and the set of distinct client source IPs.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::collections::BTreeSet;

async fn mock_ingest() -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<Mutex<BTreeSet<std::net::IpAddr>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cur = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let ips = Arc::new(Mutex::new(BTreeSet::new()));
    let (c, p, i) = (cur.clone(), peak.clone(), ips.clone());
    tokio::spawn(async move {
        loop {
            let (mut sock, peer) = listener.accept().await.unwrap();
            let (c, p, i) = (c.clone(), p.clone(), i.clone());
            tokio::spawn(async move {
                i.lock().unwrap().insert(peer.ip());
                let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(n, Ordering::SeqCst);
                // read request head, small delay so overlaps are observable
                let mut buf = [0u8; 2048];
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let _ = sock.read(&mut buf).await;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                let _ = sock.write_all(b"HTTP/1.1 202 Accepted\r\ncontent-length: 2\r\n\r\nok").await;
                c.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    (format!("http://{addr}"), cur, peak, ips)
}

#[tokio::test]
async fn pool_bounds_concurrency_and_fans_out_source_ips() {
    let (base, _cur, peak, ips) = mock_ingest().await;
    // NOTE: engine enforces the cap via a fixed worker count; here we drive `cap`
    // workers over the pool directly to prove the observable effect.
    let cap = 5;
    let src = [127u8].iter().map(|_| ()).enumerate()
        .map(|(k, _)| std::net::Ipv4Addr::new(127, 0, 0, (k + 1) as u8))
        .collect::<Vec<_>>(); // 127.0.0.1
    let src = vec![std::net::Ipv4Addr::new(127,0,0,1), std::net::Ipv4Addr::new(127,0,0,2), std::net::Ipv4Addr::new(127,0,0,3)];
    let pool = ReqwestPool::new(&base, "app", "k", false, &src).unwrap();
    let body = b"{}".to_vec();
    // Fire 50 requests through exactly `cap` concurrent workers round-robining slots.
    let pool = std::sync::Arc::new(pool);
    let mut handles = vec![];
    let counter = std::sync::Arc::new(AtomicUsize::new(0));
    for w in 0..cap {
        let (pool, body, counter) = (pool.clone(), body.clone(), counter.clone());
        handles.push(tokio::spawn(async move {
            loop {
                let i = counter.fetch_add(1, Ordering::SeqCst);
                if i >= 50 { break; }
                let _ = pool.send(i, &body).await;
                let _ = w;
            }
        }));
    }
    for h in handles { h.await.unwrap(); }
    assert!(peak.load(Ordering::SeqCst) <= cap, "peak {} exceeded cap {cap}", peak.load(Ordering::SeqCst));
    let seen = ips.lock().unwrap().clone();
    assert!(seen.len() >= 2, "expected multiple source IPs, saw {seen:?}");
}
```

- [ ] **Step 2: Run to confirm fail** — `cargo test -p crebain transport::` → FAIL (module missing).

- [ ] **Step 3: Implement `transport.rs`** (ReqwestPool portion). Move `encode` from `client.rs`. Build one `reqwest::Client` per source IP with `.local_address(IpAddr::V4(ip))`; if `source_ips` is empty, build a single client with no `local_address`. `send(slot, body)` selects `clients[slot % clients.len()]`, POSTs to `url`, classifies status → `SendOutcome`, drains body for keep-alive. Keep `.no_proxy()`, `.pool_max_idle_per_host(usize::MAX)`, `.timeout(30s)`.

```rust
pub struct ReqwestPool {
    clients: Vec<reqwest::Client>,
    url: String,
    key: String,
    gzip: bool,
}
impl ReqwestPool {
    pub fn new(base_url: &str, app_id: &str, key: &str, gzip: bool, source_ips: &[std::net::Ipv4Addr]) -> anyhow::Result<Self> {
        let build = |local: Option<std::net::IpAddr>| {
            let mut b = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .pool_max_idle_per_host(usize::MAX)
                .no_proxy();
            if let Some(ip) = local { b = b.local_address(ip); }
            b.build()
        };
        let clients = if source_ips.is_empty() {
            vec![build(None)?]
        } else {
            source_ips.iter().map(|ip| build(Some(std::net::IpAddr::V4(*ip)))).collect::<Result<_,_>>()?
        };
        Ok(Self { clients, url: format!("{base_url}/api/{app_id}/envelope"), key: key.to_string(), gzip })
    }
    pub fn conns(&self) -> usize { self.clients.len() }
    pub async fn send(&self, slot: usize, body: &[u8]) -> SendOutcome {
        let client = &self.clients[slot % self.clients.len()];
        let mut req = client.post(&self.url).header("x-sauron-key", &self.key)
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        if self.gzip { req = req.header(reqwest::header::CONTENT_ENCODING, "gzip"); }
        match req.body(body.to_vec()).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let _ = resp.bytes().await;
                let kind = if (200..300).contains(&status) { OutcomeKind::Accepted }
                    else if status == 429 { OutcomeKind::RateLimited } else { OutcomeKind::HttpError };
                SendOutcome { kind, status: Some(status) }
            }
            Err(_) => SendOutcome { kind: OutcomeKind::Transport, status: None },
        }
    }
}
```

- [ ] **Step 4: Update imports** — replace `crate::client::` with `crate::transport::` in `metrics.rs`, `engine.rs`; delete `client.rs`; swap `mod client;`→`mod transport;` in `main.rs`.
- [ ] **Step 5: Run tests** — `cargo test -p crebain transport::` → PASS; `cargo build -p crebain` → OK.
- [ ] **Step 6: Commit** (checkpoint) — `git commit -am "feat(crebain): transport module with reqwest source-IP fan-out pool"`

---

### Task 5: `schedule.rs` — pure rate math (`items_due`)

**Files:**
- Create: `backend/bins/crebain/src/schedule.rs`
- Modify: `main.rs` (`mod schedule;`)

**Interfaces:**
- Produces: `pub fn items_due(rate_per_sec: f64, elapsed_secs: f64, already_emitted: u64) -> u64` and `pub fn ramp_identifies_due(total: u64, ramp_secs: f64, elapsed_secs: f64, already: u64) -> u64`.

- [ ] **Step 1: Write failing tests**

```rust
use super::*;
#[test]
fn items_due_tracks_cumulative_rate() {
    assert_eq!(items_due(100.0, 0.05, 0), 5);   // 100/s × 50ms = 5
    assert_eq!(items_due(100.0, 0.05, 5), 0);   // already caught up
    assert_eq!(items_due(100.0, 1.0, 5), 95);   // 100 due − 5 sent
}
#[test]
fn ramp_spreads_identifies_linearly_then_completes() {
    assert_eq!(ramp_identifies_due(1000, 5.0, 0.0, 0), 0);
    assert_eq!(ramp_identifies_due(1000, 5.0, 2.5, 0), 500); // halfway
    assert_eq!(ramp_identifies_due(1000, 5.0, 10.0, 400), 600); // clamps to total
    assert_eq!(ramp_identifies_due(1000, 0.0, 0.0, 0), 1000);  // zero ramp = all now
}
```

- [ ] **Step 2: Run to confirm fail** — `cargo test -p crebain schedule::` → FAIL.

- [ ] **Step 3: Implement**

```rust
//! Pure open-model rate math: how many items are "due" by a given elapsed time.

/// Items due since start for a constant rate, minus those already emitted.
pub fn items_due(rate_per_sec: f64, elapsed_secs: f64, already_emitted: u64) -> u64 {
    if rate_per_sec <= 0.0 || elapsed_secs <= 0.0 {
        return 0;
    }
    let target = (rate_per_sec * elapsed_secs).floor() as u64;
    target.saturating_sub(already_emitted)
}

/// Identifies due under a linear ramp: `total × (elapsed/ramp)` clamped to `total`.
pub fn ramp_identifies_due(total: u64, ramp_secs: f64, elapsed_secs: f64, already: u64) -> u64 {
    let target = if ramp_secs <= 0.0 {
        total
    } else {
        ((total as f64) * (elapsed_secs / ramp_secs)).floor().min(total as f64) as u64
    };
    target.saturating_sub(already)
}
```

- [ ] **Step 4: Run tests** — `cargo test -p crebain schedule::` → PASS.
- [ ] **Step 5: Commit** (checkpoint) — `git commit -am "feat(crebain): pure open-model rate scheduler math"`

---

### Task 6: metrics — `behind`, peak-inflight, offered-vs-accepted; scheduled-time latency

**Files:**
- Modify: `backend/bins/crebain/src/metrics.rs`

**Interfaces:**
- Consumes: `Sample { outcome: SendOutcome, counts: ItemCounts, latency: Duration }` — latency now supplied by the engine (scheduled→completion), so add a `latency` field to `Sample` and drop it from `SendOutcome` (done in Task 4). Add `pub struct Metrics` counters: `behind: u64`, `peak_inflight: usize`, and (Task 12) `peak_connections`. Add to `Summary`: `pub behind: u64`, `pub peak_inflight: usize`, `pub offered: u64`.
- Produces: `pub fn record_behind(&mut self, n: u64)`, `pub fn set_inflight(&mut self, n: usize)` (tracks peak). The aggregator gains a second channel or a `Sample` variant for backpressure ticks — simplest: extend `Sample` to an enum `Event { Done(Sample), Behind(u64), Inflight(usize) }`. Keep it minimal: add `behind`/`offered` to the summary and a `Meta` message.

- [ ] **Step 1: Write failing test** (extend `metrics::tests`)

```rust
#[test]
fn records_behind_and_offered_and_peak_inflight() {
    let mut m = Metrics::new(4, Instant::now());
    m.record_behind(7);
    m.set_inflight(3);
    m.set_inflight(9);
    m.set_inflight(5);
    let s = m.finalize(vec![]);
    assert_eq!(s.behind, 7);
    assert_eq!(s.peak_inflight, 9);
}
```

- [ ] **Step 2: Run to confirm fail** — FAIL.
- [ ] **Step 3: Implement** — add fields `behind: u64`, `peak_inflight: usize` to `Metrics` + `Summary`; `record_behind` adds; `set_inflight` does `self.peak_inflight = self.peak_inflight.max(n)`; `finalize` copies them; `offered = requests + behind`. Update the `Sample` struct to carry `latency: Duration` and `record()` to push `s.latency` into the reservoir (replacing `s.outcome.latency`). Fix all existing `Summary { … }` constructions in tests to include the new fields.
- [ ] **Step 4: Run tests** — `cargo test -p crebain metrics::` → PASS.
- [ ] **Step 5: Commit** (checkpoint).

---

### Task 7: `engine.rs` rewrite — scheduler + bounded worker pool

**Files:**
- Rewrite: `backend/bins/crebain/src/engine.rs`

**Interfaces:**
- Consumes: `RunConfig` (Task 3), `ReqwestPool` (Task 4), `schedule::{items_due, ramp_identifies_due}` (Task 5), `metrics` (Task 6).
- Produces: `pub async fn run(cfg: &RunConfig, target: &Target, target_pid: Option<u32>, plan: &netlimit::FanoutPlan) -> anyhow::Result<Summary>` (signature gains `plan`).

**Design (implement, not verbatim — iterate against the compiler):**
1. Build the transport from `plan.source_ips` and `cfg`.
2. Derive rates: `events_rate = users × events_per_min / 60`, `issues_rate = users × issues_per_min / 60` (or `cfg.rps` split proportionally when set).
3. Channels: bounded `mpsc::channel::<WorkItem>(cfg.max_inflight * 2)` for work; unbounded `mpsc` for metrics `Sample`s (as today).
4. **Scheduler task:** loop every `SCHED_TICK = 5ms` until deadline; compute `ramp_identifies_due`, `items_due(events_rate,…)`, `items_due(issues_rate,…)` since start; for each due item build a `WorkItem { kind, identity_index, seq, scheduled: Instant::now() }` (rotating `identity_index` across `cfg.users`), and `try_send`; on `Full`, `metrics.record_behind(1)` (send a `Behind` meta) and drop. Update the running `emitted_*` counters by what was actually sent.
5. **Worker pool:** spawn exactly `cfg.max_inflight` worker tasks; each `while let Some(item) = rx.recv().await`: build the envelope via `generator::*` from `VirtualUser::new(item.identity_index)`, `encode`, `pool.send(item.identity_index, &body)`, then record `Sample { outcome, counts, latency: item.scheduled.elapsed() }`. The fixed worker count is the concurrency cap (no semaphore needed).
6. Deadline: `tokio::time::sleep_until(deadline)` stops the scheduler; dropping the work `tx` closes the channel so workers drain remaining items then exit; the metrics `tx`es drop → aggregator returns.
7. Report peak inflight: a shared `AtomicUsize` incremented around each `pool.send`, sampled each second into metrics (or send `Inflight` meta on a timer).

- [ ] **Step 1: Write a scoped integration test** — `run()` against the Task-4 mock server (bind a real mock ingest, tiny `users`/`duration`, `max_inflight=4`), assert `summary.accepted > 0`, `summary.requests > 0`, `summary.elapsed <= duration + slack`, and `summary.peak_inflight <= 4`.
- [ ] **Step 2: Run to confirm fail** — FAIL (signature/shape).
- [ ] **Step 3: Implement the rewrite** as designed above.
- [ ] **Step 4: Run tests** — `cargo test -p crebain engine::` → PASS; `cargo build -p crebain` → OK.
- [ ] **Step 5: Commit** (checkpoint) — `git commit -am "feat(crebain): open-model scheduler + bounded worker pool engine"`

---

### Task 8: `main.rs` + `report*` wiring — plan, banner, honest reporting; end-to-end M1

**Files:**
- Modify: `main.rs`, `report.rs`, `report_html.rs`

- [ ] **Step 1:** In `main.rs`, after resolving `cfg` and the `Target`: compute `let per = netlimit::ephemeral_port_budget();` `let loopback = netlimit::is_loopback_host(host_of(&target));` `let plan = netlimit::plan_fanout(cfg.max_inflight, loopback, per, MAX_IPS=512, cfg.source_ips);` then `let fd = netlimit::raise_nofile(plan.effective as u64 + 1024);` **before** `harness::provision` (so the isolated child inherits the raised limit). Pass `&plan` into `engine::run`.
- [ ] **Step 2:** Banner: print `concurrency` (requested `max_inflight` vs `plan.effective`), `source IPs` (`plan.source_ips.len()`), `fd limit` (`fd.soft`/`fd.hard`, warn if `fd.capped`), and `plan.warning` loudly if present.
- [ ] **Step 3:** `report.rs` `print_summary`: add a "concurrency" block — `offered req/s` vs `accepted req/s`, `behind` (shed), `peak in-flight`, and reiterate transport-error %. `report_html.rs`: add `ReportMeta` fields (`max_inflight`, `source_ips`, `effective`) and stat cards ("peak in-flight", "offered vs accepted req/s", "concurrency effective / requested").
- [ ] **Step 4: Manual end-to-end** (the M1 acceptance gate):

```bash
cargo build -p crebain -p sauron-ingest
# small correctness run:
./target/debug/crebain --isolated --database-url "$DATABASE_URL" \
  --users 50000 --duration 20 --events-per-min 60 --issues-per-min 10 \
  --max-inflight 4096 --report tmp/m1.html
```
Expected: accept rate ≫ 10% (near 100% until the server's own ceiling), transport errors ≈ 0, run ends at ~20s (not overrun), report shows peak-in-flight ≤ 4096 and offered-vs-accepted.

- [ ] **Step 5: Commit** (checkpoint) — `git commit -am "feat(crebain): plan/banner/report wiring; M1 portable 1M-identity engine"`

---

# MILESTONE 2 — Tier 3: literal 1M live sockets (localhost) + UDS

Adds the "1M sockets open at once" capacity reading: a raw HTTP/1.1 sender over TCP/UDS, a `--live-sockets` hold-open engine path, a UDS transport with an ingest `UnixListener`, server-side backlog + fd raising, and peak-connection sampling. Localhost-focused; reports peak sockets, never req/s.

### Task 9: `RawSender` — hand-rolled HTTP/1.1 over TCP or UDS

**Files:**
- Modify: `backend/bins/crebain/src/transport.rs`

**Interfaces:**
- Produces: `pub struct RawConn` wrapping a `tokio::io::{AsyncRead+AsyncWrite}` stream (enum over `TcpStream`/`UnixStream`), with `pub async fn connect_tcp(addr, local_ip: Option<Ipv4Addr>) -> io::Result<RawConn>`, `pub async fn connect_uds(path) -> io::Result<RawConn>`, and `pub async fn post(&mut self, path: &str, host: &str, key: &str, body: &[u8], gzip: bool) -> SendOutcome` (writes a keep-alive request, reads the status line, drains `content-length` bytes).

- [ ] **Step 1: Write failing test** — against the Task-4 mock server (extend it to speak `content-length`): open a `RawConn::connect_tcp`, `post` twice on the SAME conn (proving keep-alive reuse), assert both return `OutcomeKind::Accepted` / status 202.

```rust
#[tokio::test]
async fn raw_sender_posts_and_reuses_keepalive() {
    let (base, ..) = mock_ingest_cl().await; // returns "127.0.0.1:PORT"
    let mut conn = RawConn::connect_tcp(base.parse().unwrap(), None).await.unwrap();
    for _ in 0..2 {
        let o = conn.post("/api/app/envelope", "localhost", "k", b"{}", false).await;
        assert_eq!(o.status, Some(202));
    }
}
```

- [ ] **Step 2: Run to confirm fail** — FAIL.
- [ ] **Step 3: Implement** — a `RawConn` enum `{ Tcp(TcpStream), Uds(UnixStream) }`; `post()` formats `POST {path} HTTP/1.1\r\nHost: {host}\r\nx-sauron-key: {key}\r\ncontent-type: application/json\r\n[content-encoding: gzip\r\n]content-length: {n}\r\nconnection: keep-alive\r\n\r\n` + body; then read into a buffer until `\r\n\r\n`, parse the status code from the first line, read exactly the `content-length` response body so the socket is reusable. On any I/O error → `OutcomeKind::Transport`. For TCP with a `local_ip`, bind via `tokio::net::TcpSocket::new_v4()?; sock.bind((ip,0))?; sock.connect(addr)`.
- [ ] **Step 4: Run tests** — PASS.
- [ ] **Step 5: Commit** (checkpoint) — `git commit -am "feat(crebain): raw HTTP/1.1 sender over TCP/UDS"`

---

### Task 10: sauron-ingest — optional UDS listen + `TcpSocket` backlog + `setrlimit`

**Files:**
- Modify: `backend/bins/sauron-ingest/src/main.rs`, `backend/bins/sauron-ingest/Cargo.toml` (`+ libc`), `backend/crates/sauron-core/src/config.rs` (`+ ingest_uds_path: Option<String>` from `INGEST_UDS_PATH`, `+ ingest_backlog: u32` from `INGEST_BACKLOG` default `4096`).

- [ ] **Step 1: Write failing test** — in `sauron-core` config tests, assert `INGEST_UDS_PATH` and `INGEST_BACKLOG` parse into `Config` (set env, `Config::from_env()`, assert fields). For the listener wiring, a `#[tokio::test]` that binds the UDS branch to a temp path and hits `/health` over a `UnixStream` (guard `#[cfg(unix)]`).
- [ ] **Step 2: Run to confirm fail** — FAIL.
- [ ] **Step 3: Implement**
  - `config.rs`: add the two fields + parsing.
  - `main.rs`: `let _ = raise_nofile();` at startup (small libc helper or reuse a copy — a 10-line `setrlimit` to push soft→hard). Then branch: if `cfg.ingest_uds_path` is set, `let listener = tokio::net::UnixListener::bind(path)?; axum::serve(listener, app)`; else build the TCP listener via `TcpSocket::new_v4()? ; socket.set_reuseaddr(true)?; socket.bind(addr)?; let listener = socket.listen(cfg.ingest_backlog)?` and `axum::serve(listener, app)`. (axum 0.8 `serve` accepts both `TcpListener` and `UnixListener`.)
- [ ] **Step 4: Run tests + build** — `cargo test -p sauron-core`, `cargo build -p sauron-ingest` → OK.
- [ ] **Step 5: Commit** (checkpoint) — `git commit -am "feat(ingest): optional UDS listen, configurable backlog, raised fd limit"`

---

### Task 11: harness — spawn ingest with UDS + inherited rlimit

**Files:**
- Modify: `backend/bins/crebain/src/harness.rs`, `backend/bins/crebain/src/cli.rs` (`IsolatedConfig` gains `transport`, `uds_path`)

- [ ] **Step 1: Write failing test** — a `cli` test that `--transport uds --isolated` yields an `IsolatedConfig` with `transport == Transport::Uds` and an auto `uds_path` (e.g. `std::env::temp_dir()/crebain-<uuid>.sock`) when none is given.
- [ ] **Step 2: Run to confirm fail** — FAIL.
- [ ] **Step 3: Implement** — thread `transport`/`uds_path` into `IsolatedConfig`; in `spawn_ingest`, when UDS: `.env("INGEST_UDS_PATH", path)`; the target `base_url` becomes a sentinel (`http://localhost` + the UDS path carried alongside) and `wait_ready` polls `/ready` over a `RawConn::connect_uds`. For TCP isolated mode, unchanged. `main.rs` builds the transport accordingly (`ReqwestPool` for TCP, a UDS `RawSender` engine path for UDS).
- [ ] **Step 4: Run tests + build** — OK.
- [ ] **Step 5: Commit** (checkpoint).

---

### Task 12: procstat + metrics — peak-connection (fd count) sampling

**Files:**
- Modify: `backend/bins/crebain/src/procstat.rs`, `metrics.rs`, `report*.rs`

**Interfaces:**
- Produces: `pub fn count_fds(pid: u32) -> Option<usize>` (count entries in `/proc/<pid>/fd`); `RawSample` gains `open_fds: Option<usize>`; `Summary` gains `peak_connections: usize` (max open_fds over the run).

- [ ] **Step 1: Write failing test** — `count_fds(std::process::id())` returns `Some(n)` with `n >= 3` on Linux; `Summary` carries a `peak_connections` propagated from timeline fd samples (unit test the max-tracking like the existing CPU test).
- [ ] **Step 2: Run to confirm fail** — FAIL.
- [ ] **Step 3: Implement** — `count_fds` via `std::fs::read_dir(format!("/proc/{pid}/fd")).map(|it| it.count())`. Sample it in `Sampler::sample()` (add to `RawSample`), track peak in the aggregator, add to `Summary` and a "peak connections" stat card. Label it as the server's held-socket proxy.
- [ ] **Step 4: Run tests** — PASS.
- [ ] **Step 5: Commit** (checkpoint).

---

### Task 13: `--live-sockets` engine path + UDS transport wiring

**Files:**
- Modify: `backend/bins/crebain/src/engine.rs`, `transport.rs`, `report*.rs`

- [ ] **Step 1: Write failing test** — a `#[tokio::test]` `run_live_sockets()` against the mock server: with `--live-sockets`, `max_inflight = 200`, assert the engine opens and HOLDS ~200 concurrent connections for the duration (mock server's peak-concurrent reaches ~200 and stays), and the summary reports `peak_connections ≈ 200` and labels the run as a capacity demo (accepted req/s ≈ small trickle, not the headline).
- [ ] **Step 2: Run to confirm fail** — FAIL.
- [ ] **Step 3: Implement** — when `cfg.live_sockets`: instead of the request-rate loop, spawn `plan.effective` (bounded by users) tasks that each open ONE `RawConn` (TCP with round-robin source IP, or UDS) and HOLD it, sending a slow keep-alive trickle (e.g. one request per `--ramp`-derived interval) until the deadline; count peak held. For `--transport uds`, route both the normal and live-sockets paths through `RawConn::connect_uds`. Report peak sockets as the headline for this mode.
- [ ] **Step 4: Run tests** — PASS.
- [ ] **Step 5: Commit** (checkpoint) — `git commit -am "feat(crebain): --live-sockets hold-open path + UDS transport"`

---

### Task 14: README + honesty docs

**Files:**
- Modify: `backend/bins/crebain/README.md`

- [ ] **Step 1:** Document `--max-inflight`, `--ramp`, `--rps`, `--source-ips`, `--transport {tcp|uds}`, `--uds-path`, `--live-sockets`. Explain the ~28,232-per-tuple wall, the two readings of "1M concurrent", the fd/`tcp_mem`/`somaxconn` ceilings and that the *hard* NOFILE cap needs root, that source-IP fan-out is loopback-only, and that the stated 100+10/min config is ~1.83M req/s and is a distributed workload. Show example commands for each tier.
- [ ] **Step 2: Commit** (checkpoint).

---

### Task 15: Full end-to-end acceptance (both readings)

- [ ] **Step 1: Tier-1 portable run** (as Task 8 Step 4) — accept rate high, transport errors ≈ 0, honored duration.
- [ ] **Step 2: Tier-3 live-sockets run** (raise the OS limits first; document them):

```bash
# needs root for a literal >500k: sudo prlimit / limits.conf; sysctl net.core.somaxconn, net.ipv4.tcp_mem
./target/release/crebain --isolated --database-url "$DATABASE_URL" \
  --users 200000 --duration 30 --live-sockets --max-inflight 200000 \
  --source-ips 16 --report tmp/live.html
```
Expected: report shows PEAK CONNECTIONS approaching the requested count (bounded by fd hard limit — surfaced honestly), transport-error % explains any shortfall, and the run is labeled a capacity demo, not a req/s result.

- [ ] **Step 3: UDS run:**

```bash
./target/release/crebain --isolated --database-url "$DATABASE_URL" \
  --users 200000 --duration 30 --transport uds --live-sockets --max-inflight 200000 \
  --report tmp/uds.html
```
Expected: no ephemeral-port wall at all (UDS); peak connections bounded only by fds/memory; report notes UDS transport.

- [ ] **Step 4:** `cargo clippy -p crebain -p sauron-ingest --all-targets` clean; `cargo test -p crebain` green.
- [ ] **Step 5: Commit** (checkpoint) — `git commit -am "test(crebain): 1M-concurrency acceptance (portable + live-sockets + UDS)"`

---

## Self-Review

**Spec coverage:** Tier 1 (bounded pool + fan-out + fd raise + CO latency + honest reporting) → Tasks 1–8. Tier 3 (raw sender, UDS transport, ingest UDS/backlog/rlimit, live-sockets, peak-conn sampling) → Tasks 9–13. Docs → 14. Acceptance → 8, 15. The rejected `SO_REUSEPORT` is intentionally absent. HTTP/2 tier is intentionally out of scope (user chose Tier 1 + Tier 3-full, not Tier 2).

**Placeholder scan:** async-heavy tasks (7, 13) give design + signatures + test shape rather than verbatim final code — flagged inline as "implement, iterate against the compiler." All pure/mechanical tasks (1, 2, 3, 5, 9-core, 12) carry complete code.

**Type consistency:** `SendOutcome` loses `latency` in Task 4; `Sample` gains `latency: Duration` in Task 6; `engine` supplies it via `item.scheduled.elapsed()` in Task 7 — consistent. `plan_fanout`/`FanoutPlan`/`raise_nofile`/`NofileStatus` names match across Tasks 1, 2, 8. `RawConn`/`ReqwestPool` both live in `transport.rs` and are consumed by `engine.rs`.

**Open risk to confirm during execution:** axum 0.8 `serve(UnixListener, …)` — verify the `Listener` impl exists (Task 10 Step 3); if not, fall back to a small hyper-util accept loop for the UDS branch only.
