//! The DSN wire format shared with the SDKs, and the resolved [`Target`] both
//! run modes converge on.
//!
//! DSN: `scheme://<public_key>@host:port/<project_id>` (identical to what a
//! real SDK is configured with). The ingest edge authenticates by the key in the
//! `X-Sauron-Key` header and does not READ this path segment — its handler binds
//! it to `_project_id` and resolves the app from the key instead.
//!
//! It must still be a valid UUID. The route is `/api/{project_id}/envelope` with
//! a `Path<Uuid>` extractor, so axum rejects a non-UUID segment with 400 before
//! the handler runs. "Ignored" and "may be anything" are NOT the same thing, and
//! an earlier version of this comment said only the first — which cost a full
//! run of 100% 400s whose cause was invisible, because `send` discarded the
//! response body. Both halves are fixed; keep them together.

/// Everything the load engine needs to talk to an ingest edge, however it was
/// obtained (parsed from `--dsn`, or minted by the isolated harness).
#[derive(Debug, Clone)]
pub struct Target {
    /// `scheme://host:port`, no trailing slash.
    pub base_url: String,
    pub app_id: String,
    pub public_key: String,
}

impl Target {
    /// The ingest endpoint an envelope is POSTed to.
    pub fn envelope_url(&self) -> String {
        format!("{}/api/{}/envelope", self.base_url, self.app_id)
    }

    /// The DSN string, for display / logging.
    pub fn dsn(&self) -> String {
        // reinsert the key into the authority: scheme://key@host:port/app_id
        match self.base_url.split_once("://") {
            Some((scheme, authority)) => {
                format!("{scheme}://{}@{authority}/{}", self.public_key, self.app_id)
            }
            None => self.base_url.clone(),
        }
    }
}

/// Parse an SDK-format DSN into a [`Target`].
pub fn parse_dsn(dsn: &str) -> anyhow::Result<Target> {
    let (scheme, rest) = dsn
        .split_once("://")
        .filter(|(s, _)| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("DSN must start with scheme:// — got {dsn:?}"))?;

    let (public_key, host_and_path) = rest
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("DSN must contain <public_key>@host — got {dsn:?}"))?;
    if public_key.is_empty() {
        anyhow::bail!("DSN public key is empty: {dsn:?}");
    }

    let (host_port, app_id) = host_and_path
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("DSN must contain a /<app_id> path — got {dsn:?}"))?;
    if host_port.is_empty() {
        anyhow::bail!("DSN host is empty: {dsn:?}");
    }
    // trim any trailing slash / query from the app id
    let app_id = app_id.trim_end_matches('/');
    let app_id = app_id.split(['?', '#']).next().unwrap_or(app_id);
    if app_id.is_empty() {
        anyhow::bail!("DSN project id is empty: {dsn:?}");
    }
    // Checked HERE rather than left to the server, because the server's refusal
    // is a bare 400 on every request for the whole run and crebain reports it
    // only as a status-code tally. One line before any traffic is sent beats a
    // 60-second run that transmits nothing and cannot say why.
    if uuid::Uuid::parse_str(app_id).is_err() {
        anyhow::bail!(
            "DSN path segment must be the project id as a UUID — got {app_id:?}. \
             The ingest route is /api/{{project_id}}/envelope with a Path<Uuid> \
             extractor: it never reads the value, but a non-UUID is rejected \
             with 400 before the handler runs."
        );
    }

    Ok(Target {
        base_url: format!("{scheme}://{host_port}"),
        app_id: app_id.to_string(),
        public_key: public_key.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_dsn() {
        let t =
            parse_dsn("http://pk_abc@localhost:8081/11111111-2222-3333-4444-555555555555").unwrap();
        assert_eq!(t.base_url, "http://localhost:8081");
        assert_eq!(t.public_key, "pk_abc");
        assert_eq!(t.app_id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(
            t.envelope_url(),
            "http://localhost:8081/api/11111111-2222-3333-4444-555555555555/envelope"
        );
    }

    #[test]
    fn rejects_a_path_segment_that_is_not_a_uuid() {
        // The ingest route is `/api/{project_id}/envelope` with a `Path<Uuid>`
        // extractor. It binds the value to `_project_id` and never reads it —
        // the app is resolved from the key — but axum still REJECTS the request
        // with 400 before the handler runs if the segment is not a UUID.
        //
        // Caught this the expensive way: a DSN ending `/detail` produced 400 on
        // every single request, and because `send` discarded the response body
        // the only signal was "status codes 400xN". Failing here, at parse time,
        // turns a silent full-run failure into one line before any traffic.
        let err = parse_dsn("http://pk_abc@localhost:8081/detail").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("UUID") || msg.contains("uuid"),
            "error should name the real requirement, got: {msg}"
        );
    }

    #[test]
    fn round_trips_dsn_display() {
        let t = parse_dsn("https://key@example.com/11111111-2222-3333-4444-555555555555").unwrap();
        assert_eq!(
            t.dsn(),
            "https://key@example.com/11111111-2222-3333-4444-555555555555"
        );
    }

    #[test]
    fn rejects_missing_parts() {
        assert!(parse_dsn("localhost:8081/app").is_err()); // no scheme
        assert!(parse_dsn("http://localhost:8081/app").is_err()); // no key@
        assert!(parse_dsn("http://pk@localhost:8081").is_err()); // no /app_id
        assert!(parse_dsn("http://@localhost:8081/app").is_err()); // empty key
    }
}
