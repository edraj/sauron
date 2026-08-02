# S0 — Email foundation

Date: 2026-08-01
Status: designed; not implemented

## Problem

Sauron can send email, but only one kind of email, and only to an org.

`lettre` appears in exactly one file: `backend/crates/sauron-alerts/src/deliver.rs`.
There, `deliver_email` is a private function that hardcodes
`ContentType::TEXT_PLAIN`, the subject `[Sauron/{severity}] {title}` and the
footer `— Sauron alerting`, and takes its recipients from a per-org
`notification_channels` JSONB row. None of that can carry a password-reset link
to one person. `render::html_escape` — the single piece of escaping in the
repository — is `fn`, not `pub fn` (`render.rs:133`), so it is unreachable from
any new code without either widening it or copying it.

Four gaps have to close before any product email can be written:

- **No deployment-level relay.** SMTP credentials live in `notification_channels`,
  encrypted per org. A user who belongs to no org — or whose org's admin should
  not learn that they requested a password reset — has no path to a mailbox.
- **No link base.** `DASHBOARD_URL` does not exist. Today the dashboard learns
  the API's URL (`API_BASE_URL` in `dashboard.env`) and never the reverse, and
  the shipped nginx serves only the SPA without proxying the API, so the two
  origins genuinely differ. Nothing can build a clickable URL.
- **No way off the request path.** `deliver_email` awaits the relay. A handler
  that calls it inherits an unbounded wait: the only bound today is
  `AsyncSmtpTransport::builder_dangerous(..).timeout(..)`, which lettre applies
  *per socket operation*, and `sauron_monitor_core::ssrf::resolve_checked` calls
  `tokio::net::lookup_host` with no timeout at all.
- **No observability.** Nothing anywhere can assert that a message left the
  process. There is no signal an integration test can read.

S0 ships the plumbing and zero senders. The only thing it can prove end to end
is that a hand-written `MailContent` reaches `status='sent'`.

## Decisions

| Question | Decision | Rejected |
|---|---|---|
| Where does mail code live? | A new leaf crate `backend/crates/sauron-mail` owning transport, templates and the `{{var}}`/escaping primitives. `sauron-alerts` drops `lettre` and depends on it | A module inside `sauron-alerts` — leaves a crate named for org alerting owning user-facing product mail, and forces a future digest worker to link the whole alerting engine to send one message |
| How does mail leave the request path? | A durable `mail_outbox` table. The handler renders and INSERTs one row; a supervised drain loop in `sauron-api` sends it | Bare `tokio::spawn` (dies with the process; a lost reset mail is unrecoverable for a user who has already spent their rate-limit bucket). Redis (`RedisStore` sets `response_timeout(None)`, so a command against a dead Redis sits through reconnect for 9–19s — on the auth path) |
| Which process drains? | `sauron-api` only | `sauron-alerts` — its systemd unit is preset-driven and not enabled by default, so password reset would silently do nothing on a minimal install |
| Text part: derived or rendered? | Rendered separately. Two independent functions over one `MailContent` | Tag-stripping — entities survive as `&amp;`, the CTA href vanishes leaving a bare label, table scaffolding leaves ragged whitespace |
| Rendered at enqueue or at send? | At enqueue | Store a payload and render at send — moves a fallible pure step into the retry loop, and does not avoid credential-at-rest because the payload holds the token anyway |
| Is cleartext SMTP allowed? | Only when the relay resolves to **loopback**. Checked syntactically at boot and structurally at connect | Gating it on `SMTP_ALLOW_PRIVATE` — that flag would then be the only consent gate for shipping reset links across a LAN, and it is a flag an operator may have set for an unrelated webhook |
| Does `SMTP_ALLOW_PRIVATE` inherit `ALERTS_ALLOW_PRIVATE`? | **No.** Default `false`, read on its own | Inheriting it — `ALERTS_ALLOW_PRIVATE` unlocks private delivery for *user-supplied* webhook URLs, a strictly larger surface. Declaring a LAN Slack endpoint is not declaring anything about the relay |
| Does `SMTP_SINK` inherit `SAURON_DEV`? | **No.** Default `false`, read on its own | Inheriting it — `SAURON_DEV=1` exists to get past a `JWT_SECRET` complaint, and an operator who sets it during a stalled first boot must not thereby convert every reset link into a log line |
| What does the API return when SMTP is broken? | Nothing **account-specific** ever changes. Unconfigured ⇒ `AppState.mail` is `None`, one INFO line at boot, everything else serves normally. An unauthenticated route's response is identical either way; only a route already behind a permission may admit that the relay is missing | 503 on an unauthenticated route — a free config-state oracle for an anonymous caller. Refusing to boot — bailing in `from_env` once took down `sauron-ingest` and `sauron-tier` |

## Non-goals

- Any actual email. No password-reset mail (S1), no digest or per-user
  notification mail (S3), no invitations. S0 defines their kinds and sends none
  of them.
- Every dashboard change. No page, no route, no Sidebar entry, no `models/*.ts`.
  S0 adds no permission, so none of the five coordinated RBAC edits apply and
  `perm::ALL` stays `[&str; 27]`.
- Changing what an alert email looks like. Alert mail stays `text/plain` with the
  `[Sauron/{severity}] {title}` subject and the `— Sauron alerting` footer.
- An operator-facing "send a test email" endpoint. It needs a route, a permission
  decision and UI.
- Per-org or per-user override of the relay. Routing a user's reset mail through
  their org's SMTP channel leaks their existence to whoever administers an
  arbitrary org they belong to, and strands users who belong to none.
- Bounce handling, `List-Unsubscribe`, DKIM/SPF signing, attachments, embedded
  images. S3 will need `List-Unsubscribe` for digests.
- Graceful shutdown. No server binary in this repo handles SIGTERM; adding it is
  a consistent-across-all-binaries change or it is nothing.

---

## 1. Migration `2026-08-01-000034_mail_outbox`

S0 consumes migration **000034 and only 000034**. Last on disk is
`2026-07-30-000033_env_per_project`. `run_pending_migrations`
(`backend/crates/sauron-db/src/lib.rs:30`) orders by the full directory version
string, i.e. **lexicographically by date first** — so the date prefix must be
monotone non-decreasing with NN. A slice that lands late uses the date of its
landing, never its authoring date. Downstream allocation is pinned in the
programme document as S2=000035, S1=000036, S3=000037, S4=000038-000040 and
S5=000041-000043. Numbers follow **build** order, and S2 is built before S1.

```sql
CREATE TABLE mail_outbox (
  id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  kind           TEXT NOT NULL,
  recipient      TEXT NOT NULL,
  recipient_key  TEXT NOT NULL,
  subject        TEXT NOT NULL,
  body_text      TEXT NOT NULL,
  body_html      TEXT NOT NULL,
  status         TEXT NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending','sending','sent','failed','sink')),
  attempts       INT NOT NULL DEFAULT 0,
  max_attempts   INT NOT NULL DEFAULT 8,
  next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at     TIMESTAMPTZ NOT NULL DEFAULT now() + interval '1 hour',
  last_error     TEXT,
  user_id        UUID REFERENCES users(id) ON DELETE SET NULL,
  created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
  sent_at        TIMESTAMPTZ
);

CREATE INDEX mail_outbox_due_idx     ON mail_outbox (next_attempt_at) WHERE status = 'pending';
CREATE INDEX mail_outbox_stuck_idx   ON mail_outbox (updated_at)      WHERE status = 'sending';
CREATE INDEX mail_outbox_dedup_idx   ON mail_outbox (kind, recipient_key, created_at DESC);
CREATE INDEX mail_outbox_created_idx ON mail_outbox (created_at);
```

`down.sql` is `DROP TABLE IF EXISTS mail_outbox;`, which takes the four indexes
with it. It drops **only** what this `up.sql` created — migration 21's `down.sql`
calls out migration 20 dropping pre-existing indexes as the mistake not to repeat.

Notes that belong in the `up.sql` prose header, because each is a decision a
reader will otherwise reverse:

- **No `org_id`.** Transactional mail is addressed to a person, and a user who
  belongs to no org still has to be reachable. That is precisely why the per-org
  `notification_channels` model cannot serve this.
- **`kind` has no CHECK**, deviating from the house TEXT+CHECK rule with a stated
  reason: the value set keeps growing after S0 lands — S0 seeds the four kinds it
  can name today — and the slice that adds the fifth must not also have to widen a
  CHECK on a table holding live credentials, which is a migration nobody wants to
  rush. The authority is `sauron_mail::MailKind`, which also owns each kind's
  dedup window — two things that must change together, so splitting one of them
  into SQL guarantees drift.
- **`status` does have a CHECK.** S0 owns every value it can take.
- **A pending row holds a live credential.** Before this table, a read-only
  database compromise — a backup, a replica, an SQL injection — could not take
  over an account: password hashes are Argon2 and refresh tokens are stored
  hashed. A `body_html` containing a working reset URL hands over accounts
  outright. The bound on that exposure is **min(delivery time, the row's own
  `expires_at`)**: the body is blanked the moment the row reaches `sent`/`sink`,
  and the hygiene task blanks *any* row's body once it is past `expires_at`,
  regardless of status (§6). Nothing recoverable is lost, because `claim_due_mail`
  refuses an expired row anyway — a body that survived that instant could never be
  delivered, only stolen. Keying the sweep off the row rather than off a flat hour
  is what keeps that true for both of S1's token lifetimes at once: a 24-hour
  admin-initiated reset link must not be scrubbed at the one-hour mark while its
  token is still live and the row is still the only thing an operator can requeue.
- **`expires_at` is what stops a stale message being delivered on revoked
  authorization.** `claim_due_mail` will not pick up an expired row. Reset mail
  is worthless after the token dies; a digest rendered at enqueue is a snapshot
  of grants that may since have been revoked. **Every enqueue sets it explicitly**,
  from the lifetime of whatever the body carries — the column DEFAULT is only a
  backstop for a row an operator writes by hand, and a reader who takes the one
  hour there for the real policy will scrub 24-hour admin reset mail early.
- **`max_attempts` is a column, not a config knob**, so an operator can bump one
  stuck row. Combined with the fact that `mark_mail_failed` does *not* blank the
  body, a failed row can be genuinely resurrected for as long as its body survives
  — that is, up to its own `expires_at`:
  `UPDATE mail_outbox SET status='pending', attempts=0, next_attempt_at=now(), expires_at=now()+interval '10 minutes' WHERE id=…`.
- **`recipient_key`** is the parsed, lowercased envelope address. It exists so the
  per-recipient cap cannot be walked around: `register` validates addresses with
  `req.email.contains('@')` alone (`auth.rs:195`), and lettre's `Mailbox::from_str`
  discards the unparsed remainder, so `victim@corp.test`,
  `victim@corp.test ` and `victim@corp.test <x>` are three `users.email` rows that
  deliver to one mailbox.

`backend/crates/sauron-db/src/schema.rs` takes three **hand** edits — the diesel
CLI must never run, and `backend/diesel.toml` deliberately omits `file =`:
a `diesel::table! { mail_outbox (id) { … } }` block after `alert_events`;
`diesel::joinable!(mail_outbox -> users (user_id));`; and `mail_outbox,` in
`allow_tables_to_appear_in_same_query!`. That is **+1 table**, stated as a delta
and never as a total: four later slices add tables to this same file, so any
absolute count written here is wrong for every slice but the first to land.

`models.rs` gains `MailOutbox` deriving `(Clone, Queryable, Selectable, QueryableByName)`
— `QueryableByName` because the claim is a `sql_query` with `RETURNING *`. It
**deliberately derives neither `Serialize` nor `Debug`**. No `Serialize`, so a
pending row's body cannot reach an API view struct by someone adding
`#[derive(Serialize)]` upstream. A hand-written `impl Debug` prints id/kind/
recipient/status/attempts and literally `body_text: <redacted>` — following the
`SecretCipher` precedent at `sauron-alerts/src/crypto.rs:19` — because one
`warn!(row = ?r, …)` in a drain loop would otherwise write live credentials to
the journal. `NewMailOutbox<'a>` derives `Insertable` only, same reason.
`models.rs` needs no import edit: it does `use crate::schema::*;` at line 13.

## 2. `backend/crates/sauron-mail`

Picked up automatically by `members = ["crates/*", "bins/*"]`. Dependencies:
`sauron-core`, `sauron-monitor-core` (for `ssrf::resolve_checked`), `lettre`,
`tokio`, `tracing`, `thiserror` — all `{ workspace = true }`. **Not** `sauron-db`:
keeping the data layer out is what lets this stay a leaf anything can link.

Three Cargo edits, not two:

| File | Change |
|---|---|
| `backend/Cargo.toml` | `sauron-mail = { path = "crates/sauron-mail" }` in the `# --- internal crates ---` block |
| `backend/crates/sauron-alerts/Cargo.toml` | drop `lettre`, add `sauron-mail` |
| `backend/bins/sauron-api/Cargo.toml` | **add `sauron-mail`** — `mail.rs` names `SmtpParams`, `Branding` and `sauron_mail::render` directly, and today the crate lists nine internal deps and not this one |

lettre's workspace entry is untouched (same version, same five features, still
rustls-only, no OpenSSL) — it moves one crate down. The set of binaries linking
it is unchanged: `sauron-api` already linked it transitively.

### Public surface

```rust
pub mod kind;  pub mod smtp;  pub mod template;  pub mod text;

pub use kind::MailKind;
pub use smtp::{is_transient, normalize_recipient, send, MailBody, MailError,
               OutgoingMail, SmtpClient, SmtpParams};
pub use template::{render, Branding, Cta, MailContent, RenderedMail, TemplateError};
pub use text::{html_escape, substitute};
// Single home for both: sauron-core. sauron-mail depends on sauron-core, so
// defining them here too would be a second, incompatible type — and Config
// cannot depend on sauron-mail without a cycle.
pub use sauron_core::config::{SmtpSettings, SmtpTls};
```

The module doc states the split out loud: **this crate knows how to compose and
transmit a message; it does not know what a user is, where a message queues, or
when to retry.**

### `text.rs` — moved, not copied

`pub fn substitute(&str, &BTreeMap<String,String>) -> String` and
`pub fn html_escape(&str) -> String` move here verbatim from
`sauron-alerts/src/render.rs:106` and `:133`. Their existing unit tests
(`substitute_replaces_known_and_blanks_unknown`,
`substitute_preserves_multibyte_text`, and the escaping half of
`matrix_html_escapes_user_content`) move with them, so the move is provably
behaviour-preserving. `sauron_alerts::render` keeps the public path alive with
`pub use sauron_mail::text::substitute;` and takes `html_escape` via `use`, so
`AlertContext::message` and the Matrix renderer are untouched.

### `kind.rs`

```rust
pub enum MailKind { PasswordReset, NotificationDigest, PersonalNotification, SmtpTest }
impl MailKind {
    pub fn as_str(&self) -> &'static str;
    /// Per-recipient suppression window. Zero disables it.
    pub fn dedup_window(&self) -> Duration;
}
```

**There is deliberately no `ttl()`.** How long a rendered body stays deliverable
is a property of what the body carries, not of its kind: S1 mints a one-hour
token on the self-service path and a 24-hour one on the admin-initiated path from
the same `PasswordReset` kind. A per-kind constant would mark the second
`expired before delivery` an hour in, blank its body, and destroy the manual
requeue path — while the token it carried stayed valid for another 23 hours. So
`expires_at` is an argument to `enqueue` (§6), and the column below records only
what the callers are expected to pass.

| Kind | `as_str` | Expiry the caller passes | Dedup window | Why |
|---|---|---|---|---|
| `PasswordReset` | `password_reset` | the token's own lifetime — 1 h self-service, 24 h admin-initiated | 5 min | The backoff ladder (≈45 min) fits inside even the shorter of the two. 5 min is short enough not to defeat a user who genuinely did not receive the first mail |
| `NotificationDigest` | `notification_digest` | 15 min | 15 min | The body is a snapshot of authorization state at enqueue; 15 minutes bounds how stale a delivered digest can be (§9) |
| `PersonalNotification` | `personal_notification` | 15 min | **0** | Same snapshot argument as the digest. The window is zero because a non-zero one here throws S3's mail away: S3 caps a user at 20 notifications an hour, so a 15-minute window would suppress roughly 16 of them, and suppression is indistinguishable from success — `enqueue` returns the same `Ok(None)` it returns for a deliberate discard. S3 already suppresses duplicates twice, with a Redis `SET NX EX` per `(subscription, dedup_key)` and a partial unique index; a third layer this far downstream can only lose mail |
| `SmtpTest` | `smtp_test` | 5 min | 0 | An operator clicking "test" twice must get two mails |

### `smtp.rs`

`SmtpParams` replaces the old `EmailDest` + `DeliverOpts` pair:

```rust
pub struct SmtpParams {
    pub host: String, pub port: u16,
    pub username: Option<String>, pub password: Option<String>,
    pub tls: SmtpTls, pub allow_private: bool,
    pub op_timeout: Duration,      // per socket operation (lettre)
    pub total_deadline: Duration,  // whole send, DNS included
    pub sink: bool, pub sink_log_body: bool,
}
impl SmtpParams { pub fn from_settings(s: &SmtpSettings) -> Self }
```

It does **not** derive `Debug`. A hand-written impl prints `password: <redacted>`
(username unredacted is fine), with the failure mode named in a comment: this is
the copy that lives inside the drain loop, and it is the struct a contributor
debugging a delivery failure will reach for with `debug!(?params, …)`. `clippy`
would not object to a `#[derive(Debug)]` here and it would bypass every
redaction in `sauron-core`.

```rust
pub enum MailBody { Text(String), Alternative { text: String, html: String } }
pub struct OutgoingMail {
    pub from_address: String, pub from_name: Option<String>,
    pub to: Vec<String>, pub reply_to: Option<String>,
    pub subject: String, pub body: MailBody,
}
```

`Text` builds `.header(ContentType::TEXT_PLAIN).body(text)` — byte-identical to
today's alert mail. `Alternative` builds
`.multipart(MultiPart::alternative_plain_html(text, html))`, present in lettre
0.11.22 under the already-enabled `builder` feature, no Cargo change. The From
mailbox is `Mailbox::new(from_name.clone(), from_address.parse::<Address>()?)`
so lettre does the RFC 2047 display-name encoding rather than us `format!`-ing a
header.

Two entry points, because the drain and the alerting path want different things:

```rust
/// Build a transport once and reuse it for a batch.
pub async fn SmtpClient::connect(params: &SmtpParams) -> Result<SmtpClient, MailError>;
pub async fn SmtpClient::send(&self, mail: &OutgoingMail) -> Result<(), MailError>;

/// One-shot: connect, send, drop. What `sauron-alerts` calls.
pub async fn send(params: &SmtpParams, mail: &OutgoingMail) -> Result<(), MailError>;
```

The transport is the expensive part. `deliver_email` rebuilds
`AsyncSmtpTransport` on every call, so every message pays a DNS lookup, a TCP
connect, a full TLS handshake and an AUTH round trip. That is tolerable for one
alert; at S3's digest volume it is 10k connection+AUTH cycles, which postfix's
`smtpd_client_connection_rate_limit` and every hosted relay will throttle.
`SmtpClient` exists so a batch pays it once.

**Resolution and the loopback rule**, inside `connect`:

```rust
// Always resolve and always pin, so the value that was validated is the value
// dialed. TLS still validates the certificate against the configured hostname,
// so pinning costs no authenticity. Today `deliver_email` skips resolution
// entirely when allow_private is set, which quietly drops the DNS-rebinding pin
// on exactly the deployments most likely to need it.
let addrs = resolve_checked(&p.host, p.allow_private || p.tls == SmtpTls::None)
    .await.map_err(classify_resolve_error)?;
if p.tls == SmtpTls::None && !addrs.iter().all(|a| a.ip().is_loopback()) {
    return Err(MailError::Blocked(format!(
        "SMTP_TLS=none requires SMTP_HOST to resolve to loopback; {} resolves to {} \
         — use SMTP_TLS=starttls, or put a local relay in front", p.host, addrs[0].ip())));
}
```

`SmtpTls` maps to `Tls::Wrapper(TlsParameters::new(host)?)` /
`Tls::Required(..)` / `Tls::None`. `sauron-alerts` only ever constructs the first
two, so its "never cleartext" guarantee is preserved exactly.

**Total deadline.** The whole of `connect` + `send` — DNS included — runs inside
one `tokio::time::timeout(params.total_deadline, ...)`. `total_deadline` is
`SMTP_TIMEOUT_MS * 3` clamped to a hard ceiling of 60s. Without it the worst case
is unbounded: lettre applies its timeout per socket operation (connect, EHLO,
STARTTLS, AUTH, MAIL FROM, RCPT TO, DATA, end-of-data, QUIT) and
`resolve_checked`'s `lookup_host` has no timeout at all. This also fixes a latent
bug on the **shipped** alerting path, where a tarpitting relay can hold one
delivery indefinitely today.

### `MailError` and the string contract

```rust
#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("invalid from address: {0}")]  InvalidFrom(String),
    #[error("invalid recipient: {0}")]     InvalidRecipient(String),
    #[error("smtp tls setup failed: {0}")] Tls(String),
    #[error("{0}")]                        Dns(String),
    #[error("{0}")]                        Blocked(String),
    #[error("email build failed: {0}")]    Build(String),
    #[error("smtp send failed: {0}")]      Send(String),
    #[error("smtp send failed: {0}")]      Rejected(String),
    #[error("smtp send failed: deadline exceeded after {0}ms")] DeadlineExceeded(u64),
}
```

`Rejected` is returned when lettre's SMTP error exposes a 5xx `Code` with
`PermanentNegativeCompletion` severity. Its Display is **identical** to `Send`'s.
That is deliberate: `test_channel` (`routes/notifications.rs:282-286`) returns
this string verbatim as the `error` field of
`POST /v1/notification-channels/{id}/test`, and persists it to `alert_events`.
An earlier draft added a `(permanent)` infix; it would have changed a
user-visible string while claiming byte-for-byte parity. The drain distinguishes
the two by **variant**, which is free.

**The retry predicate is load-bearing and invisible.** `sauron_alerts::engine`
decides whether to retry with four `e.contains(..)` substring checks including
`"smtp send failed"` (`engine.rs:209-213`). Moving the transport into another
crate makes that string a `thiserror` attribute in a different file, and
"improving" its wording silently stops every alert email from retrying with
nothing failing to compile. So the predicate moves too:

```rust
pub fn is_transient(msg: &str) -> bool  // in sauron-mail, same four substrings
```

`sauron-alerts` calls `sauron_mail::is_transient(&e)`, and a test in
`sauron-alerts` pins each `MailError` variant's Display against it, so the
coupling is visible from both sides.

`classify_resolve_error` splits `resolve_checked`'s two error strings, whose
first branch is literally `DNS resolution failed: {e}` (`ssrf.rs:77`) — a
resolver timeout, textbook transient — and whose second is
`target {host} resolves to a blocked address`. **Anything it does not recognise
maps to `Dns`, i.e. transient.** That is the safe direction: if the upstream
wording drifts, mail retries and eventually fails out, rather than being
classified permanent on the first hiccup.

### Error classification, both consumers

| Variant | Drain (by variant) | `is_transient` (alerting, by string) |
|---|---|---|
| `Send` | retry | true |
| `DeadlineExceeded` | retry | true |
| `Dns` | **retry** | false |
| `Tls` | **retry** | false |
| `Rejected` | **permanent** | true — unchanged from today |
| `InvalidFrom`, `InvalidRecipient`, `Build`, `Blocked` | permanent | false |

The two columns disagree on purpose. The drain owns its own ladder and can
afford to burn 45 minutes on a genuinely broken relay; alerting keeps its string
predicate byte-compatible so its behaviour is unchanged. Classifying `Dns`/`Tls`
as permanent — as the first draft did — meant a 20-second resolver hiccup during
a nightly restart marked every row in that window `failed` after one attempt.

`normalize_recipient(&str) -> Result<String, MailError>` parses with
`Address::from_str`, rejects rather than stores anything unparseable, and returns
the lowercased result for `recipient_key`. Delegating the entire header-injection
barrier to a transitive dependency that discards its unparsed remainder is not a
barrier.

## 3. Templates

### Content model

```rust
pub struct MailContent { pub subject: String, pub heading: String,
                         pub paragraphs: Vec<String>, pub cta: Option<Cta>,
                         pub footnotes: Vec<String> }
pub struct Cta { label: String, url: String }
impl Cta { pub fn new(label: impl Into<String>, url: impl Into<String>)
               -> Result<Cta, TemplateError> }   // rejects anything not http:// or https://
pub struct Branding { pub product_name: String, pub dashboard_url: Option<String>,
                      pub footer: String }
impl Branding { pub fn link(&self, hash_path: &str) -> Result<String, TemplateError> }
```

`Branding::link` returns `format!("{base}/#{hash_path}")` — the base is already
trailing-slash-stripped by `Config` — and errors `TemplateError::NoDashboardUrl`
when `dashboard_url` is `None`. **This is where "any email containing a link
requires `DASHBOARD_URL`" is actually enforced.** The `#` is load-bearing: the
dashboard is `svelte-spa-router`, so S1's link is
`https://host/#/reset-password?token=…`.

`Cta::new`'s scheme check is belt-and-braces against a `javascript:` href even
though every URL we build comes from the scheme-validated `DASHBOARD_URL`.

### `render(b: &Branding, c: &MailContent) -> Result<RenderedMail, TemplateError>`

Returns `RenderedMail { subject, text, html }`. **Two independent renderers over
one struct; nothing ever strips tags.**

`render_html` builds a `BTreeMap<String,String>` whose values are *already*
`html_escape`d and hands it to `substitute(LAYOUT_HTML, &escaped)`. The local is
named `escaped` and carries a comment that `substitute` copies bytes and escapes
nothing — an unescaped value here is stored XSS in an inbox. Repeated blocks
(paragraphs, footnotes) cannot be looped by `substitute`, so each renders through
its own one-placeholder const into a `String` accumulator and is injected as a
single pre-escaped var.

`render_text` emits heading, blank line, paragraphs separated by blank lines,
then `"{label}:\n{url}"` for the CTA, then footnotes, then `"—\n{product_name}"`.
No escaping anywhere, because it is not markup.

The subject goes through `sanitize_header(s) -> String`, which replaces CR and LF
with a space and truncates to 200 chars. A user-supplied fragment — an app name,
a display name — in a subject is exactly how SMTP header injection happens.

### `LAYOUT_HTML`

One `const LAYOUT_HTML: &str` raw string, doc-commented with the client that
motivated each constraint. **The first comment line is the escaping rule:
`html_escape` replaces exactly `& < > "` and does not escape `'`, so every
attribute in this layout is double-quoted. Adding a single-quoted attribute
introduces attribute breakout.**

Structure, outer to inner:

- `<!doctype html><html lang="en"><head>` with `<meta charset>`, viewport,
  `<meta name="color-scheme" content="light dark">`,
  `<meta name="supported-color-schemes" content="light dark">`,
  `<title>{{subject}}</title>`, and a `<style>` block holding **only**
  `:root { color-scheme: light dark; }` plus one
  `@media (prefers-color-scheme: dark)` rule overriding `.s-page`, `.s-card`,
  `.s-h1`/`.s-body`, `.s-muted`/`.s-foot`.
- `<body style="margin:0;padding:0;background-color:#f4f5f7;-webkit-text-size-adjust:100%">`,
  then a hidden preheader span (`display:none;font-size:1px;…;overflow:hidden`)
  carrying `{{preheader}}` — the first paragraph — followed by `&#8199;&#65279;`,
  so the inbox preview is not garbage.
- **Tables only, never divs.** Outlook 2016+ renders through Word and ignores
  flex and most margins. An outer
  `<table role="presentation" width="100%" cellpadding=0 cellspacing=0 border=0 style="border-collapse:collapse;mso-table-lspace:0pt;mso-table-rspace:0pt">`
  → `<td align="center" style="padding:32px 12px">` → an inner
  `<table role="presentation" width="600" style="width:100%;max-width:600px;border-collapse:collapse">`.
  The `width` **attribute** is for Word, which ignores `max-width`; `max-width` is
  for everyone else; `width:100%` keeps it fluid on mobile.
- A muted text wordmark `{{product}}` — **no `<img>`**, because remote images are
  blocked by default in Outlook and Gmail, so a logo is an empty box in most
  inboxes.
- The card: `<td class="s-card" style="background-color:#ffffff;border:1px solid #e5e7eb;border-radius:10px;padding:32px">`
  containing `<h1 class="s-h1" style="margin:0 0 16px;font-size:22px;line-height:1.3;font-weight:600;color:#111827">{{heading}}</h1>{{paragraphs}}{{cta}}{{footnotes}}`.
- Footer `<td class="s-foot" style="font-size:12px;color:#9ca3af">{{footer}}</td>`.

Every element carries an inline `style=` with the same stack
`-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif`. Sub-templates:
`P_HTML` at 15px/#374151; `FOOTNOTE_HTML` at 13px/#6b7280 plus
`word-break:break-all` (the raw-URL fallback is long); `CTA_HTML` is the
bulletproof-button pattern — a one-cell `<table>` with `bgcolor="#3b82f6"` and
`border-radius:8px` wrapping an `<a>` with `display:inline-block;padding:12px 26px`.
Accent `#3b82f6` is deliberately the same blue as `Severity::Info::hex()`
(`render.rs:40`).

**Dark mode is best-effort and the design says so.** Gmail strips
`prefers-color-scheme`; Outlook.com rewrites CSS; Apple Mail and some Android
clients force-invert on their own. The promise is not a pixel-matched dark
variant — it is that the *inline* palette is legible whether or not any of that
happens. Dark ink on white reads correctly either way.

**`substitute` can silently eat the stylesheet.** It treats any `{{` as a
placeholder opener and renders an unknown key as an empty string, so two adjacent
`{` anywhere in the CSS would delete everything up to the next `}}` — no error,
no test failure, an email that still sends and merely looks broken. The layout
avoids it by construction, but nobody will maintain that property by hand. The
placeholder-key-set test (§10) is the guard.

## 4. Config

`SmtpSettings`, `SmtpTls` and the accessors live in
`backend/crates/sauron-core/src/config.rs`.

```rust
pub struct SmtpSettings { pub host: String, pub port: u16,
    pub username: Option<String>, pub password: Option<String>,
    pub from_address: String, pub from_name: String,
    pub tls: SmtpTls, pub allow_private: bool,
    pub timeout_ms: u64, pub sink: bool }
pub enum SmtpTls { Implicit, StartTls, None }
```

`SmtpSettings` derives `Clone` but **not** `Debug`; a hand-written impl prints
`password: <redacted>`, so adding this field to the `Debug`-deriving `Config`
does not put an SMTP password one `debug!("{cfg:?}")` away from the journal.

`Config` gains private `smtp: Result<SmtpSettings, String>` and private
`dashboard_url: Result<String, String>`, plus `pub dev_mode: bool` (today
`SAURON_DEV` is a throwaway local at `config.rs:143`; S0 owns the promotion, and
S1 consumes it rather than re-adding it). Accessors mirror `require_jwt_secret`
(`config.rs:117`) exactly:

```rust
pub fn require_smtp(&self) -> anyhow::Result<&SmtpSettings>
pub fn require_dashboard_url(&self) -> anyhow::Result<&str>
```

**`from_env` never bails for either.** `sauron-ingest` and `sauron-tier` read the
same `Config` and were previously taken down by exactly that mistake — the
documented reason `jwt_secret` is a recorded `Result` rather than a `?`.

`build_smtp(...) -> Result<SmtpSettings, String>` is a free function taking the
already-read values rather than reading env itself, because env-var tests are
process-global and race under `cargo test`. Rules, in order:

1. `sink` and no host → `Ok` with `host = "(sink)"`, `from_address = SMTP_FROM` or `sauron@localhost`.
2. No host → `Err("SMTP_HOST is not set; transactional email is disabled. Set SMTP_HOST/SMTP_FROM, or SMTP_SINK=1 to log mail instead of sending it.")`
3. No from → `Err("SMTP_FROM is required when SMTP_HOST is set")`.
4. `from` must contain exactly one `@`, no whitespace, no CR/LF, non-empty both
   sides — a cheap check that turns a runtime "invalid from address" into a
   boot-time message. Real parsing still happens in lettre.
5. `tls_raw`: `implicit`|`smtps` → `Implicit`; `starttls`|`required` → `StartTls`;
   `none`|`plain` → `None`; anything else → `Err` naming the accepted values.
   Unset defaults to `if port == 465 { Implicit } else { StartTls }`, the same
   rule `channel.rs` uses for `implicit_tls`.
6. `SmtpTls::None` and `host` is not one of `localhost`, `127.0.0.1`, `::1`,
   `[::1]` → `Err("SMTP_TLS=none sends the SMTP password and password-reset links in cleartext and is only accepted for a relay on this host; SMTP_HOST={host} is not loopback")`.
   This is the boot-time half; `SmtpClient::connect` enforces the same rule
   against the *resolved* address, which is what makes it structural rather than
   syntactic. Both exist: the boot check is loud and early, the connect check
   survives a `localhost` that has been pointed elsewhere.
7. `timeout_ms` clamps to `1_000..60_000`, matching `AlertEngine::new`.

`Config` also loses its `#[derive(Debug)]` in favour of a hand-written impl that
prints `<redacted>` for `jwt_secret`, `notify_secret_key`, `smtp`,
`database_url`, `redis_url` and `symbols_redis_url`. Nothing `Debug`-prints
`Config` today, so this is a latent leak — but S0 is the slice that adds the most
tempting one, and a single `debug!(?cfg)` added during an incident would dump the
Postgres password, the JWT signing key and the SMTP password into the journal at
once.

## 5. Repository functions

All SQL lives in `backend/crates/sauron-db/src/repo.rs`. None uses
`conn.transaction` (MSRV 1.82); every one is a single statement.

```rust
pub async fn enqueue_mail(conn, row: NewMailOutbox<'_>, ttl_secs: i64,
                          dedup_secs: i64, commit: bool)
    -> QueryResult<Option<Uuid>>;
pub async fn claim_due_mail(conn, batch: i64) -> QueryResult<Vec<MailOutbox>>;
pub async fn heartbeat_mail(conn, id: Uuid) -> QueryResult<usize>;
pub async fn mark_mail_sent(conn, id: Uuid, attempts: i32, sink: bool) -> QueryResult<usize>;
pub async fn mark_mail_failed(conn, id: Uuid, attempts: i32, error: &str, permanent: bool)
    -> QueryResult<usize>;
pub async fn requeue_stuck_mail(conn, stale_secs: i64) -> QueryResult<usize>;
pub async fn expire_stale_mail(conn) -> QueryResult<usize>;
pub async fn blank_expired_mail_bodies(conn) -> QueryResult<usize>;
pub async fn prune_mail_outbox(conn, older_than_days: i64, batch: i64) -> QueryResult<usize>;
pub async fn mail_outbox_depth(conn) -> QueryResult<(i64, Option<i64>)>;
```

**`enqueue_mail`** is one `INSERT … SELECT … WHERE` with the per-recipient
backstop inline:

```sql
INSERT INTO mail_outbox (kind, recipient, recipient_key, subject, body_text,
                         body_html, user_id, expires_at)
SELECT $1, $2, $3, $4, $5, $6, $7, now() + make_interval(secs => $8)
 WHERE $10
   AND ($9 = 0 OR NOT EXISTS (
         SELECT 1 FROM mail_outbox
          WHERE kind = $1 AND recipient_key = $3 AND status <> 'failed'
            AND created_at > now() - make_interval(secs => $9)))
RETURNING id
```

Three things are load-bearing here.

`$8` is the caller's TTL rather than the kind's. The only code that knows how
long a body is worth delivering is whatever minted the credential inside it, and
`PasswordReset` alone spans two token lifetimes an order of magnitude apart.

`$9` is the dedup window. `out_of_scope` in an earlier draft deferred
per-recipient rate limiting as "relay-side" — it is not. The relay is the
operator's own and is what gets throttled and blacklisted; this INSERT is the
only chokepoint where a cap can live. With S1 copying login's limiter
(`sauron:auth:login:{email}` at 10/min), an unauthenticated attacker sends
roughly 14k mails a day to one victim, and the Redis limiter degrades to a
*per-process* window on any Redis blip — multiplied by replica count. The
`status <> 'failed'` term means a permanently-failed attempt does not block a
genuine retry.

`$10` is `commit`, and it is how the timing oracle is closed. The earlier draft
claimed as a hard contract that "the request path costs one INSERT plus one
non-blocking spawn … no branch on recipient existence in anything the caller can
time". That was false by construction: `enqueue` is only reachable when a user
row was found, so an existing address paid a render plus a round trip and an
unknown address paid nothing. `auth.rs:283-292` already burns `spend_dummy_verify`
purely to close this class of gap, with a comment naming "microseconds vs tens of
milliseconds" as a reliable enumeration oracle. Passing `commit = false` runs the
same statement, against the same index, over the network, and inserts nothing.
The honest claim is: **S0 removes the SMTP round trip from the request path
entirely, and makes the enqueue itself cost one round trip either way. The
residual difference is the query planner's, orders of magnitude below the network
jitter S1's callers see.** S1 must call `enqueue_or_discard`, never branch on the
lookup before it.

**`claim_due_mail`** is copied shape-for-shape from `claim_due_monitors`
(`repo.rs:6104`), the only genuinely concurrency-safe worker pattern in the
backend:

```sql
UPDATE mail_outbox SET status='sending', attempts=attempts+1, updated_at=now()
 WHERE id IN (SELECT id FROM mail_outbox
               WHERE status='pending' AND next_attempt_at <= now() AND expires_at > now()
               ORDER BY next_attempt_at FOR UPDATE SKIP LOCKED LIMIT $1)
RETURNING *
```

**`mark_mail_sent` and `mark_mail_failed` both carry
`WHERE id=$1 AND status='sending' AND attempts=$2`** and return the affected
count. Without that guard a slow drainer whose rows were reclaimed underneath it
can blank and mark `sent` a row another drainer is mid-send on, or reset it to
`pending` for a third delivery. The drain logs at `warn!` when the count is 0 —
that is a lost claim, and it should be visible.

`mark_mail_sent` sets `status = 'sent'` or `'sink'`, `sent_at=now()`, and blanks
both bodies. `mark_mail_failed` sets
`status = CASE WHEN $4 OR attempts >= max_attempts THEN 'failed' ELSE 'pending' END`,
`last_error=$3`, and
`next_attempt_at = now() + make_interval(secs => LEAST(900, 30 * POWER(2, GREATEST(attempts-1,0))::int))`
— 30/60/120/240/480/900/900, ≈45 minutes of coverage at `max_attempts` 8.
**It does not blank the body.** Blanking on failure is what made a
misclassification irreversible; the expiry sweep covers the credential instead,
and until `expires_at` passes an operator can requeue the row by hand.

**`requeue_stuck_mail`** recovers rows orphaned by a process killed mid-send —
the claim query only looks at `pending`, so nothing else ever reclaims them. It
needs three guards the first draft omitted:

```sql
UPDATE mail_outbox
   SET status = CASE WHEN attempts >= max_attempts THEN 'failed' ELSE 'pending' END,
       last_error = CASE WHEN attempts >= max_attempts
                    THEN 'orphaned mid-send ' || attempts || ' times; giving up'
                    ELSE 'orphaned mid-send; requeued' END,
       next_attempt_at = now() + make_interval(secs => LEAST(900, 30 * POWER(2, GREATEST(attempts-1,0))::int)),
       updated_at = now()
 WHERE status='sending' AND updated_at < now() - make_interval(secs => $1)
```

The give-up decision lives only in `mark_mail_failed`, which a sender that
crashed or was OOM-killed never reaches — so without `attempts >= max_attempts`
here, a row whose send reliably kills the process is claimed → orphaned →
requeued → claimed, forever. And without resetting `next_attempt_at`, the
requeued row is immediately eligible for the very next claim, bypassing the
backoff ladder entirely on exactly the path that most needs it.

**`prune_mail_outbox`** is bounded and non-blocking:

```sql
DELETE FROM mail_outbox WHERE id IN (
  SELECT id FROM mail_outbox
   WHERE status IN ('sent','failed','sink') AND created_at < now() - ($1 || ' days')::interval
   ORDER BY created_at LIMIT $2 FOR UPDATE SKIP LOCKED)
```

called in a loop until it returns 0. `prune_alert_events` (`repo.rs:6090`) is an
unbounded DELETE, but it runs in a standalone worker; this one runs inside
`sauron-api`, which serves HTTP from a **16-connection pool**. An operator
lowering `MAIL_OUTBOX_RETENTION_DAYS` after a digest run would otherwise hold one
of those 16 for minutes. The `FOR UPDATE SKIP LOCKED` is also what lets N API
instances reap concurrently without serializing on row locks — this repo has
**zero advisory locks** and S0 deliberately does not introduce the first one; the
existing claim idiom already solves the problem.

`expire_stale_mail` flips non-terminal rows past `expires_at` to `failed` with
`last_error='expired before delivery'`. `blank_expired_mail_bodies` is
`UPDATE mail_outbox SET body_text='', body_html='' WHERE (body_text <> '' OR body_html <> '') AND expires_at < now()`,
status-independent, and takes no age argument at all: the row already carries the
only deadline that means anything, and a second flat constant sitting beside it
is the drift that scrubs a live 24-hour reset link at the one-hour mark. Blanking
a row the drain is mid-send on is harmless — `claim_due_mail` returned the body
by value, so the sender is working from its own copy. Neither sweep is indexed:
the non-terminal set is small by construction and every status transition already
rewrites two partial indexes, so a fifth index costs more than these sweeps save.

## 6. `backend/bins/sauron-api/src/mail.rs`

Sits alongside `admin_storage.rs` / `symbolicate.rs` / `tier_read.rs`, the house
pattern for orchestration that is neither a route nor a repo fn.

```rust
#[derive(Clone)]           // never Debug: params holds the relay password
pub struct MailSender {
    pool: PgPool, params: Arc<SmtpParams>,
    branding: Arc<Branding>, drain_slots: Arc<Semaphore>,   // 2
}

pub async fn enqueue(&self, kind: MailKind, recipient: &str,
                     content: &MailContent, user_id: Option<Uuid>, ttl: Duration)
    -> anyhow::Result<Option<Uuid>>;
pub async fn enqueue_or_discard(&self, kind: MailKind, recipient: Option<&str>,
                                content: &MailContent, user_id: Option<Uuid>,
                                ttl: Duration)
    -> anyhow::Result<Option<Uuid>>;
pub fn nudge(&self);
pub async fn drain_once(&self) -> usize;
pub async fn hygiene(&self, retention_days: i64) -> anyhow::Result<()>;
```

**Rendering happens at enqueue, not at send.** The body is then fixed at request
time, a template error surfaces to the handler that can report it instead of
inside a retry loop that will just fail eight times, and the drain becomes pure
I/O with nothing fallible but the network. `enqueue` returns `anyhow::Error`, not
`ApiError`, so a caller like S1's `forgot_password` can swallow it and still
return its fixed 200. `Ok(None)` covers both a dedup suppression and a
`commit = false` discard, so the caller cannot distinguish them either.

**`ttl` comes from the caller, and it is the caller's credential lifetime, not a
round number.** `MailSender` writes `now() + ttl` into `expires_at`, which then
governs three separate things: whether the drain will still send the row, when
the hygiene sweep scrubs its body, and how long an operator has to requeue it by
hand. A sender that passes a lifetime shorter than the token it just minted
throws away its own recovery path; one that passes a longer one leaves a working
credential in Postgres after the token it carries is dead.

`enqueue_or_discard` renders unconditionally, normalizes
`recipient.unwrap_or("discard@invalid")` (`.invalid` is RFC 2606 reserved, so it
can never be a real mailbox even if a row somehow escaped), passes
`commit = recipient.is_some()`, and calls `nudge()` **on both branches** so the
spawn and the semaphore acquisition are paid identically.

`nudge()` spawns a detached drain that first does `try_acquire_owned()` on
`drain_slots` and returns immediately if it fails — another drain is already
running and the `SKIP LOCKED` claim will pick the row up anyway. That is what
bounds spawn under a burst without a queue.

`drain_once`:

1. Check out a connection → `requeue_stuck_mail(conn, self.stale_secs())` →
   `claim_due_mail(conn, BATCH)` → **drop the connection.** Never hold a pooled
   connection across network I/O; the pool is 16 for the whole process, and this
   is the documented reason `AlertEngine::fire` takes a pool rather than a conn.
2. If the claim is empty, return.
3. Build **one** `SmtpClient::connect(&self.params)` for the whole batch. If it
   fails, mark every claimed row with that error, classified as above, and return.
4. Per row, bounded to `SEND_CONCURRENCY` by a `Semaphore` over a
   `tokio::task::JoinSet`: check out → `heartbeat_mail(id)` → drop → `client.send()`
   → check out → `mark_*` → drop.
5. Loop while the last claim returned a full `BATCH` and a `DRAIN_BUDGET`
   wall clock has not expired, so a backlog actually drains instead of moving 16
   messages per tick.

| Constant | Value | Why |
|---|---|---|
| `BATCH` | 16 | Small enough that one batch's worst-case hold stays well inside the stale threshold |
| `SEND_CONCURRENCY` | 4 | Mirrors `monitor_max_concurrency`'s existence, and keeps at most 4 short connection checkouts live out of 16 |
| `DRAIN_BUDGET` | 5 min | Bounds how long one tick can monopolise the process |
| `stale_secs()` | `(BATCH/SEND_CONCURRENCY) * total_deadline_secs * 2 + 60` | **Derived, not hardcoded.** With defaults it is 300s — the same number the first draft hardcoded, but now provably larger than a batch's worst-case hold. A hardcoded constant with a tunable batch size and a tunable timeout is how a drain robs its own sibling and a user gets two reset emails |

The per-row `heartbeat_mail` before each send makes the threshold independent of
`BATCH` and `SEND_CONCURRENCY` altogether, so the next person to tune those two
numbers without re-deriving the threshold does not reintroduce the bug.

`hygiene(retention_days)` runs, in order: `expire_stale_mail`,
`blank_expired_mail_bodies`, `prune_mail_outbox` in bounded
batches, then `mail_outbox_depth` logged at `info!` as
`pending=N oldest_pending_secs=M`. That last line is the only queue-depth signal
S0 ships and it is deliberately unconditional: there is no metrics endpoint and
no admin view, so without it a stalled queue is invisible until a user reports
that password reset does not work.

## 7. `backend/bins/sauron-api/src/tasks.rs` — the supervisor

`sauron-api`'s `main.rs` contains **zero** `tokio::spawn` today; this is genuinely
the process's first background work. That matters more than it looks. The
`tick + last_prune` idiom in `bins/sauron-alerts/src/main.rs:65-86` looks like the
thing to copy, but its failure semantics do not survive the move: there the loop
*is* `main()`, so a panic aborts the process and
`Restart=on-failure` in `packaging/rpm/systemd/sauron-alerts.service` brings it
back. In `sauron-api` the loop would be a detached task whose `JoinHandle` is
dropped. `backend/Cargo.toml` sets no `panic = "abort"` and `sauron-telemetry`
installs no panic hook, so tokio catches the panic and the task simply stops. The
HTTP server keeps serving, `/health` keeps returning 200, systemd sees a healthy
unit, and transactional email stops **forever**.

So S0 builds the supervisor once, and S1 and S2 mount into it rather than each
minting a pattern:

```rust
pub struct TaskHealth { last_success: Mutex<Option<Instant>>,
                        consecutive_failures: AtomicU32 }
pub fn supervise<F, Fut>(name: &'static str, interval: Duration, f: F) -> Arc<TaskHealth>
where F: Fn() -> Fut + Send + Sync + 'static,
      Fut: Future<Output = anyhow::Result<()>> + Send + 'static;
```

- Each tick spawns the closure's future as its own `tokio::spawn` and awaits the
  `JoinHandle`, so a panic arrives as `Err(JoinError)` instead of killing the loop.
- Panic or `Err` → `error!(task = name, …)`, increment `consecutive_failures`,
  back off `interval * min(failures, 8)` capped at 5 minutes.
- `Ok` → record `last_success`, reset the counter.
- An initial sleep of a per-process jitter in `0..interval`, derived from
  `SystemTime::now().subsec_nanos()`. With N instances behind a load balancer, a
  rolling restart otherwise makes all N fire the identical reaper within seconds
  of each other.
- Module doc carries the absolute rule: **no task's initialization may `?` out of
  `main()`.** `sauron-api.service` is `Restart=on-failure` with no StartLimit
  override, and `sauron-migrate.service` has no `[Install]` section, so a `?` on a
  missing table burns systemd's 5-starts-in-10s budget and leaves the unit
  `failed` with no HTTP surface to diagnose from.

Two tasks mount in `main()`:

| Task | Interval | Condition |
|---|---|---|
| `mail_drain` | `MAIL_DRAIN_TICK_SECS` (60, clamped 10..3600) | only when `require_smtp()` is `Ok` |
| `mail_hygiene` | 15 min | **unconditional** |

The hygiene task must not be conditional on SMTP, and that is the whole point of
splitting it out. The credential-at-rest story is the single control this design
nominates as its answer to the riskiest thing it introduces, and gating it on the
feature being switched on inverts it: an operator who enables SMTP, sends reset
mail, then unsets `SMTP_HOST` — rotating relays, cutting cost, or *responding to
an incident* — would otherwise leave every pending row, each holding a working
reset URL, in Postgres permanently, backed up and replicated, with no code path
that will ever touch it again. The hygiene task is pure SQL and needs no relay.

`/health` grows a body:

```rust
.route("/health", get(health))   // ALWAYS 200
// {"status":"ok","tasks":[{"name":"mail_drain","last_success_secs":12,
//                          "consecutive_failures":0}, …]}
```

`last_success_secs` is `null` before the first success. **It never changes the
status code.** `packaging/rpm/SETUP.md:115` documents `curl -fsS …/health` and
`tests/http_env_scoping.rs:208` polls it for readiness — both read a non-2xx as
"the API is down", which a stalled reaper is not.

`AppState` gains one field:

```rust
/// None when SMTP is unconfigured. Every caller must degrade rather than fail:
/// the API has to boot and serve everything else on a deployment with no relay.
pub mail: Option<crate::mail::MailSender>,
```

In `main()`, after the state is built:

```rust
match cfg.require_smtp() {
    Err(e) => info!(reason = %e, "transactional email disabled"),
    Ok(s) => { if s.sink { warn!(…) } /* build sender, supervise mail_drain */ }
}
tasks::supervise("mail_hygiene", Duration::from_secs(900), …);   // outside the match
```

The drain loop should also special-case Postgres SQLSTATE `42P01`
(undefined_table) with a one-shot `error!` naming `sauron-migrate`, rather than
logging the same opaque diesel error every 60 seconds. That is the exact symptom
an RPM upgrade produces (§9).

## 8. The dev sink

`SMTP_SINK=1` makes `SmtpClient::connect` and `send` return before touching a
socket, so every caller, every template and the whole outbox state machine are
exercised identically to production — the only difference is the last five lines.
It sits at the single narrowest point that would otherwise open a connection.

Three rules make it safe, and each closes a real hole in the first draft:

1. **`SMTP_SINK` is read on its own and defaults to `false`.** Deriving it from
   `SAURON_DEV` was cheap in local dev and catastrophic in the case that
   actually happens: `SAURON_DEV=1` exists solely to relax the `JWT_SECRET` rule
   (`config.rs:143-158`, README:165), so an operator who sets it to get a stalled
   first boot or an RPM upgrade past a secret complaint would have, with no
   further action, converted every transactional email into a log line containing
   a working account-takeover URL.
2. **The body is logged only when `SMTP_SINK=1` *and* `SAURON_DEV=1`.** Logs are
   routinely shipped to an aggregator with a broader reader set and a longer
   retention than the database — a sink that logs bodies strictly worsens the
   exposure the rest of this design narrows. `RUST_LOG` is no gate: the shipped
   default is `info,sauron=debug`, and `EnvFilter` matches targets by prefix, so
   `sauron_mail::sink=debug` is already on. Two explicit variables is the gate.
   The header line — outbox id, kind, recipient, subject — logs at `warn!`
   whenever the sink is on. The **plain-text** body is the one logged, not the
   HTML: it is the readable one and it contains the same URL.
3. **A sink delivery is `status='sink'`, never `'sent'`.** `status='sent'` is the
   one observable this whole design offers; a sink row reporting `sent` for mail
   that was never transmitted makes the single place an operator would look
   actively lie. `last_error` carries `delivered to log sink (SMTP_SINK=1)`.
   `prune_mail_outbox` treats `sink` as terminal alongside `sent` and `failed`.

`sauron-api` emits one additional startup `warn!` naming exactly what is on and
whether bodies are being written, so it can never be silently enabled in
production.

## 9. Failure semantics

| Situation | Operator sees | Caller sees |
|---|---|---|
| SMTP unconfigured | one INFO line at boot, `AppState.mail = None`, API serves normally | an unauthenticated route's normal response, unchanged. A route already behind a permission refuses with 503 carrying the `require_smtp` text, before it applies any state change |
| `DASHBOARD_URL` unset | `require_dashboard_url()` error naming the variable and the expected URL shape; render fails at enqueue | the same split — the misconfiguration is reported to a caller who is already an admin, at the moment a human is looking, and to nobody else |
| Relay unreachable / resolver hiccup / TLS setup failure / deadline | `warn!` with outbox id, kind, recipient, error; row retries 30s→900s to `max_attempts` 8 | nothing |
| 5xx rejection, invalid address, blocked host | `warn!`; row `failed` after one attempt; body kept until `expires_at` so it can be requeued by hand | nothing |
| Row never delivered before `expires_at` | row `failed` with `expired before delivery` | nothing |
| Process killed mid-send | row reclaimed after `stale_secs`, backoff applied, `attempts` still counts toward the cap | possibly a duplicate — see below |
| Queue stalled (panicked task) | `error!` from the supervisor each tick, `consecutive_failures` on `/health`, `pending=N oldest_pending_secs=M` every 15 min | nothing |

**Delivery is at-least-once, not at-most-once.** If the SMTP send succeeds but
the process dies before `mark_mail_sent`, the row is reclaimed and re-sent, and a
user gets two identical emails. That is the right trade — the alternative is
losing mail — but it constrains S1: the reset token must tolerate arriving in two
envelopes, which it does as long as S1 does not mint a token per send attempt.
The per-recipient dedup window shrinks the window further, incidentally.

**Nothing account-specific is ever observable in an HTTP response.** Recipient
existence and send outcome have no signal on the request path at all — the
enqueue costs one round trip either way, the send happens off it, and
`ApiError::Internal` already collapses to a generic body while logging detail.
What *is* observable, and only to a caller who has already passed a permission
check, is deployment-wide relay state: an admin-initiated send refuses with 503
rather than silently succeeding into a queue nothing will ever drain. A uniform
503 on an authenticated route says nothing about any account, which is why it is
allowed there and forbidden on `forgot-password`. The recipient is logged only on
failure, never on success, so an address — which is PII — stays out of the
steady-state log while an operator can still answer "why did this bounce".

**Render-at-enqueue makes stale-authorization delivery the default, and S3 must
opt out of it.** For `password_reset` this is harmless. For a digest, the
rendered snapshot of an org's issue titles would otherwise be transmitted on the
authorization state that existed at enqueue: an admin revokes a member's grant at
T+2 minutes, the relay was down at T, and the digest leaves at T+40 with no
revocation check anywhere in the path — the drain reads `RETURNING *` and sends,
and nothing in it can consult `role_grants`, because the body is already
rendered. A short `expires_at` is the mechanism S0 provides — 15 minutes for both
`NotificationDigest` and `PersonalNotification`, which is why those two are the
kinds whose expiry is a round number rather than a token lifetime. S3 owns
choosing between that and re-rendering at send.

## 10. Config, packaging and docs wiring

Thirteen new variables. Three (`SMTP_ALLOW_PRIVATE`, `SMTP_TIMEOUT_MS`,
`SMTP_SINK`) have safe defaults an operator never touches, so a typical
deployment sets four: `SMTP_HOST`, `SMTP_FROM`, `SMTP_PASSWORD`, `DASHBOARD_URL`.

| Variable | Default |
|---|---|
| `SMTP_HOST` | unset ⇒ transactional email disabled |
| `SMTP_PORT` | `587` |
| `SMTP_USERNAME` / `SMTP_PASSWORD` | unset |
| `SMTP_FROM` | unset; **required** once `SMTP_HOST` is set |
| `SMTP_FROM_NAME` | `Sauron` |
| `SMTP_TLS` | `implicit` when `SMTP_PORT` is 465, else `starttls` |
| `SMTP_ALLOW_PRIVATE` | `false` |
| `SMTP_TIMEOUT_MS` | `ALERTS_DELIVER_TIMEOUT_MS` (10000), clamped 1000..60000 |
| `SMTP_SINK` | `false` |
| `DASHBOARD_URL` | unset ⇒ any email containing a link refuses to render |
| `MAIL_DRAIN_TICK_SECS` | `60`, clamped 10..3600 |
| `MAIL_OUTBOX_RETENTION_DAYS` | `30` |

Booleans use the house
`.map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false)` form.

- **`.env.example`** — a new `# --- transactional email (sauron-api) ---` block
  after the alerting block, in the house long-comment register: this is
  deployment-level mail, distinct from per-org notification channels; a relay on
  a LAN needs `SMTP_ALLOW_PRIVATE=true` or the SSRF guard blocks it; `SMTP_TLS=none`
  is only accepted for a loopback relay; leaving `SMTP_HOST` unset disables
  password reset rather than breaking the API. `DASHBOARD_URL` goes in the
  existing `# --- CORS / URLs ---` block with the note that it is the
  browser-facing origin of the SPA, which in the shipped nginx topology is
  **not** the API's origin.
- **`docker-compose.yml`** — the `api:` service gains `DASHBOARD_URL: ${DASHBOARD_URL:-}`
  and each `SMTP_*` as a `${VAR:-}` passthrough. **No fallback on
  `DASHBOARD_URL`.** Mirroring `CORS_ALLOWED_ORIGINS: ${…:-http://localhost:10002}`
  was the obvious move and it is exactly the behaviour this design rejects
  everywhere else: `require_dashboard_url()` would succeed, `Branding::link`
  would render `http://localhost:10002/#/reset-password?token=…`, the message
  would send, the row would reach `sent` — every server-side signal reporting
  success while the user's browser hits their own machine. An unset value must
  produce the loud error naming the variable.
- **`packaging/rpm/config/api.env`** — a commented section for `DASHBOARD_URL`
  (left commented out, not given a working-looking localhost value) and every
  `SMTP_*` except the password, each documenting the consequence of getting it
  wrong. `SMTP_PASSWORD` gets a pointer only:
  `# SMTP_PASSWORD belongs in /etc/sauron/secret.env (0640 root:sauron), not here.`
  That file is a `%ghost` generated in `%post` (`sauron.spec:189-200`) and
  `sauron-api.service` already loads it non-optionally, so no unit change.
- **`packaging/rpm/SETUP.md`** — add `SMTP_PASSWORD` to the secret.env section
  (it documents only `JWT_SECRET` today), an "enabling transactional email" step
  naming `DASHBOARD_URL`, and **§11 "Upgrading", which does not exist today**.
  SETUP.md has sections 1–10 and no upgrade guidance at all, while every slice in
  this programme points at it as their mitigation. S0 creates it once, with the
  gate in the imperative:
  `systemctl stop sauron-api sauron-ingest && systemctl start sauron-migrate && systemctl start sauron-api sauron-ingest`,
  plus a per-migration table each later slice appends to, and a line telling
  operators to diff `api.env.rpmnew` — `/etc/sauron/api.env` is
  `%config(noreplace)` (`sauron.spec:237`), so the new SMTP block never reaches an
  upgrading operator's actual file.
- **`packaging/rpm/sauron.spec`** — a `%changelog` entry naming the new
  `mail_outbox` table and instructing `sauron-migrate` after upgrading, matching
  the migration-000032 precedent at `sauron.spec:265`. `%post server` runs only
  `%systemd_post`, `ldconfig` and a first-install-only secret generation;
  `sauron-migrate.service` has no `[Install]` section and is never started on
  upgrade. Without this, `dnf upgrade` installs a `sauron-api` that queries
  `mail_outbox` against a schema with no such relation, and because S1 swallows
  enqueue errors behind a fixed 200 the user-visible symptom is "password reset
  does nothing, silently".
- **No binary is added**, so `packaging/rpm/binaries.txt`, the spec's `%install`
  loop / `%files` / `%post` / `%preun` / `%postun`, `packaging/rpm/systemd/` and
  `build-rpm.sh` are all untouched. That is the main packaging benefit of putting
  the drain inside `sauron-api` rather than in a new worker.
- **`README.md`** — a `### Transactional email` table after
  `### Alerting & notifications`, one row per variable in the existing
  `| Variable | What it does | Default | Used by |` format, led by a note that
  this is separate from notification channels and that leaving it unset disables
  password reset rather than degrading anything else. `DASHBOARD_URL` gets a row
  in the CORS/URLs area.
- **CI** needs no new configuration. `cargo fmt --all --check` and
  `cargo clippy --workspace --all-targets -- -D warnings` pick the new crate up
  from the glob; `cargo test --workspace` runs its unit tests with no database.

## 11. Testing

**Constraint:** CI runs `cargo test --workspace` with no Postgres service. The
DB-backed tests use the existing `TestDb::setup() -> Option` pattern
(`crates/sauron-db/tests/common/mod.rs`) and skip rather than fail when
`TEST_DATABASE_URL` is unset.

**`sauron-mail` unit — no I/O:**

- *Escaping.* A `MailContent` whose heading, one paragraph, one footnote and the
  CTA label each contain `<script>alert(1)</script>&"` renders HTML in which every
  one appears as `&lt;script&gt;` and none appears raw — and renders text in which
  all of them appear **verbatim**, because the text part is not markup and must
  not carry entities. Mirrors `matrix_html_escapes_user_content`.
- *Plain-text fidelity.* The CTA URL appears verbatim on its own line, the text
  part contains no `<` at all, and paragraph order and blank-line separation hold.
- *Placeholder key set.* Scan `LAYOUT_HTML` for every `{{…}}`, collect the trimmed
  keys, assert the set is exactly
  `{subject, preheader, product, heading, paragraphs, cta, footnotes, footer}`.
  This is what catches a stray `{{` in the CSS silently deleting the stylesheet,
  and a typo'd placeholder rendering as a hole. Same test for `P_HTML`,
  `CTA_HTML`, `FOOTNOTE_HTML`.
- *Layout invariants.* Rendered HTML contains `max-width:600px`, `width="600"`,
  `role="presentation"`, `color-scheme`, and **no** `<img`; doctype/head/body each
  appear exactly once.
- *URL guards.* `Cta::new` rejects `javascript:alert(1)`, `data:text/html,…` and a
  bare `/reset`; accepts `http://` and `https://`. `Branding::link` returns
  `NoDashboardUrl` when unset, and otherwise produces `https://host/#/path` with
  exactly one slash before the `#` whether or not the base had a trailing slash.
- *Header sanitation.* A subject containing `\r\nBcc: attacker@evil.test` renders
  with CR/LF replaced by spaces; a 500-char subject truncates to 200.
- *Recipient normalization.* `victim@corp.test`, `Victim@Corp.Test` and
  `victim@corp.test ` all normalize to one key; `victim@corp.test <x>` is
  rejected, not silently truncated.
- *Redaction.* `format!("{:?}", params)` for `password: Some("hunter2")` contains
  `<redacted>` and does not contain `hunter2`.
- *Resolve classification.* `classify_resolve_error("DNS resolution failed: x")`
  is `Dns`, `"target x resolves to a blocked address"` is `Blocked`, and an
  unrecognised string is `Dns` — the transient direction.
- The moved `text.rs` tests, carried over verbatim.

**`sauron-core` unit:** the `build_smtp` truth table over the pure function (not
over env vars, which race under `cargo test`) — host unset + sink false → `Err`
naming `SMTP_HOST`; host set + from unset → `Err` naming `SMTP_FROM`; from without
an `@` → `Err`; `SMTP_TLS` unset at port 465 → `Implicit`, at 587 → `StartTls`;
`SMTP_TLS=none` with a non-loopback host → `Err` whose text contains both
`SMTP_TLS` and the host; `SMTP_TLS=none` with `127.0.0.1` → `Ok`;
`SMTP_TLS=garbage` → `Err` listing the accepted values; timeouts 10 and 900_000
both clamp into 1000..60000; sink true with everything unset → `Ok`. Plus the
redaction assertions for `SmtpSettings` and for the whole `Config`.

**`sauron-alerts` unit:** `is_transient(MailError::Send("connection reset").to_string())`
true; `is_transient(MailError::Rejected("550 no such user").to_string())` **true**
— proving the refactor did not change alert-email behaviour;
`is_transient(MailError::DeadlineExceeded(30000).to_string())` true;
`InvalidFrom` and `Blocked` both false.

**`sauron-db` integration:**

- *Happy path.* `enqueue_mail` inserts `pending`/`attempts 0`/`next_attempt_at <= now()`;
  `claim_due_mail(1)` returns it as `sending`/`attempts 1`; `mark_mail_sent` sets
  `sent`, non-null `sent_at`, and **both body columns empty** — the
  credential-scrubbing assertion is the important one.
- *Dedup.* Two `enqueue_mail` calls with the same kind and recipient inside the
  window: the second returns `None` and inserts nothing. A third after the window
  inserts. A row in `failed` does not suppress a retry.
- *Constant-cost discard.* `commit = false` returns `None` and inserts nothing,
  while `commit = true` with otherwise identical arguments inserts one row.
- *SKIP LOCKED.* Enqueue three rows, run two `claim_due_mail(2)` calls on separate
  connections concurrently; the union of returned ids has no duplicates and totals
  exactly three.
- *Claim guards.* A row past `expires_at` is never claimed.
- *Per-call expiry.* Two `enqueue_mail` calls with the same `kind` and different
  `ttl_secs` land two different `expires_at` values, so nothing anywhere derives
  the deadline from the kind.
- *Stale recovery.* A row forced to `sending` with `updated_at` 10 minutes ago:
  `requeue_stuck_mail(300)` returns 1, the row is `pending` again **and its
  `next_attempt_at` has moved forward**. The same row at `attempts = max_attempts`
  goes to `failed`, not `pending`. A `sending` row with `updated_at = now()` is
  untouched.
- *Lost claim.* `mark_mail_sent(id, attempts = 1)` on a row whose `attempts` is
  now 2 returns 0 and changes nothing.
- *Backoff and give-up.* `mark_mail_failed(permanent = false)` on `attempts = 1`
  leaves `pending` with `next_attempt_at` ~30s out **and the body intact**; the
  same at `max_attempts` sets `failed`; `permanent = true` sets `failed` after a
  single attempt without consuming the remaining attempts.
- *Hygiene.* `blank_expired_mail_bodies` empties both body columns on a `pending`
  row whose `expires_at` has passed and leaves its status alone — and leaves an
  unexpired row's body **intact**, which is the assertion that catches anyone
  reintroducing a flat age cutoff and scrubbing a live 24-hour reset mail.
  `expire_stale_mail` moves an expired `pending` row to `failed`.
  `prune_mail_outbox(0, 5000)` deletes `sent`, `failed` and `sink` rows and
  leaves `pending`/`sending` untouched regardless of age.

**`sauron-api` integration** (`tests/http_mail_outbox.rs`, using the `TestServer`
harness that spawns the real `CARGO_BIN_EXE_sauron-api` against an ephemeral DB):
boot with `SMTP_SINK=1` and no `SMTP_HOST` and assert `/health` is up; boot with
`SMTP_SINK=1` plus `SMTP_FROM` and assert the same. This is the regression test
for the fail-closed-at-point-of-use rule that bailing in `from_env` once broke.
Also assert `/health`'s body lists `mail_hygiene` on a deployment with no SMTP at
all, which is the check that the hygiene task really is unconditional.

**Manual E2E** — the real gate for everything DB- and network-dependent:

1. **Dev sink.** `SMTP_SINK=1 SAURON_DEV=1 DASHBOARD_URL=http://localhost:3000`,
   drive the S1 forgot-password flow, confirm the rendered plain-text body —
   including a clickable `http://localhost:3000/#/reset-password?token=…` — appears
   in the log, and that the row reaches `status='sink'` (**not** `'sent'`) with
   both bodies empty. Repeat with `SMTP_SINK=1` and `SAURON_DEV` unset and confirm
   the header line logs but the body does not.
2. **Cleartext config refusal.** `SMTP_HOST=192.168.1.20 SMTP_TLS=none` → assert
   the boot log carries the `build_smtp` loopback message and mail is disabled.
   Then `SMTP_HOST=localhost SMTP_TLS=none` with `/etc/hosts` pointing `localhost`
   off-box → assert `MailError::Blocked` at connect. These are two different
   checks and the first draft conflated them into one unreachable step.
3. **SSRF block.** `SMTP_TLS=starttls SMTP_HOST=127.0.0.1` with
   `SMTP_ALLOW_PRIVATE` unset → this is the path that actually reaches
   `resolve_checked` and yields `Blocked`. Note the raw message from
   `resolve_checked` is `target {host} resolves to a blocked address` and does not
   name the variable, so `sauron-mail` appends it.
4. **Real relay, no TLS.** MailHog on `127.0.0.1:1025` with
   `SMTP_TLS=none SMTP_ALLOW_PRIVATE=true`; inspect the MailHog UI for a correct
   `multipart/alternative` — HTML inside a 600px card, text part readable alone.
5. **Real relay, real TLS.** A genuine relay on :587 with credentials; confirm
   delivery to a Gmail and an Outlook.com inbox and eyeball both plus one mobile
   client — the 600px cap, the button, and that the preheader rather than raw
   markup shows in the inbox preview list.
6. **Backlog.** Enqueue 100 rows against MailHog and confirm they drain inside one
   tick rather than 16 per minute, and that one `SmtpClient` handled the batch.
7. **Regression.** `POST /v1/notification-channels/{id}/test` against a real SMTP
   channel: the alert email must be byte-identical to before — same
   `[Sauron/info] …` subject, same `text/plain`, same `— Sauron alerting` footer.
   Also exercise the **failure** response: a channel pointed at a dead host must
   still return the same `smtp send failed: …` string in the `error` field, since
   `test_channel` surfaces it verbatim. The one deliberate behaviour change is
   that a tarpitting relay now fails at the total deadline instead of hanging.

## 12. Files

**New**
- `backend/migrations/2026-08-01-000034_mail_outbox/{up,down}.sql`
- `backend/crates/sauron-mail/{Cargo.toml,src/lib.rs,src/kind.rs,src/smtp.rs,src/template.rs,src/text.rs}`
- `backend/bins/sauron-api/src/mail.rs` — `MailSender`
- `backend/bins/sauron-api/src/tasks.rs` — the supervisor
- `backend/bins/sauron-api/tests/http_mail_outbox.rs`
- `packaging/rpm/SETUP.md` §11 "Upgrading"

**Modified**
- `backend/Cargo.toml`, `backend/crates/sauron-alerts/Cargo.toml`,
  `backend/bins/sauron-api/Cargo.toml`
- `backend/crates/sauron-core/src/config.rs` — `SmtpSettings`, `SmtpTls`,
  `build_smtp`, `require_smtp`, `require_dashboard_url`, `pub dev_mode`,
  hand-written `Debug`
- `backend/crates/sauron-db/src/{schema.rs,models.rs,repo.rs}`
- `backend/crates/sauron-alerts/src/{deliver.rs,render.rs,engine.rs}` —
  `deliver_email` shrinks to ~15 lines building `SmtpParams`/`OutgoingMail`;
  `render` re-exports the moved primitives; `engine` calls
  `sauron_mail::is_transient`
- `backend/bins/sauron-api/src/main.rs` — `AppState.mail`, two supervised tasks,
  `/health` body
- `.env.example`, `docker-compose.yml`, `README.md`,
  `packaging/rpm/config/api.env`, `packaging/rpm/sauron.spec` (`%changelog`)

## 13. Hand-offs to later slices

- **The outbox is the async side-effect primitive, not a mail detail.** It is the
  first durable, restart-surviving, observable deferred-work mechanism in this
  codebase. S1 uses it instead of `tokio::spawn`; S3 uses it for digests; any
  future "do this after the response" work uses it rather than minting a second
  pattern. Say so in `sauron-mail`'s crate doc.
- **S1 deletes its spawn/semaphore section entirely** — the 8-permit semaphore,
  the spawned worker, the awaited admin send, and its "no request-level
  backpressure" risk entry all have nothing to attach to. `forgot_password`
  becomes: two rate-limit checks → look up user → `enqueue_or_discard` → generic
  200.
- **The two reset routes report a dead relay differently, and S1 owns both.**
  `POST /v1/auth/forgot-password` is unauthenticated, so it returns its generic
  200 whether or not `AppState.mail` is `None`: a response that distinguishes
  configured from unconfigured is a config oracle handed to anyone on the
  internet, and a handler that enqueues cannot know the delivery outcome anyway.
  The admin-initiated route is already behind `member:manage`, so it may say so —
  `503` carrying the `require_smtp` / `require_dashboard_url` text, refused
  **before** any state change, which is what the dialog needs at the moment a
  human is watching. Neither route ever returns the link itself.
- **`html_escape` and `substitute` are `pub` in `sauron_mail::text`, not in
  `sauron_alerts::render`.** S1's instruction to make `render::html_escape` public
  targets a file S0 removes the code from.
- **Dashboard routing, verified and stated once because both readings look
  plausible.** `dashboard/src/App.svelte:11` is
  `const PUBLIC_ROUTES = ['/login', '/register']`, and the `$effect` at :18-24
  pushes an authenticated visitor **away** from any route in that array. Publicness
  comes from the other direction: `routes.ts:55-56` registers `/login` and
  `/register` as bare components with no `wrap({ conditions })`. So S1 registers
  `/forgot-password` and `/reset-password` **unwrapped in `routes.ts`**, puts
  `/forgot-password` **in** `PUBLIC_ROUTES` (a signed-in user has no business
  there), and deliberately leaves `/reset-password` **out** — a reset link arriving
  by email while a stale session exists must still complete.
- **Every reaper lives in the process that owns its table's write path.**
  `mail_outbox` → `sauron-api`'s supervisor, as designed. `password_reset_tokens`
  → also `sauron-api`'s supervisor, **not** `sauron-alerts`' hourly loop: password
  reset must not silently stop being reaped because an optional worker is not
  deployed. Retention values are compile-time consts, not env vars.
- **S2 mounts the revocation poller into `tasks.rs`** and gets its "surface
  `age()` on /health" mitigation for free — and must drop its synchronous pre-bind
  `revocations.refresh(..).await?`, per the supervisor's no-`?`-at-boot rule.
- **S0 lands `AppState.mail` first** (purely additive, no extractor change); S2
  lands its `FromRef` + generic-bound change second and rebases.
- **S0 defines the `.env.example` section headers**
  (`# --- transactional email (sauron-api) ---`, and the existing
  `# --- CORS / URLs ---`); S2 appends one line. A CI grep asserting every
  `var("KEY")` / `parse("KEY"` literal in `config.rs` appears in `.env.example`
  costs an hour and is the only thing that will keep thirteen new variables
  documented in a year — S0 should add it.

## 14. Follow-ups (out of scope)

- An operator-facing "send a test email" endpoint and button. `MailKind::SmtpTest`
  already exists for it, so it needs no schema change — only a route, a permission
  decision (probably `alert:write`) and UI.
- An admin view of `mail_outbox`. If it is ever built it must project columns
  explicitly and never return `body_text`/`body_html`.
- Rendering **alert** mail through the new HTML layout. An obvious follow-up and
  an obvious way to break six channel kinds at once.
- Setting `AlertContext.link`. `DASHBOARD_URL` finally makes it possible — the
  field is plumbed through `url_payload`, `matrix_content`, `telegram_text` and
  `email_body` but has never been set in production code — but threading it into
  every construction site is an alerting change, not a mail-foundation one.
- Splitting SMTP 4xx from 5xx for the **alerting** path. `MailError::Rejected`
  exists and the outbox uses it; `is_transient` deliberately still treats it as
  retryable so alerting is unchanged. Fixing the known gotcha that a permanently
  misconfigured email channel burns 3 attempts and ~30s on every fire is a
  one-line change to `is_transient` — and an alerting behaviour change that
  deserves its own decision.
- Giving `sauron_monitor_core::ssrf::resolve_checked` a typed error, so
  `classify_resolve_error` can stop matching on substrings. Three callers, so it
  is its own change.
- Moving `MailSender` out of `bins/sauron-api/src/mail.rs` into a crate. Correct
  today because `sauron-api` is the only enqueuer and the only drainer; the moment
  S3 needs a worker-side digest drain, the `SKIP LOCKED` claim makes that
  mechanical.
- Graceful shutdown, and having `sauron-api` run `run_pending_migrations` at boot
  the way the test harness already does. This is the third programme in a row to
  pay the RPM-upgrade tax; §11 of SETUP.md documents around it rather than fixing
  it.
