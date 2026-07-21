//! The ingest HTTP sender. A [`ReqwestPool`] holds one `reqwest::Client` per
//! loopback source IP (reqwest pools connections internally) and is shared across
//! every in-flight send. `send` never returns an error — every failure is
//! classified into a [`SendOutcome`] so the run continues and the failure is
//! measured, not fatal. Payloads are serialized once via the free [`encode`]
//! function before the bytes are handed to `send`.

use std::io::Write;
use std::time::Duration;

use flate2::write::GzEncoder;
use flate2::Compression;
use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use sauron_core::envelope::Envelope;

/// How a single request resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeKind {
    /// 2xx — the edge queued the envelope.
    Accepted,
    /// 429 — the per-app rate limit rejected it.
    RateLimited,
    /// Any other HTTP status.
    HttpError,
    /// Never got a response (connection refused, timeout, …).
    Transport,
}

#[derive(Debug, Clone)]
pub struct SendOutcome {
    pub kind: OutcomeKind,
    pub status: Option<u16>,
}

/// Classify an HTTP status code into an [`OutcomeKind`]. Shared by
/// [`ReqwestPool::send`] and [`post_on`] so the 2xx/429/else mapping lives in
/// exactly one place.
fn classify(status: u16) -> OutcomeKind {
    if (200..300).contains(&status) {
        OutcomeKind::Accepted
    } else if status == 429 {
        OutcomeKind::RateLimited
    } else {
        OutcomeKind::HttpError
    }
}

/// Serialize an envelope to JSON and, when `gzip`, compress it with a fast gzip
/// pass. The engine encodes once per request before handing the bytes to
/// [`ReqwestPool::send`]; latency is measured around the send by the engine, so
/// encoding is intentionally outside the timed path.
pub fn encode(env: &Envelope, gzip: bool) -> anyhow::Result<Vec<u8>> {
    let json = serde_json::to_vec(env)?;
    if !gzip {
        return Ok(json);
    }
    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(&json)?;
    Ok(enc.finish()?)
}

/// A pool of `reqwest::Client`s, one per loopback source IP, so a single-box
/// benchmark can fan requests out across many `127.0.0.0/8` addresses instead
/// of exhausting the ephemeral-port range of one source IP.
///
/// [`send`](ReqwestPool::send) picks a client with `slot % clients.len()`,
/// round-robining virtual users across the pool without the caller knowing
/// which source IP backs a given slot.
pub struct ReqwestPool {
    clients: Vec<reqwest::Client>,
    url: String,
    key: String,
    gzip: bool,
}

impl ReqwestPool {
    /// Builds one client per entry in `source_ips`, each bound to that address
    /// via `local_address`. An empty `source_ips` builds a single client with
    /// no `local_address` override (default routing).
    pub fn new(
        base_url: &str,
        app_id: &str,
        key: &str,
        gzip: bool,
        source_ips: &[std::net::Ipv4Addr],
    ) -> anyhow::Result<Self> {
        let build = |local: Option<std::net::IpAddr>| -> anyhow::Result<reqwest::Client> {
            let mut b = reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .pool_max_idle_per_host(usize::MAX)
                // A benchmark measures the edge directly; never route through an
                // ambient HTTP(S)_PROXY / ALL_PROXY that would skew or break requests.
                .no_proxy();
            if let Some(ip) = local {
                b = b.local_address(ip);
            }
            Ok(b.build()?)
        };
        let clients = if source_ips.is_empty() {
            vec![build(None)?]
        } else {
            source_ips
                .iter()
                .map(|ip| build(Some(std::net::IpAddr::V4(*ip))))
                .collect::<anyhow::Result<_>>()?
        };
        Ok(Self {
            clients,
            url: format!("{base_url}/api/{app_id}/envelope"),
            key: key.to_string(),
            gzip,
        })
    }

    /// POST `body` (already encoded/compressed by the caller) using the client
    /// selected by `slot % conns()`. Never errors; every failure is classified
    /// into a [`SendOutcome`].
    pub async fn send(&self, slot: usize, body: &[u8]) -> SendOutcome {
        let client = &self.clients[slot % self.clients.len()];
        let mut req = client
            .post(&self.url)
            .header("x-sauron-key", &self.key)
            .header(CONTENT_TYPE, "application/json");
        if self.gzip {
            req = req.header(CONTENT_ENCODING, "gzip");
        }

        match req.body(body.to_vec()).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                // Fully read the body so the connection can be reused.
                let _ = resp.bytes().await;
                SendOutcome {
                    kind: classify(status),
                    status: Some(status),
                }
            }
            Err(_) => SendOutcome {
                kind: OutcomeKind::Transport,
                status: None,
            },
        }
    }
}

/// A connection opened directly over TCP or a Unix-domain socket, bypassing
/// reqwest's internal connection pool. [`ReqwestPool`] is right for fanning
/// requests out across many source IPs, but it coalesces/reuses connections
/// transparently and cannot guarantee one live socket held per virtual user,
/// and it has no Unix-domain-socket connector at all. `RawConn` fills both
/// gaps with a minimal hand-rolled HTTP/1.1 client.
pub enum RawConn {
    Tcp(tokio::net::TcpStream),
    Uds(tokio::net::UnixStream),
}

impl RawConn {
    /// Opens a raw TCP connection to `addr`, optionally bound to `local_ip` as
    /// the source address — the same per-source-IP fan-out [`ReqwestPool`]
    /// does, but for a single connection the caller holds directly instead of
    /// a pooled client.
    pub async fn connect_tcp(
        addr: std::net::SocketAddr,
        local_ip: Option<std::net::Ipv4Addr>,
    ) -> std::io::Result<RawConn> {
        if let Some(ip) = local_ip {
            let sock = tokio::net::TcpSocket::new_v4()?;
            sock.bind(std::net::SocketAddr::new(std::net::IpAddr::V4(ip), 0))?;
            let stream = sock.connect(addr).await?;
            Ok(RawConn::Tcp(stream))
        } else {
            Ok(RawConn::Tcp(tokio::net::TcpStream::connect(addr).await?))
        }
    }

    /// Opens a raw Unix-domain socket connection to `path`. reqwest has no
    /// UDS connector, so this is the only way this crate can benchmark a
    /// UDS-fronted ingest listener.
    pub async fn connect_uds<P: AsRef<std::path::Path>>(path: P) -> std::io::Result<RawConn> {
        Ok(RawConn::Uds(tokio::net::UnixStream::connect(path).await?))
    }

    /// Sends one keep-alive HTTP/1.1 POST and fully drains the response
    /// (status line + exactly `content-length` body bytes), so the same
    /// `RawConn` can be `post`ed on again afterwards. Never errors; a write,
    /// read, or parse failure all collapse to
    /// `SendOutcome { kind: Transport, status: None }`, matching
    /// [`ReqwestPool::send`]'s never-fails contract.
    pub async fn post(
        &mut self,
        path: &str,
        host: &str,
        key: &str,
        body: &[u8],
        gzip: bool,
    ) -> SendOutcome {
        match self {
            RawConn::Tcp(s) => post_on(s, path, host, key, body, gzip).await,
            RawConn::Uds(s) => post_on(s, path, host, key, body, gzip).await,
        }
    }
}

/// The HTTP/1.1 request/response logic, generic over any
/// `AsyncRead + AsyncWrite` stream so it is written exactly once and shared
/// by both the TCP and UDS variants of [`RawConn`].
async fn post_on<S>(
    stream: &mut S,
    path: &str,
    host: &str,
    key: &str,
    body: &[u8],
    gzip: bool,
) -> SendOutcome
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // Bound the whole write + response-read/parse in a wall-clock timeout,
    // matching reqwest's 30s. Without it a wedged peer that accepts the request
    // but never replies (and never closes) would block the read loop forever —
    // and in `--live-sockets` that hang would stall the whole run's holder drain.
    // The timeout only fires on such a silent peer; a prompt response returns
    // exactly as before.
    let inner = async {
        let encoding_header = if gzip {
            "content-encoding: gzip\r\n"
        } else {
            ""
        };
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\nx-sauron-key: {key}\r\ncontent-type: application/json\r\n{encoding_header}content-length: {content_length}\r\nconnection: keep-alive\r\n\r\n",
            content_length = body.len(),
        );

        if stream.write_all(head.as_bytes()).await.is_err() || stream.write_all(body).await.is_err()
        {
            return SendOutcome {
                kind: OutcomeKind::Transport,
                status: None,
            };
        }

        // Read into a growing buffer until the header terminator shows up.
        let mut buf = Vec::with_capacity(256);
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
            let mut chunk = [0u8; 1024];
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => {
                    return SendOutcome {
                        kind: OutcomeKind::Transport,
                        status: None,
                    }
                }
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };

        let head_str = match std::str::from_utf8(&buf[..header_end]) {
            Ok(s) => s,
            Err(_) => {
                return SendOutcome {
                    kind: OutcomeKind::Transport,
                    status: None,
                }
            }
        };
        let mut lines = head_str.split("\r\n");
        let status: u16 = match lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
        {
            Some(s) => s,
            None => {
                return SendOutcome {
                    kind: OutcomeKind::Transport,
                    status: None,
                }
            }
        };
        let kind = classify(status);

        let mut content_length: Option<usize> = None;
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().ok();
                }
            }
        }

        // Our ingest always answers with a content-length. If some other server
        // didn't, we have no reliable way to know how many body bytes to drain
        // for keep-alive reuse, so just report the parsed status without
        // attempting to read a body.
        let Some(content_length) = content_length else {
            return SendOutcome {
                kind,
                status: Some(status),
            };
        };

        // Some (or all, or none) of the body may already be sitting in `buf`
        // alongside the header terminator; only read the remainder.
        let mut have = buf.len() - header_end;
        while have < content_length {
            let mut chunk = [0u8; 1024];
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => {
                    return SendOutcome {
                        kind: OutcomeKind::Transport,
                        status: None,
                    }
                }
                Ok(n) => have += n,
            }
        }

        SendOutcome {
            kind,
            status: Some(status),
        }
    };

    match tokio::time::timeout(std::time::Duration::from_secs(30), inner).await {
        Ok(outcome) => outcome,
        Err(_) => SendOutcome {
            kind: OutcomeKind::Transport,
            status: None,
        },
    }
}

/// The single transport the engine sends every request through: either a
/// pooled [`ReqwestPool`] fanned out across loopback source IPs (TCP), or a
/// fresh [`RawConn`] per request over a Unix-domain socket. UDS has no
/// ephemeral-port wall to fan out across, so there is no pool — connect+post
/// is cheap enough to pay on every request.
pub enum Sender {
    Reqwest(ReqwestPool),
    Uds {
        path: std::path::PathBuf,
        app_id: String,
        key: String,
        gzip: bool,
    },
}

impl Sender {
    /// Send one request through whichever transport this `Sender` wraps.
    /// Never errors: a UDS connect failure collapses to the same
    /// `OutcomeKind::Transport` outcome any other transport failure does.
    pub async fn send(&self, slot: usize, body: &[u8]) -> SendOutcome {
        match self {
            Sender::Reqwest(p) => p.send(slot, body).await,
            Sender::Uds {
                path,
                app_id,
                key,
                gzip,
            } => match RawConn::connect_uds(path).await {
                Ok(mut c) => {
                    c.post(
                        &format!("/api/{app_id}/envelope"),
                        "localhost",
                        key,
                        body,
                        *gzip,
                    )
                    .await
                }
                Err(_) => SendOutcome {
                    kind: OutcomeKind::Transport,
                    status: None,
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// A raw-TCP mock ingest server that records peak concurrent connections
    /// and the set of distinct client source IPs it was contacted from.
    async fn mock_ingest() -> (
        String,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<Mutex<BTreeSet<std::net::IpAddr>>>,
    ) {
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
                    let _ = sock
                        .write_all(b"HTTP/1.1 202 Accepted\r\ncontent-length: 2\r\n\r\nok")
                        .await;
                    c.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        (format!("http://{addr}"), cur, peak, ips)
    }

    /// A raw-TCP mock ingest server whose per-connection handler loops,
    /// answering every request on the connection with `202 Accepted` +
    /// `content-length: 2` (body `ok`), so a keep-alive client can post
    /// repeatedly on the same socket. Returns the "ip:port" address.
    async fn mock_ingest_cl() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut sock, _peer) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = Vec::new();
                    loop {
                        // Read until we've seen the header terminator for one request.
                        let mut chunk = [0u8; 1024];
                        loop {
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                            match sock.read(&mut chunk).await {
                                Ok(0) => return, // client closed
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                                Err(_) => return,
                            }
                        }
                        // Drop the consumed request head (and any body bytes we
                        // don't bother parsing for this mock — the request body
                        // is tiny `{}`  sent in the same head write in tests).
                        buf.clear();
                        if sock
                            .write_all(
                                b"HTTP/1.1 202 Accepted\r\ncontent-length: 2\r\nconnection: keep-alive\r\n\r\nok",
                            )
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        format!("127.0.0.1:{}", addr.port())
    }

    #[tokio::test]
    async fn raw_sender_posts_and_reuses_keepalive() {
        let base = mock_ingest_cl().await;
        let mut conn = RawConn::connect_tcp(base.parse().unwrap(), None)
            .await
            .unwrap();
        for _ in 0..2 {
            let o = conn
                .post("/api/app/envelope", "localhost", "k", b"{}", false)
                .await;
            assert_eq!(o.status, Some(202));
            assert_eq!(o.kind, OutcomeKind::Accepted);
        }
    }

    #[tokio::test]
    async fn pool_bounds_concurrency_and_fans_out_source_ips() {
        let (base, _cur, peak, ips) = mock_ingest().await;
        // NOTE: engine enforces the cap via a fixed worker count; here we drive
        // `cap` workers over the pool directly to prove the observable effect.
        let cap = 5;
        let src = vec![
            std::net::Ipv4Addr::new(127, 0, 0, 1),
            std::net::Ipv4Addr::new(127, 0, 0, 2),
            std::net::Ipv4Addr::new(127, 0, 0, 3),
        ];
        let pool = ReqwestPool::new(&base, "app", "k", false, &src).unwrap();
        let body = b"{}".to_vec();
        // Fire 50 requests through exactly `cap` concurrent workers round-robining slots.
        let pool = Arc::new(pool);
        let mut handles = vec![];
        let counter = Arc::new(AtomicUsize::new(0));
        for _ in 0..cap {
            let (pool, body, counter) = (pool.clone(), body.clone(), counter.clone());
            handles.push(tokio::spawn(async move {
                loop {
                    let i = counter.fetch_add(1, Ordering::SeqCst);
                    if i >= 50 {
                        break;
                    }
                    let _ = pool.send(i, &body).await;
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert!(
            peak.load(Ordering::SeqCst) <= cap,
            "peak {} exceeded cap {cap}",
            peak.load(Ordering::SeqCst)
        );
        let seen = ips.lock().unwrap().clone();
        assert!(
            seen.len() >= 2,
            "expected multiple source IPs, saw {seen:?}"
        );
    }
}
