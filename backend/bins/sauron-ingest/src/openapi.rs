//! The OpenAPI 3.1 document for `sauron-ingest`.
//!
//! Separate from `sauron-api`'s document on purpose: this is a different binary
//! on a different port with a different credential, and the deployment
//! constraint that this gateway must answer at the host root does not apply to
//! the dashboard API. One merged document could not state a truthful `servers[]`
//! for both.
//!
//! Only the document is served here — no Swagger UI assets. `sauron-api`'s
//! `/docs` page lists this document in its selector, which keeps several
//! megabytes of embedded static files out of a binary that sits on the
//! write hot path.

use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
use utoipa::{Modify, OpenApi};

pub struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi
            .components
            .get_or_insert_with(utoipa::openapi::Components::default);
        components.add_security_scheme(
            "sauronKey",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "X-Sauron-Key",
                "The environment's public ingest key — the user part of a DSN. \
                 Write-only and non-secret by design: it can identify one \
                 environment and accept telemetry for it, and can read nothing. \
                 Safe to embed in client code.",
            ))),
        );
    }
}

/// Acknowledgement returned when an envelope is accepted.
///
/// **Acceptance is not durability.** The gateway answers as soon as the
/// envelope is queued; the worker that writes it to Postgres drains far more
/// slowly than the edge accepts. Under sustained overload the queue is trimmed
/// and accepted events can still be lost — check `/metrics` rather than
/// inferring delivery from a 200 here.
#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct IngestAccepted {
    #[schema(example = true)]
    pub ok: bool,
}

#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct IngestError {
    #[schema(example = "invalid_key")]
    pub error: String,
}

#[utoipa::path(
    post,
    path = "/api/{environment_id}/envelope",
    tag = "Ingest",
    summary = "Submit a telemetry envelope",
    description = "\
The single endpoint every Sauron SDK posts to. One envelope carries a header, a \
shared context, and a batch of items (errors, events, identifies, transactions).

**Authentication is the key, not the path.** The gateway authenticates on \
`X-Sauron-Key` alone; the `{environment_id}` segment is informational and is not \
read. A `sendBeacon` fallback may pass the key as `?k=` instead, because \
`sendBeacon` cannot set headers.

**Deployment constraint:** this path must be reachable at the host root. A proxy \
that mounts the gateway under a prefix causes SDKs to post to a path that does \
not exist, and events are dropped with no error visible to the application.

Bodies may be gzipped (`Content-Encoding: gzip`) and are capped by \
`INGEST_MAX_BODY_BYTES`.",
    security(("sauronKey" = [])),
    params(
        ("environment_id" = String, Path,
         description = "The environment enrollment id from the DSN. Informational — the key is what authenticates."),
        ("X-Sauron-Key" = Option<String>, Header,
         description = "The environment's public key. Required unless supplied as `?k=`."),
        ("k" = Option<String>, Query,
         description = "The public key, for `sendBeacon` clients that cannot set headers."),
        ("Content-Encoding" = Option<String>, Header, description = "`gzip` if the body is compressed."),
    ),
    request_body(
        content = sauron_core::envelope::Envelope,
        description = "A telemetry envelope.",
        content_type = "application/json",
        // The published example IS the golden fixture the SDK parity tests
        // guard — see `sauron_core::envelope::GOLDEN_ENVELOPE`. Parsed rather
        // than pasted so the two cannot drift.
        example = json!(serde_json::from_str::<serde_json::Value>(
            sauron_core::envelope::GOLDEN_ENVELOPE
        ).expect("the golden envelope must be valid JSON")),
    ),
    responses(
        (status = 200, description = "Envelope accepted and queued. Not a durability guarantee — see `IngestAccepted`.", body = IngestAccepted),
        (status = 400, description = "Malformed JSON, or an envelope that does not match the wire contract.", body = IngestError),
        (status = 401, description = "Missing, unknown, or disabled ingest key.", body = IngestError),
        (status = 413, description = "Body exceeds `INGEST_MAX_BODY_BYTES`.", body = IngestError),
        (status = 429, description = "Per-key rate limit exhausted (`INGEST_RATE_LIMIT_PER_MIN`).", body = IngestError),
        (status = 503, description = "The queue is unavailable. Retry with backoff; SDKs buffer and retry automatically.", body = IngestError),
    ),
)]
#[allow(dead_code)]
fn ingest_doc() {}

#[utoipa::path(
    get, path = "/health", tag = "Ingest",
    summary = "Liveness probe",
    description = "Always 200 while the process is serving. Says nothing about whether Redis or Postgres are reachable — use `/ready` for that.",
    security(),
    responses((status = 200, description = "The gateway is serving.", body = String)),
)]
#[allow(dead_code)]
fn health_doc() {}

#[utoipa::path(
    get, path = "/ready", tag = "Ingest",
    summary = "Readiness probe",
    description = "Reports whether the gateway can currently accept and queue telemetry. Unlike `/health`, this does answer non-2xx when its dependencies are unreachable, so it is the correct probe for a load balancer.",
    security(),
    responses(
        (status = 200, description = "Ready to accept envelopes.", body = String),
        (status = 503, description = "A dependency is unreachable; do not route traffic here.", body = String),
    ),
)]
#[allow(dead_code)]
fn ready_doc() {}

#[utoipa::path(
    get, path = "/metrics", tag = "Ingest",
    summary = "Prometheus metrics",
    description = "\
Text-format metrics for the write path: accepted, queued, drained and dropped \
counts.

This is where silent loss becomes visible. Because acceptance is not \
durability, the gap between accepted and drained — and any trim counter — is \
the only signal that events are being discarded under load.",
    security(),
    responses((status = 200, description = "Prometheus text exposition.", content_type = "text/plain", body = String)),
)]
#[allow(dead_code)]
fn metrics_doc() {}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Sauron Ingest",
        description = "\
The telemetry gateway every Sauron SDK posts to.

Authenticated by a **write-only public key** rather than a JWT — the key can \
accept telemetry for exactly one environment and can read nothing, so it is safe \
to embed in client code. The dashboard and administration API is a separate \
service with its own document.",
        version = env!("CARGO_PKG_VERSION"),
        license(name = "LGPL-3.0-only"),
    ),
    modifiers(&SecurityAddon),
    tags((name = "Ingest", description = "The telemetry write path, plus its liveness and readiness probes.")),
    paths(ingest_doc, health_doc, ready_doc, metrics_doc),
    components(schemas(IngestAccepted, IngestError)),
)]
pub struct IngestDoc;

#[cfg(test)]
mod tests {
    use super::*;

    /// The published example must be the **same bytes** the parity tests guard,
    /// not a copy that is free to drift from the contract.
    #[test]
    fn the_documented_example_is_the_golden_fixture() {
        let parsed: sauron_core::envelope::Envelope =
            serde_json::from_str(sauron_core::envelope::GOLDEN_ENVELOPE)
                .expect("the documented example must satisfy the wire contract it documents");
        assert!(
            !parsed.items.is_empty(),
            "the golden envelope should carry items; an empty one would document nothing"
        );
    }

    #[test]
    fn the_key_scheme_is_defined_and_is_a_header_api_key() {
        let doc = serde_json::to_value(IngestDoc::openapi()).expect("serializes");
        let scheme = doc
            .pointer("/components/securitySchemes/sauronKey")
            .expect("sauronKey must be defined; SecurityAddon did not run");
        assert_eq!(scheme["type"], "apiKey");
        assert_eq!(scheme["in"], "header");
        assert_eq!(scheme["name"], "X-Sauron-Key");
    }

    /// The four routes the gateway actually serves, and no others.
    #[test]
    fn the_document_describes_exactly_the_gateway_routes() {
        let doc = serde_json::to_value(IngestDoc::openapi()).expect("serializes");
        let mut paths: Vec<_> = doc["paths"].as_object().unwrap().keys().cloned().collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "/api/{environment_id}/envelope".to_string(),
                "/health".to_string(),
                "/metrics".to_string(),
                "/ready".to_string(),
            ]
        );
    }
}
