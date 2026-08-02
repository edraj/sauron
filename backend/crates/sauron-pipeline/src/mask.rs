//! Forward enforcement: mask developer-supplied values on their way in.
//!
//! Two application sites, ONE policy lookup per job. `apply_wire` runs
//! immediately after `serde_json::from_str::<IngestJob>` succeeds, on the
//! owned wire payload; `apply_context` runs inside `process_job` right after
//! `enrich_context`, and touches ONLY targets whose column is `context` —
//! that is the enriched-only surface (the `woothee` `ua` block and
//! `device_key`), which the ingest edge physically cannot see.
//!
//! What this does NOT reach is named in the wiki and in the mask dialog: the
//! raw value still lives in `sauron:ingest:stream` for the `MAXLEN ~1e6`
//! window, and a payload that fails to DESERIALIZE dead-letters raw.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use sauron_core::envelope::{EnvelopeItem, IngestJob};
use sauron_db::PgPool;
use sauron_inspector::mask::{apply_wire_path, MASK_SENTINEL};
use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

/// The masked-key rows for one app, grouped for O(1) lookup per column.
#[derive(Debug, Default, Clone)]
pub struct MaskSet {
    by_column: HashMap<(String, String), Vec<String>>,
}

impl MaskSet {
    pub fn from_rows(rows: Vec<(String, String, String)>) -> MaskSet {
        let mut by_column: HashMap<(String, String), Vec<String>> = HashMap::new();
        for (table, column, path) in rows {
            by_column.entry((table, column)).or_default().push(path);
        }
        MaskSet { by_column }
    }

    pub fn is_empty(&self) -> bool {
        self.by_column.is_empty()
    }

    pub fn paths(&self, table: &str, column: &str) -> &[String] {
        self.by_column
            .get(&(table.to_string(), column.to_string()))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// An empty `json_path` means the whole column, which only ever applies to
    /// a TEXT column.
    pub fn masks_whole(&self, table: &str, column: &str) -> bool {
        self.paths(table, column).iter().any(|p| p.is_empty())
    }
}

fn mask_json(set: &MaskSet, table: &str, column: &str, v: &mut Value) {
    for path in set.paths(table, column) {
        if path.is_empty() {
            // Never collapse a whole jsonb document: `masks_whole` is for TEXT.
            continue;
        }
        apply_wire_path(v, path);
    }
}

/// Mask the owned wire payload in place.
///
/// Every field touched here is `pub` and owned on the envelope types.
pub fn apply_wire(set: &MaskSet, job: &mut IngestJob) {
    if set.is_empty() {
        return;
    }
    match &mut job.item {
        EnvelopeItem::Error(e) => {
            mask_json(set, "error_events", "tags", &mut e.tags);
            mask_json(set, "error_events", "contexts", &mut e.contexts);
            mask_json(set, "error_events", "extra", &mut e.extra);
            if !set.paths("error_events", "breadcrumbs").is_empty() {
                // `breadcrumbs` is a typed Vec on the wire but a jsonb column
                // at rest, so it round-trips through Value to reuse ONE path
                // applier rather than forking a second one that would drift.
                let mut v = serde_json::to_value(&e.breadcrumbs).unwrap_or(Value::Null);
                mask_json(set, "error_events", "breadcrumbs", &mut v);
                if let Ok(back) = serde_json::from_value(v) {
                    e.breadcrumbs = back;
                }
            }
            // `error_events.title`/`culprit` are derived server-side by
            // `build_title`/`build_culprit` and have NO wire field, so the
            // only way forward enforcement reaches them is by masking the
            // INPUTS. That is what `expand_targets` produces.
            if set.masks_whole("error_events", "message") && e.message.is_some() {
                e.message = Some(MASK_SENTINEL.to_string());
            }
            if let Some(exc) = e.exception.as_mut() {
                // Both fields are guarded on already carrying a value: the
                // retro-mask can only rewrite a value that exists, so masking
                // an absent one here would FABRICATE `"****"` on the wire and
                // make the enforcer and the at-rest mask disagree about what
                // the same event looks like.
                if set.masks_whole("error_events", "exception_value") && exc.value.is_some() {
                    exc.value = Some(MASK_SENTINEL.to_string());
                }
                if set.masks_whole("error_events", "exception_type") {
                    exc.ty = MASK_SENTINEL.to_string();
                }
            }
            if !set.paths("error_events", "event_user").is_empty() {
                if let Some(user) = e.user.as_mut() {
                    let mut v = serde_json::to_value(&*user).unwrap_or(Value::Null);
                    mask_json(set, "error_events", "event_user", &mut v);
                    if let Ok(back) = serde_json::from_value(v) {
                        *user = back;
                    }
                }
            }
        }
        EnvelopeItem::Event(ev) => {
            mask_json(set, "analytics_events", "properties", &mut ev.properties);
            mask_json(set, "analytics_events", "tags", &mut ev.tags);
            mask_json(set, "analytics_events", "contexts", &mut ev.contexts);
            mask_json(set, "analytics_events", "extra", &mut ev.extra);
        }
        EnvelopeItem::Identify(id) => {
            // Reachable through forward enforcement ONLY: `upsert_event_user`
            // merges with `||`, which never removes keys, so an at-rest mask is
            // undone by the next identify(). The UI says so.
            mask_json(set, "event_users", "properties", &mut id.traits);
        }
        EnvelopeItem::Transaction(t) => {
            if set.masks_whole("transactions", "url") && t.url.is_some() {
                t.url = Some(MASK_SENTINEL.to_string());
            }
        }
        // Breadcrumb batches carry no maskable column of their own; they are
        // folded into `error_events.breadcrumbs` when an error arrives.
        EnvelopeItem::BreadcrumbBatch(_) => {}
    }
}

/// Mask ONLY the enriched context. Called after `enrich_context`.
pub fn apply_context(set: &MaskSet, context: &mut Value) {
    if set.is_empty() {
        return;
    }
    for table in ["error_events", "analytics_events"] {
        for path in set.paths(table, "context") {
            if path.is_empty() {
                continue;
            }
            apply_wire_path(context, path);
        }
    }
}

/// Per-app masked-key cache with a short TTL, negative-cached.
///
/// A mask takes effect on every pipeline replica within about
/// `INSPECTOR_POLICY_CACHE_SECS`; the API returns that number so the UI can
/// state it literally rather than hardcoding "30 seconds".
///
/// FAILS STALE, NOT OPEN. Serving an empty set on error is tempting — failing
/// closed would drop telemetry — but the trigger set is much wider than the
/// RPM-upgrade case: a pool checkout timeout, a statement timeout, a failover
/// or a rolled-back migration would all silently disable masking
/// deployment-wide with only a `warn!`. Because the retro-mask is a one-shot
/// job that ends at `done`, every row written during that window stays raw
/// FOREVER. A five-minute Postgres blip must not permanently defeat an
/// irreversible redaction the operator was told had converged.
pub struct PolicyCache {
    pool: PgPool,
    ttl: Duration,
    inner: RwLock<HashMap<Uuid, Entry>>,
}

struct Entry {
    set: Arc<MaskSet>,
    loaded_at: Instant,
    /// Set when the last refresh FAILED. The warn is rate-limited to once per
    /// app per TTL — without that, an upgrade where migrations have not been
    /// re-run means one failing query and one log line PER INGESTED EVENT,
    /// doubling DB round-trips on the same 8 connections that accept traffic
    /// and flooding journald at ingest rate.
    last_error_at: Option<Instant>,
}

impl PolicyCache {
    pub fn new(pool: PgPool, ttl_secs: u64) -> PolicyCache {
        PolicyCache {
            pool,
            ttl: Duration::from_secs(ttl_secs.max(1)),
            inner: RwLock::new(HashMap::new()),
        }
    }

    pub async fn get(&self, app_id: Uuid) -> Arc<MaskSet> {
        if let Some(hit) = self.fresh(app_id) {
            return hit;
        }
        match self.load(app_id).await {
            Ok(set) => {
                let set = Arc::new(set);
                if let Ok(mut w) = self.inner.write() {
                    w.insert(
                        app_id,
                        Entry {
                            set: set.clone(),
                            loaded_at: Instant::now(),
                            last_error_at: None,
                        },
                    );
                }
                set
            }
            Err(e) => self.serve_stale(app_id, e),
        }
    }

    fn fresh(&self, app_id: Uuid) -> Option<Arc<MaskSet>> {
        let r = self.inner.read().ok()?;
        let entry = r.get(&app_id)?;
        (entry.loaded_at.elapsed() < self.ttl).then(|| entry.set.clone())
    }

    async fn load(&self, app_id: Uuid) -> anyhow::Result<MaskSet> {
        // Never hold this across the rest of the job: the ingest pool is 8 for
        // the whole process and the workers share it with every insert.
        let mut conn = sauron_db::conn(&self.pool).await?;
        let rows = sauron_db::repo::masked_keys_for_app(&mut conn, app_id).await?;
        drop(conn);
        Ok(MaskSet::from_rows(
            rows.into_iter()
                .map(|r| (r.target_table, r.target_column, r.json_path))
                .collect(),
        ))
    }

    fn serve_stale(&self, app_id: Uuid, err: anyhow::Error) -> Arc<MaskSet> {
        let mut should_warn = true;
        let mut stale = None;
        if let Ok(mut w) = self.inner.write() {
            if let Some(entry) = w.get_mut(&app_id) {
                should_warn = entry
                    .last_error_at
                    .map(|t| t.elapsed() >= self.ttl)
                    .unwrap_or(true);
                entry.last_error_at = Some(Instant::now());
                // Push `loaded_at` forward so the next event does not retry
                // immediately; the set is served stale for one more TTL.
                entry.loaded_at = Instant::now();
                stale = Some(entry.set.clone());
            }
        }
        if should_warn {
            warn!(
                app_id = %app_id,
                error = %err,
                serving_stale = stale.is_some(),
                "could not load masked keys; forward masking is degraded \
                 (run `systemctl start sauron-migrate` after an upgrade)"
            );
        }
        // Only when NO successful load has ever happened for this app does the
        // enforcer fall back to an empty set.
        stale.unwrap_or_else(|| Arc::new(MaskSet::default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn set(rows: &[(&str, &str, &str)]) -> MaskSet {
        MaskSet::from_rows(
            rows.iter()
                .map(|(t, c, p)| (t.to_string(), c.to_string(), p.to_string()))
                .collect(),
        )
    }

    #[test]
    fn masks_a_nested_path_in_a_jsonb_value() {
        let s = set(&[("error_events", "extra", "customer.email")]);
        let mut v = json!({"customer": {"email": "jane@acme.com", "keep": 1}});
        mask_json(&s, "error_events", "extra", &mut v);
        assert_eq!(v, json!({"customer": {"email": "****", "keep": 1}}));
    }

    #[test]
    fn a_row_for_another_column_does_nothing() {
        let s = set(&[("error_events", "tags", "email")]);
        let before = json!({"email": "a@b.c"});
        let mut v = before.clone();
        mask_json(&s, "error_events", "extra", &mut v);
        assert_eq!(v, before);
    }

    /// An empty `json_path` means the WHOLE column, which only makes sense for
    /// a TEXT column — the caller checks `masks_whole`, and the jsonb applier
    /// must skip it rather than collapsing the entire document.
    #[test]
    fn an_empty_path_never_collapses_a_jsonb_column() {
        let s = set(&[("error_events", "extra", "")]);
        let before = json!({"a": 1});
        let mut v = before.clone();
        mask_json(&s, "error_events", "extra", &mut v);
        assert_eq!(v, before);
        assert!(s.masks_whole("error_events", "extra"));
    }

    /// `apply_context` only ever touches targets whose column is `context` —
    /// the ENRICHED surface the ingest edge physically cannot see.
    #[test]
    fn apply_context_only_touches_context_targets() {
        let s = set(&[
            ("error_events", "context", "user.email"),
            ("error_events", "extra", "customer.email"),
        ]);
        let mut ctx = json!({"user": {"email": "a@b.c", "id": "u1"}, "ua": {"browser": "x"}});
        apply_context(&s, &mut ctx);
        assert_eq!(ctx["user"]["email"], json!("****"));
        assert_eq!(ctx["user"]["id"], json!("u1"));
        assert_eq!(ctx["ua"]["browser"], json!("x"));
    }

    #[test]
    fn an_empty_set_is_a_no_op() {
        let s = MaskSet::default();
        let before = json!({"user": {"email": "a@b.c"}});
        let mut ctx = before.clone();
        apply_context(&s, &mut ctx);
        assert_eq!(ctx, before);
        assert!(s.is_empty());
    }

    /// `issues.title` is masked at rest and by the sticky guard in
    /// `upsert_issue`, never on the wire — a row for it must be ignored here
    /// rather than panicking.
    #[test]
    fn rows_for_tables_the_wire_does_not_carry_are_ignored() {
        let s = set(&[("issues", "title", "")]);
        assert!(s.paths("error_events", "extra").is_empty());
        assert!(!s.masks_whole("error_events", "message"));
    }
}
