//! `sauron-api` — the JWT-authed dashboard API.
//!
//! Auth (register/login/refresh/logout), org/project management, the issues
//! API, and product-analytics queries. Every data route is scoped to the
//! caller's org/project membership.

mod admin_storage;
mod audit;
mod csv;
mod error;
mod mail;
mod overview_cache;
mod routes;
mod symbolicate;
mod tasks;
mod tier_read;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, FromRef};
use axum::http::header::{self, AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderValue, Method};
use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

/// Hard ceiling on a JSON request body. Large binary uploads go through the
/// separately-merged artifact routes, which carry their own raised limit.
const API_JSON_BODY_LIMIT: usize = 1024 * 1024;
/// Wall-clock budget for a single request. Bounds how long one expensive query
/// can hold a connection and a worker.
const REQUEST_TIMEOUT_SECS: u64 = 30;
/// Requests admitted concurrently before the service starts shedding load.
const MAX_INFLIGHT_REQUESTS: usize = 512;
/// How often the outbox is expired, scrubbed and pruned. A compile-time constant
/// rather than a variable: three files of documentation for a number nobody tunes
/// is how a config surface becomes unmaintainable.
const MAIL_HYGIENE_INTERVAL: Duration = Duration::from_secs(900);
/// How long a reset row survives, consumed or not.
///
/// This table is the only audit trail the deployment has that an admin forced a
/// reset on someone — there is no `audit_events` table — so this constant also
/// caps how far back that question can be answered. A compile-time constant
/// rather than an env var: a handful of tiny short-lived rows do not justify
/// three files of documentation.
const PASSWORD_RESET_RETENTION_DAYS: i64 = 30;
/// Footer line on every product email. Deliberately says nothing about why the
/// recipient is receiving it — each sender's own footnotes do that.
const MAIL_FOOTER: &str = "Sent by Sauron. This mailbox is not monitored.";

use sauron_auth::JwtKeys;
use sauron_core::Config;
use sauron_db::PgPool;
use sauron_redis::{RedisStore, SymbolBlobCache};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub redis: RedisStore,
    pub keys: JwtKeys,
    pub cfg: Arc<Config>,
    /// Isolated warm-blob cache for symbol artifacts (no-op when unconfigured).
    pub symbols: SymbolBlobCache,
    /// Shared symbolication engine (holds the in-process parsed-map cache).
    pub symbolicator: Arc<sauron_symbols::Symbolicator>,
    /// Alert dispatch (channel secret crypto + SSRF-safe delivery).
    pub alerts: sauron_alerts::AlertEngine,
    /// Revoked sessions this replica knows about. Read by the `AuthUser`
    /// extractor on every authenticated request; refreshed by the
    /// `revocation-poll` background task.
    pub revocations: sauron_auth::SessionRevocations,
    /// `None` when SMTP is unconfigured. Every caller must degrade rather than
    /// fail: the API has to boot and serve everything else on a deployment with
    /// no relay. An unauthenticated route's response must be identical either
    /// way — a response that distinguishes configured from unconfigured is a
    /// config oracle handed to anyone on the internet.
    pub mail: Option<crate::mail::MailSender>,
    /// Admission gate for the active-users report — the heaviest query in the
    /// product, runnable by the lowest-privileged role. Three permits, and a
    /// 503 rather than a queue: the DB pool is 16 for the whole process, so
    /// queueing here would surface as pool-checkout 500s on unrelated
    /// endpoints, including /v1/auth/login and /health.
    pub active_users_gate: std::sync::Arc<tokio::sync::Semaphore>,
    /// Result cache, recompute single-flight and SSE fan-out for the Overview
    /// page. Its aggregates outgrew the 30s request timeout, so they no longer
    /// run on the request path at all — see `overview_cache`'s module docs.
    pub overview_cache: crate::overview_cache::OverviewCache,
    /// Whether `event_users.identified_at` exists, probed once at boot.
    ///
    /// Probed rather than assumed because RPM upgrades do not re-run
    /// `sauron-migrate`. Refusing to START would be an unnecessary
    /// deployment-wide outage over one endpoint, so this only turns the
    /// active-users routes into a 503 that names the fix.
    pub event_users_identified: bool,
}

impl FromRef<AppState> for JwtKeys {
    fn from_ref(state: &AppState) -> JwtKeys {
        state.keys.clone()
    }
}

impl FromRef<AppState> for sauron_auth::SessionRevocations {
    fn from_ref(state: &AppState) -> sauron_auth::SessionRevocations {
        state.revocations.clone()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sauron_telemetry::init("sauron-api");
    // Behind an Arc from the start: the background tasks below capture settings
    // out of it, and the state build would otherwise move it first.
    let cfg = Arc::new(Config::from_env()?);

    let pool = sauron_db::build_pool(&cfg.database_url, 16)?;
    let redis = RedisStore::connect(&cfg.redis_url).await?;
    let keys = JwtKeys::new(cfg.require_jwt_secret()?, cfg.jwt_access_ttl_secs);
    let symbols = SymbolBlobCache::connect(
        cfg.symbols_redis_url.as_deref(),
        cfg.symbols_redis_max_blob_mb * 1024 * 1024,
    )
    .await;
    let symbolicator = Arc::new(sauron_symbols::Symbolicator::new(
        cfg.symbols_cache_mb * 1024 * 1024,
    ));
    // Allow artifact uploads well above axum's 2 MB default body limit.
    let artifact_body_limit = (cfg.symbols_max_artifact_mb + 8) * 1024 * 1024;

    // Keep the seeded preset roles in sync with code, and learn once whether
    // the schema is ahead of or behind this binary.
    let event_users_identified = {
        let mut conn = sauron_db::conn(&pool).await?;
        // FIRST, before anything else touches a table: does this binary's
        // embedded migration set match what the database has applied? An
        // upgrade that skipped `sauron-migrate` otherwise surfaces as a 500 per
        // request forever, under a boot log that says everything is fine. One
        // query; refuses to boot when the database is behind (see
        // `sauron_db::require_current_schema` for why refusing beats degrading,
        // and for the `SAURON_ALLOW_SCHEMA_DRIFT` escape hatch).
        //
        // Ordered ahead of `ensure_preset_roles` on purpose: that call is
        // itself a schema-dependent write, so on a drifting database it would
        // otherwise fail first and report a diesel column error instead of the
        // remedy.
        sauron_db::require_current_schema(&mut conn, "sauron-api").await?;
        sauron_auth::ensure_preset_roles(&mut conn).await?;
        let present = sauron_db::repo::probe_event_users_identified(&mut conn)
            .await
            .is_ok();
        drop(conn);
        if !present {
            tracing::error!(
                "event_users.identified_at is missing — run sauron-migrate (see \
                 packaging/rpm/SETUP.md §11). GET /v1/projects/{{project_id}}/active-users \
                 will return 503 schema_migration_required until it is applied."
            );
        }
        present
    };

    let port = cfg.api_port;
    let origins: Vec<HeaderValue> = cfg
        .cors_allowed_origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();

    // The channel cipher. Fail-closed, with no JWT_SECRET fallback: this key
    // decrypts both halves of every notification channel, and deriving it from
    // a value operators are told to rotate turned a routine rotation into
    // silent, total loss of every stored credential.
    let notify_cipher = sauron_alerts::SecretCipher::new(cfg.require_notify_secret_key()?);

    // Migration 000046 moved `config` behind the cipher; the row conversion
    // cannot run in SQL (no pgcrypto, and the key lives in the environment) and
    // must not run in `sauron-migrate` (no cipher there, and RPM upgrades never
    // re-run it). It happens here, once, idempotently — and a failure takes the
    // boot with it rather than leaving the table half-converted.
    sauron_alerts::crypto::seal_legacy_channel_configs(&pool, &notify_cipher).await?;

    let alerts = sauron_alerts::AlertEngine::new(
        notify_cipher,
        cfg.alerts_allow_private,
        cfg.alerts_deliver_timeout_ms,
    );

    // The pool is moved into the state below; the hygiene task needs its own
    // handle.
    let hygiene_pool = pool.clone();

    let branding = sauron_mail::Branding {
        product_name: "Sauron".to_string(),
        // `.ok()` on purpose: an unset DASHBOARD_URL disables link-bearing mail
        // at render time with a message naming the variable, rather than
        // preventing the process from booting.
        dashboard_url: cfg.require_dashboard_url().ok().map(|s| s.to_string()),
        footer: MAIL_FOOTER.to_string(),
    };

    let mail = match cfg.require_smtp() {
        Err(e) => {
            // Two very different situations reach here, and logging them at the
            // same level buried the second one. Nothing configured is the
            // ordinary state of a deployment that never wanted transactional
            // email — one INFO line, not a warning and not a failure. But a
            // relay that WAS configured and then refused (a bad SMTP_FROM, a
            // cleartext relay off-box) is a misconfiguration whose entire
            // visible symptom is that password reset silently never arrives.
            // That one an operator has to be able to find, and an INFO line
            // identical to "you didn't set this up" is not findable.
            if std::env::var("SMTP_HOST").is_ok_and(|v| !v.trim().is_empty()) {
                warn!(reason = %e, "SMTP_HOST is set but the relay was refused; password reset and every other transactional email is DISABLED");
            } else {
                info!(reason = %e, "transactional email disabled");
            }
            None
        }
        Ok(s) => {
            let mut params = sauron_mail::SmtpParams::from_settings(s);
            // Two explicit variables, because logs are routinely shipped to an
            // aggregator with a broader reader set and a longer retention than the
            // database. RUST_LOG is no gate: the shipped default is
            // `info,sauron=debug` and EnvFilter matches targets by prefix.
            params.sink_log_body = s.sink && cfg.dev_mode;
            if s.sink {
                tracing::warn!(
                    log_bodies = params.sink_log_body,
                    "SMTP_SINK=1: transactional email is written to the log and NEVER \
                     transmitted; rows are recorded as status='sink'"
                );
            }
            Some(mail::MailSender::new(
                pool.clone(),
                params,
                s.from_address.clone(),
                s.from_name.clone(),
                branding,
            ))
        }
    };

    // The drain only exists where a relay does.
    if let Some(sender) = mail.clone() {
        let tick = Duration::from_secs(cfg.mail_drain_tick_secs);
        tasks::supervise("mail_drain", tick, move || {
            let s = sender.clone();
            async move {
                s.drain_once().await;
                Ok(())
            }
        });
    }

    // UNCONDITIONAL, and that is the whole point of splitting it out. An operator
    // who enables SMTP, sends reset mail, then unsets SMTP_HOST — rotating
    // relays, cutting cost, or responding to an incident — would otherwise leave
    // every pending row, each holding a working reset URL, in Postgres
    // permanently, backed up and replicated, with no code path that will ever
    // touch it again.
    let retention_days = cfg.mail_outbox_retention_days;
    tasks::supervise("mail_hygiene", MAIL_HYGIENE_INTERVAL, move || {
        let p = hygiene_pool.clone();
        async move { mail::hygiene(&p, retention_days).await }
    });

    let state = AppState {
        pool,
        redis,
        keys,
        cfg: cfg.clone(),
        symbols,
        symbolicator,
        alerts,
        revocations: sauron_auth::SessionRevocations::new(),
        mail,
        active_users_gate: Arc::new(tokio::sync::Semaphore::new(3)),
        overview_cache: crate::overview_cache::OverviewCache::new(),
        event_users_identified,
    };

    // Floored at 900 on purpose. The correctness argument — "a token minted
    // before a revocation older than the access TTL has already expired on its
    // own exp" — only holds if the TTL never DECREASES. An operator hardening
    // 900 -> 120 and restarting leaves pre-restart tokens alive for 900s against
    // a 240s window: ~11 minutes of accepted-but-revoked access, with no error
    // and no log. Clamped above because JWT_ACCESS_TTL_SECS is an unvalidated
    // i64 from the environment; `parse()` has no floor, no ceiling and no sign
    // check, and a negative value cast to u64 wraps to ~1.8e19.
    let revocation_window_secs = state.cfg.jwt_access_ttl_secs.clamp(900, 86_400) + 120;
    let revocation_poll = Duration::from_secs(state.cfg.auth_revocation_poll_secs.clamp(1, 60));

    // Deliberately NOT preceded by a synchronous `revocations.refresh(..).await?`
    // before the listener binds — see tasks.rs. The snapshot starts empty and the
    // supervisor retries; one poll interval of stale revocation data on a cold
    // start is strictly smaller than the 900-second window that exists today.
    {
        let revocations = state.revocations.clone();
        let pool = state.pool.clone();
        tasks::supervise("revocation-poll", revocation_poll, move || {
            let revocations = revocations.clone();
            let pool = pool.clone();
            async move {
                revocations.refresh(&pool, revocation_window_secs).await?;
                Ok(())
            }
        });
    }

    {
        // `auth_sessions` is a permanent per-user record of where and on what
        // device someone signed in, and its partial index is proportional to
        // lifetime logins, not to live sessions — nothing writes `revoked_at`
        // when a session merely expires. The reaper lives here because the rule
        // is that a table's reaper runs in the process that owns its write path.
        let pool = state.pool.clone();
        tasks::supervise(
            "auth-session-reaper",
            Duration::from_secs(86_400),
            move || {
                let pool = pool.clone();
                async move {
                    let mut conn = sauron_db::conn(&pool).await?;
                    let deleted = sauron_db::repo::prune_auth_sessions(
                        &mut conn,
                        sauron_db::repo::AUTH_SESSION_RETENTION_DAYS,
                    )
                    .await?;
                    // The API pool is 16 for the whole process; never hold a slot
                    // across work that does not need one.
                    drop(conn);
                    if deleted > 0 {
                        tracing::info!(deleted, "pruned expired and long-revoked auth_sessions");
                    }
                    Ok(())
                }
            },
        );
    }

    {
        // Lives here, not in `sauron-alerts`. packaging/rpm/SETUP.md's shipped
        // install line is
        // `systemctl enable --now sauron-api sauron-ingest sauron-monitor sauron-tier`,
        // there is no preset file under packaging/rpm/, and `%systemd_post` falls
        // through to the distro default of `disable` — so on every RPM deployment a
        // reaper in that binary would simply never run, while this table's write
        // path is an unauthenticated endpoint. The rule is that a table's reaper
        // lives in the process that owns its write path.
        //
        // Deleting these rows disables nothing: unlike `refresh_tokens`, whose
        // revoked rows are load-bearing for replay detection, nothing reads a dead
        // reset row.
        let pool = state.pool.clone();
        tasks::supervise(
            "password_reset_reaper",
            Duration::from_secs(3600),
            move || {
                let pool = pool.clone();
                async move {
                    let mut conn = sauron_db::conn(&pool).await?;
                    let removed = sauron_db::repo::prune_password_reset_tokens(
                        &mut conn,
                        PASSWORD_RESET_RETENTION_DAYS,
                    )
                    .await?;
                    // Checked out, worked, dropped — the API pool is 16 for the
                    // whole process and this loop must not hold one between ticks.
                    drop(conn);
                    if removed > 0 {
                        tracing::info!(removed, "pruned expired password reset tokens");
                    }
                    Ok(())
                }
            },
        );
    }

    // Symbol-artifact routes carry large binary uploads, so they get their own
    // raised body limit (merged separately from the JSON API).
    let artifact_routes = Router::new()
        .route(
            "/v1/apps/{app_id}/artifacts",
            post(routes::artifacts::upload).get(routes::artifacts::list),
        )
        .route(
            "/v1/apps/{app_id}/artifacts/{artifact_id}",
            delete(routes::artifacts::delete),
        )
        .layer(DefaultBodyLimit::max(artifact_body_limit));

    let cors = CorsLayer::new()
        .allow_origin(origins)
        // Every method any route actually uses. Omitting one does NOT surface
        // as a failing preflight — the OPTIONS still answers 200 — it surfaces
        // as `net::ERR_FAILED` on the real request, from the browser only. No
        // Rust test can catch it, because none of them are subject to CORS.
        // `PUT` arrived with the store-connection upsert.
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([AUTHORIZATION, CONTENT_TYPE])
        // In BOTH shipped topologies the dashboard origin is not the API
        // origin (nginx serves the SPA on :80 with API_BASE_URL elsewhere; dev
        // is :3000 vs :8090), so without this
        // `res.headers['content-disposition']` is `undefined` in the browser
        // and every CSV download silently falls back to a generic filename —
        // a bug that reproduces in dev AND in production.
        .expose_headers([header::CONTENT_DISPOSITION]);

    let app = Router::new()
        .route("/health", get(health))
        // --- auth ---
        .route("/v1/auth/register", post(routes::auth::register))
        .route("/v1/auth/login", post(routes::auth::login))
        .route("/v1/auth/refresh", post(routes::auth::refresh))
        .route("/v1/auth/logout", post(routes::auth::logout))
        // Unauthenticated by design: the reset token travels in the body/URL
        // fragment, never as a bearer, so `password_change_gate` is never
        // reached and the extractor's allowlist stays exactly two paths.
        .route(
            "/v1/auth/forgot-password",
            post(routes::auth::forgot_password),
        )
        .route(
            "/v1/auth/reset-password",
            post(routes::auth::reset_password),
        )
        // Path must match the extractor's forced-change allowlist exactly.
        .route("/v1/auth/password", post(routes::auth::change_password))
        .route("/v1/me", get(routes::auth::me))
        // --- the caller's own account ---
        // Not `/v1/sessions` — that name is taken by product telemetry
        // (`GET /v1/apps/{app_id}/sessions`). None of these match
        // `/v1/apps/{app_id}/...`, so `routes::scope::reject_environment_id` is
        // not required and the env-scoping router enumeration does not see them.
        .route("/v1/me/sessions", get(routes::account::list_sessions))
        .route(
            "/v1/me/sessions/{session_id}",
            delete(routes::account::revoke_session),
        )
        .route(
            "/v1/me/sessions/revoke-others",
            post(routes::account::revoke_other_sessions),
        )
        // --- orgs, members, grants, roles ---
        .route(
            "/v1/orgs",
            get(routes::orgs::list_orgs).post(routes::orgs::create_org),
        )
        .route("/v1/orgs/{org_id}/access", get(routes::orgs::access))
        .route(
            "/v1/orgs/{org_id}/members",
            get(routes::orgs::list_members).post(routes::orgs::create_member),
        )
        .route(
            "/v1/orgs/{org_id}/members/{user_id}",
            patch(routes::orgs::set_member_active),
        )
        .route(
            "/v1/orgs/{org_id}/members/{user_id}/password-reset",
            post(routes::orgs::reset_member_password),
        )
        .route(
            "/v1/orgs/{org_id}/members/{user_id}/revoke-sessions",
            post(routes::orgs::revoke_member_sessions),
        )
        .route("/v1/orgs/{org_id}/grants", post(routes::orgs::create_grant))
        .route(
            "/v1/grants/{grant_id}",
            delete(routes::orgs::delete_grant).patch(routes::orgs::update_grant_handler),
        )
        .route(
            "/v1/orgs/{org_id}/roles",
            get(routes::orgs::list_roles).post(routes::orgs::create_role),
        )
        .route(
            "/v1/orgs/{org_id}/roles/{role_id}",
            patch(routes::orgs::update_role_handler).delete(routes::orgs::delete_role_handler),
        )
        // --- projects (grouping) ---
        .route(
            "/v1/orgs/{org_id}/projects",
            get(routes::projects::list_projects).post(routes::projects::create_project),
        )
        .route(
            "/v1/projects/{project_id}",
            get(routes::projects::get_project)
                .patch(routes::projects::update_project)
                .delete(routes::projects::delete_project),
        )
        .route(
            "/v1/projects/{project_id}/apps",
            get(routes::projects::list_apps).post(routes::projects::create_app),
        )
        // --- apps ---
        .route(
            "/v1/apps/{app_id}",
            get(routes::apps::get_app)
                .patch(routes::apps::update_app)
                .delete(routes::apps::delete_app),
        )
        // --- app stores ---
        // Credentials are write-only: no response here carries a secret. The
        // sync itself belongs to `sauron-storesync`; `/sync` only queues.
        .route(
            "/v1/apps/{app_id}/store-connections",
            get(routes::stores::list),
        )
        .route(
            "/v1/apps/{app_id}/store-connections/{store}",
            put(routes::stores::upsert).delete(routes::stores::delete),
        )
        .route(
            "/v1/apps/{app_id}/store-connections/{store}/sync",
            post(routes::stores::queue_sync),
        )
        .route(
            "/v1/apps/{app_id}/store-metrics",
            get(routes::stores::metrics),
        )
        // The catalogue: environments as a project defines them. There is no
        // POST under `/v1/apps/{app_id}/environments` any more — an app does not
        // get to invent an environment its siblings have never heard of.
        .route(
            "/v1/projects/{project_id}/environments",
            get(routes::environments::list_project_environments)
                .post(routes::environments::create_project_environment),
        )
        .route(
            "/v1/environments/{env_id}",
            patch(routes::environments::update_project_environment)
                .delete(routes::environments::retire_project_environment),
        )
        // Enrollments: one app's membership in one environment, which is what
        // carries the ingest key and the per-app switches.
        .route(
            "/v1/apps/{app_id}/environments",
            get(routes::environments::list_app_environments),
        )
        // No DELETE here on purpose. Withdrawing one app from an environment
        // would be a one-way door: enrollment happens only when an environment
        // or an app is created, so there is no path back short of retiring the
        // environment project-wide and recreating it, which re-keys every
        // sibling app. `PATCH { ingest_enabled: false }` expresses the same
        // intent — "this app should not report here" — and is reversible.
        .route(
            "/v1/app-environments/{id}",
            patch(routes::environments::update_app_environment),
        )
        .route(
            "/v1/app-environments/{id}/rotate-key",
            post(routes::environments::rotate_app_environment_key),
        )
        .route(
            "/v1/apps/{app_id}/first-event",
            get(routes::apps::first_event),
        )
        // --- issues (app-scoped) ---
        .route("/v1/apps/{app_id}/issues", get(routes::issues::list))
        .route(
            "/v1/apps/{app_id}/issues/{issue_id}",
            get(routes::issues::detail).patch(routes::issues::update),
        )
        .route(
            "/v1/apps/{app_id}/issues/{issue_id}/events",
            get(routes::issues::events),
        )
        .route(
            "/v1/apps/{app_id}/issues/{issue_id}/events/stats",
            get(routes::issues::event_stats),
        )
        // --- analytics (app-scoped) ---
        .route(
            "/v1/apps/{app_id}/events/top",
            get(routes::analytics::top_events),
        )
        .route(
            "/v1/apps/{app_id}/events/series",
            get(routes::analytics::event_series),
        )
        .route(
            "/v1/apps/{app_id}/events/list",
            get(routes::analytics::events_list),
        )
        .route(
            "/v1/apps/{app_id}/persons",
            get(routes::analytics::persons_list),
        )
        .route(
            "/v1/apps/{app_id}/persons/{distinct_id}",
            get(routes::analytics::person),
        )
        .route(
            "/v1/apps/{app_id}/overview",
            get(routes::analytics::overview),
        )
        // The same data as `/overview`, addressable one section at a time, so the
        // dashboard can render each card as its own answer lands instead of
        // waiting on the sum of five sequential aggregates. `/overview` stays for
        // callers that want one round trip.
        .route(
            "/v1/apps/{app_id}/analytics/active-users",
            get(routes::analytics::active_users_series),
        )
        .route(
            "/v1/apps/{app_id}/overview/totals",
            get(routes::analytics::overview_totals),
        )
        .route(
            "/v1/apps/{app_id}/overview/series",
            get(routes::analytics::overview_series),
        )
        .route(
            "/v1/apps/{app_id}/overview/top-issues",
            get(routes::analytics::overview_top_issues),
        )
        .route(
            "/v1/apps/{app_id}/overview/top-events",
            get(routes::analytics::overview_top_events),
        )
        // The push half of the Overview cache: the five sections above now
        // answer instantly from Redis and enqueue their own recompute, and the
        // finished aggregate arrives here rather than on the request that
        // triggered it.
        .route(
            "/v1/apps/{app_id}/overview/stream",
            get(routes::analytics::overview_stream),
        )
        .route(
            "/v1/apps/{app_id}/overview/refresh",
            post(routes::analytics::overview_refresh),
        )
        .route(
            "/v1/apps/{app_id}/users/summary",
            get(routes::analytics::users_summary),
        )
        .route(
            "/v1/apps/{app_id}/errors/timeseries",
            get(routes::analytics::error_timeseries),
        )
        .route(
            "/v1/apps/{app_id}/events/timeseries",
            get(routes::analytics::event_timeseries),
        )
        .route(
            "/v1/apps/{app_id}/transactions/timeseries",
            get(routes::analytics::transaction_timeseries),
        )
        // The searched per-span list, as opposed to the two aggregates above
        // and under `/performance`. Registered AFTER `/transactions/timeseries`
        // so the literal segment keeps winning; axum matches literals over the
        // bare path regardless, but the order states the intent.
        .route(
            "/v1/apps/{app_id}/transactions",
            get(routes::transactions::list),
        )
        // --- exceptions dashboard ---
        .route("/v1/apps/{app_id}/issues/stats", get(routes::issues::stats))
        // --- search schema ---
        .route(
            "/v1/apps/{app_id}/search/schema",
            get(routes::search::schema),
        )
        // --- sessions (app-scoped) ---
        .route("/v1/apps/{app_id}/sessions", get(routes::sessions::list))
        .route(
            "/v1/apps/{app_id}/sessions/summary",
            get(routes::analytics::sessions_summary),
        )
        .route(
            "/v1/apps/{app_id}/sessions/{session_id}",
            get(routes::sessions::detail),
        )
        .route(
            "/v1/apps/{app_id}/sessions/{session_id}/workflows",
            get(routes::workflows::session_spans),
        )
        // --- devices (app-scoped) ---
        .route("/v1/apps/{app_id}/devices", get(routes::devices::list))
        .route(
            "/v1/apps/{app_id}/device-groups",
            get(routes::devices::groups),
        )
        .route("/v1/apps/{app_id}/device", get(routes::devices::detail))
        // --- row counts for the offset-paged lists ---
        //
        // Separate routes rather than a `total` on each list response: these
        // four lists page by a `limit + 1` over-fetch probe and have no total,
        // which is why they could only ever offer Prev/Next. Counting on its
        // own request keeps a slow count off the latency path of the table it
        // captions — the rows paint at the speed they always did and the page
        // strip resolves a beat later.
        //
        // `/counts/{resource}` and NOT `{resource}/count`. The nested form
        // collides on persons: `/v1/apps/{app_id}/persons/{distinct_id}` is
        // already registered, axum resolves a static segment ahead of a
        // `{param}` capture, and distinct IDs are arbitrary strings from SDK
        // `identify()` calls — so `/persons/count` would permanently shadow the
        // profile page of anyone identified as `count`. One prefix with no
        // dynamic sibling avoids having to dodge that per resource, the way
        // `/device` (singular) already dodges it for `/devices`.
        .route(
            "/v1/apps/{app_id}/counts/screens",
            get(routes::screens::count),
        )
        .route(
            "/v1/apps/{app_id}/counts/devices",
            get(routes::devices::count),
        )
        .route(
            "/v1/apps/{app_id}/counts/persons",
            get(routes::analytics::persons_count),
        )
        .route(
            "/v1/apps/{app_id}/counts/workflows",
            get(routes::workflows::count),
        )
        // --- screens (app-scoped) ---
        .route("/v1/apps/{app_id}/screens", get(routes::screens::list))
        .route(
            "/v1/apps/{app_id}/screens/detail",
            get(routes::screens::detail),
        )
        // The four collapsible sections on the screen detail page. Static
        // path segments, so they cannot collide with `screens/detail` or with
        // the `screens` list above.
        .route(
            "/v1/apps/{app_id}/screens/events",
            get(routes::screens::section_events),
        )
        .route(
            "/v1/apps/{app_id}/screens/exceptions",
            get(routes::screens::section_exceptions),
        )
        .route(
            "/v1/apps/{app_id}/screens/devices",
            get(routes::screens::section_devices),
        )
        .route(
            "/v1/apps/{app_id}/screens/users",
            get(routes::screens::section_users),
        )
        // --- funnels & journeys ---
        .route("/v1/apps/{app_id}/funnel", post(routes::funnels::compute))
        .route(
            "/v1/apps/{app_id}/funnels",
            get(routes::funnels::list_saved).post(routes::funnels::create_saved),
        )
        .route(
            "/v1/apps/{app_id}/funnels/{funnel_id}",
            patch(routes::funnels::update_saved).delete(routes::funnels::delete_saved),
        )
        .route("/v1/apps/{app_id}/journeys", get(routes::journeys::explore))
        // --- workflows (app-scoped) ---
        .route("/v1/apps/{app_id}/workflows", get(routes::workflows::list))
        .route(
            "/v1/apps/{app_id}/workflows/{name}",
            get(routes::workflows::detail),
        )
        .route(
            "/v1/apps/{app_id}/workflows/{name}/runs",
            get(routes::workflows::runs),
        )
        // --- uptime monitors (project-scoped) ---
        .route(
            "/v1/projects/{project_id}/monitors",
            get(routes::monitors::list).post(routes::monitors::create),
        )
        .route(
            "/v1/monitors/{monitor_id}",
            get(routes::monitors::detail)
                .patch(routes::monitors::update)
                .delete(routes::monitors::delete),
        )
        .route(
            "/v1/monitors/{monitor_id}/checks",
            get(routes::monitors::checks),
        )
        .route(
            "/v1/monitors/{monitor_id}/incidents",
            get(routes::monitors::incidents),
        )
        // --- combined active users (project-scoped) ---
        .route(
            "/v1/projects/{project_id}/active-users",
            get(routes::active_users::active_users),
        )
        // A separate route, not `?format=csv`: browsers download GETs, the view
        // must stay bookmarkable, and one handler returning two content types
        // collapses its success type to `Response` for both.
        .route(
            "/v1/projects/{project_id}/active-users.csv",
            get(routes::active_users::active_users_csv),
        )
        // --- performance (app-scoped) ---
        .route(
            "/v1/apps/{app_id}/performance/summary",
            get(routes::performance::summary),
        )
        .route(
            "/v1/apps/{app_id}/performance/series",
            get(routes::performance::series),
        )
        // --- alerting: channels, rules, history (org-scoped, alert:* gated) ---
        .route(
            "/v1/orgs/{org_id}/notification-channels",
            get(routes::notifications::list_channels).post(routes::notifications::create_channel),
        )
        .route(
            "/v1/notification-channels/{channel_id}",
            get(routes::notifications::get_channel)
                .patch(routes::notifications::update_channel)
                .delete(routes::notifications::delete_channel),
        )
        .route(
            "/v1/notification-channels/{channel_id}/test",
            post(routes::notifications::test_channel),
        )
        .route(
            "/v1/orgs/{org_id}/alert-rules",
            get(routes::notifications::list_rules).post(routes::notifications::create_rule),
        )
        .route(
            "/v1/alert-rules/{rule_id}",
            get(routes::notifications::get_rule)
                .patch(routes::notifications::update_rule)
                .delete(routes::notifications::delete_rule),
        )
        .route(
            "/v1/orgs/{org_id}/alert-events",
            get(routes::notifications::list_history),
        )
        .route("/v1/alert-meta", get(routes::notifications::meta))
        .route(
            "/v1/me/notification-subscriptions",
            get(routes::notification_prefs::list_subscriptions)
                .post(routes::notification_prefs::create_subscription),
        )
        .route(
            "/v1/me/notification-subscriptions/{id}",
            patch(routes::notification_prefs::patch_subscription)
                .delete(routes::notification_prefs::delete_subscription_route),
        )
        .route(
            "/v1/me/notifications",
            get(routes::notification_prefs::list_notifications),
        )
        .route(
            "/v1/notifications/unsubscribe",
            post(routes::notification_prefs::unsubscribe),
        )
        // --- pii inspector ---
        .route(
            "/v1/orgs/{org_id}/inspector/policies",
            get(routes::inspector::list_policies).post(routes::inspector::create_policy),
        )
        .route(
            "/v1/inspector/policies/{policy_id}",
            get(routes::inspector::get_policy)
                .patch(routes::inspector::patch_policy)
                .delete(routes::inspector::delete_policy),
        )
        .route(
            "/v1/apps/{app_id}/inspector/policy",
            get(routes::inspector::effective_policy),
        )
        .route(
            "/v1/inspector/policies/{policy_id}/scans",
            get(routes::inspector::list_scans).post(routes::inspector::start_scan),
        )
        .route(
            "/v1/inspector/scans/{scan_id}",
            get(routes::inspector::get_scan),
        )
        .route(
            "/v1/inspector/scans/{scan_id}/cancel",
            post(routes::inspector::cancel_scan),
        )
        .route(
            "/v1/inspector/scans/{scan_id}/findings",
            get(routes::inspector::list_findings),
        )
        .route(
            "/v1/inspector/findings/{finding_id}/reveal",
            post(routes::inspector::reveal_finding),
        )
        .route(
            "/v1/apps/{app_id}/inspector/mask-preview",
            post(routes::inspector::mask_preview),
        )
        .route(
            "/v1/apps/{app_id}/inspector/mask-actions",
            get(routes::inspector::list_app_mask_actions),
        )
        .route(
            "/v1/apps/{app_id}/inspector/masked-keys",
            get(routes::inspector::list_app_masked_keys),
        )
        .route(
            "/v1/inspector/mask-actions/{action_id}",
            get(routes::inspector::get_mask_action_handler),
        )
        .route(
            "/v1/inspector/mask-actions/{action_id}/confirm",
            post(routes::inspector::confirm_mask),
        )
        .route(
            "/v1/inspector/mask-actions/{action_id}/cancel",
            post(routes::inspector::cancel_mask),
        )
        .route(
            "/v1/orgs/{org_id}/inspector/mask-actions",
            get(routes::inspector::list_org_mask_actions),
        )
        // --- storage & records (org:manage required) ---
        .route("/v1/admin/storage", get(routes::admin::storage))
        // --- Wall of Shame: the administrative trail for one org ---
        // `org_id` is a required query parameter and is authorized against the
        // caller's grants inside the handler, so this is org-partitioned even
        // though the path is not.
        .route("/v1/admin/audit", get(routes::audit::list))
        // Same filters, same gate, whole filtered set rather than one page.
        .route("/v1/admin/audit.csv", get(routes::audit::export_csv))
        // Deployment-wide rotation policy. Gated on holding org:manage in EVERY
        // org (see require_deployment_admin) — a single tenant's admin must not be
        // able to move the hot/cold boundary for everyone.
        .route(
            "/v1/admin/tier-policy",
            get(routes::admin::get_tier_policy).put(routes::admin::set_tier_policy),
        )
        .route(
            "/v1/admin/restore",
            get(routes::admin::list_restores).post(routes::admin::create_restore),
        )
        .route("/v1/admin/restore/{id}", get(routes::admin::get_restore))
        .route(
            "/v1/admin/tier-pins/{id}",
            axum::routing::delete(routes::admin::release_pin),
        )
        .route(
            "/v1/admin/tier-pins/{id}/extend",
            post(routes::admin::extend_pin),
        )
        // Admin data purge. Deployment-admin for the same reason as the tier
        // routes: it is irreversible, and in a multi-tenant deployment a single
        // tenant's admin must not be able to destroy signal data.
        //
        // `preview` returns 202 and a job the client polls — counting three
        // partitioned tables on a badly-polluted app is exactly the workload
        // that would sit past the 30s TimeoutLayer, and that app is the one
        // that most needs purging.
        .route(
            "/v1/admin/purge",
            get(routes::purge::list_jobs).post(routes::purge::preview),
        )
        .route("/v1/admin/purge/{id}", get(routes::purge::get_job))
        .route("/v1/admin/purge/{id}/confirm", post(routes::purge::confirm))
        .route("/v1/admin/purge/{id}/cancel", post(routes::purge::cancel))
        // Ingest failures. Deployment-wide for the same reason as the tier
        // routes above, plus one of its own: the dominant failure never
        // decoded, so it carries no org_id to scope an org-level grant against.
        .route("/v1/admin/ingest-failures", get(routes::failures::list))
        .route(
            "/v1/admin/ingest-failures/{id}",
            axum::routing::delete(routes::failures::drop_group),
        )
        .route(
            "/v1/admin/ingest-failures/{id}/payloads",
            get(routes::failures::payloads),
        )
        .route(
            "/v1/admin/ingest-failures/{id}/retry",
            post(routes::failures::retry),
        )
        // A JSON API body never legitimately reaches megabytes; the artifact
        // routes below are merged separately with their own raised limit.
        .layer(DefaultBodyLimit::max(API_JSON_BODY_LIMIT))
        .merge(artifact_routes)
        .layer(cors)
        // Shed load before it reaches a handler: an unbounded queue of slow
        // requests otherwise pins connections and pool slots indefinitely.
        //
        // Order matters, and it is the reverse of how it reads: the LAST layer
        // added is the OUTERMOST. The timeout must therefore be added last so it
        // wraps the concurrency limit. `ConcurrencyLimit` applies backpressure by
        // parking in `poll_ready` rather than shedding, so with the two swapped
        // the wait for a permit sat *outside* the timeout and was never bounded
        // — exactly the unbounded, untimed queue this is meant to prevent.
        .layer(ConcurrencyLimitLayer::new(MAX_INFLIGHT_REQUESTS))
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        // The API only ever returns JSON, so a maximally restrictive CSP is
        // safe and stops a sniffed response from executing as a document.
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(%addr, "sauron-api listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    // `into_make_service_with_connect_info` is required for the `ConnectInfo`
    // extractor the auth rate limiters use to key on the peer address.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// ALWAYS 200. `packaging/rpm/SETUP.md` documents `curl -fsS .../health` and
/// `tests/http_env_scoping.rs` polls it for readiness; both read a non-2xx as
/// "the API is down", which a stalled reaper is not. The task list is the signal;
/// the status code is not.
async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "tasks": tasks::snapshot(),
    }))
}
