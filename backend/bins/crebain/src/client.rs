//! The ingest HTTP sender. One shared, cloneable [`IngestClient`] (reqwest pools
//! connections internally) is handed to every virtual user. `send` never returns
//! an error — every failure is classified into a [`SendOutcome`] so the run
//! continues and the failure is measured, not fatal.

use std::io::Write;
use std::time::{Duration, Instant};

use flate2::write::GzEncoder;
use flate2::Compression;
use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE};

use sauron_core::envelope::Envelope;

use crate::dsn::Target;

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
    pub latency: Duration,
}

#[derive(Clone)]
pub struct IngestClient {
    http: reqwest::Client,
    url: String,
    key: String,
    gzip: bool,
}

impl IngestClient {
    pub fn new(target: &Target, gzip: bool) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(usize::MAX)
            .build()?;
        Ok(IngestClient {
            http,
            url: target.envelope_url(),
            key: target.public_key.clone(),
            gzip,
        })
    }

    /// Serialize, (optionally) gzip, POST. Classifies every result; never errors.
    pub async fn send(&self, env: &Envelope) -> SendOutcome {
        let started = Instant::now();
        let body = match self.encode(env) {
            Ok(b) => b,
            // A serialization/compression failure is a bug in our own payload;
            // surface it as a transport failure with zero latency.
            Err(_) => {
                return SendOutcome {
                    kind: OutcomeKind::Transport,
                    status: None,
                    latency: Duration::ZERO,
                }
            }
        };

        let mut req = self
            .http
            .post(&self.url)
            .header("x-sauron-key", &self.key)
            .header(CONTENT_TYPE, "application/json");
        if self.gzip {
            req = req.header(CONTENT_ENCODING, "gzip");
        }

        match req.body(body).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                // Fully read the body so the connection can be reused.
                let _ = resp.bytes().await;
                let kind = if (200..300).contains(&status) {
                    OutcomeKind::Accepted
                } else if status == 429 {
                    OutcomeKind::RateLimited
                } else {
                    OutcomeKind::HttpError
                };
                SendOutcome {
                    kind,
                    status: Some(status),
                    latency: started.elapsed(),
                }
            }
            Err(_) => SendOutcome {
                kind: OutcomeKind::Transport,
                status: None,
                latency: started.elapsed(),
            },
        }
    }

    fn encode(&self, env: &Envelope) -> anyhow::Result<Vec<u8>> {
        let json = serde_json::to_vec(env)?;
        if !self.gzip {
            return Ok(json);
        }
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(&json)?;
        Ok(enc.finish()?)
    }
}
