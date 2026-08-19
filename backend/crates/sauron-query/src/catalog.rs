//! The dimension catalog: the single source of truth for what is filterable.
//!
//! Five things derive from this table rather than being maintained alongside it:
//! field resolution, the planner's SQL mapping and cost classification, the
//! `/search/fields` autocomplete endpoint, the in-app docs reference, and the
//! `wiki/Search.md` field table.
//!
//! Adding a dimension here does NOT make it queryable — the planner (S2) must
//! also learn to map its `Store` to SQL. `dimensions_for` is what the tests in
//! S2 iterate to prove nothing is declared-but-unplanned.

use crate::ast::MatchOp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resource {
    Issues,
    Occurrences,
    Events,
    Sessions,
    Devices,
    Persons,
    Transactions,
}

impl Resource {
    pub const ALL: &'static [Resource] = &[
        Resource::Issues,
        Resource::Occurrences,
        Resource::Events,
        Resource::Sessions,
        Resource::Devices,
        Resource::Persons,
        Resource::Transactions,
    ];
}

/// Where the value physically lives. The planner switches on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store {
    /// A real column. The `&'static str` is the column name.
    Column(&'static str),
    /// A JSONB column addressed by a caller-supplied dynamic path, e.g. `extra.*`.
    ///
    /// `column` is the physical column; `prefix` is the path segment that must be
    /// prepended inside it. They differ because `enrich.rs` writes several
    /// namespaces into one `context` column — `os.name` lives at `context->os->name`
    /// (prefix `os`), whereas `extra.cartValue` lives at `extra->cartValue`
    /// (prefix empty) and `user.email` at `event_user->email` (prefix empty, since
    /// the column *is* the user object).
    JsonRoot {
        column: &'static str,
        prefix: &'static str,
    },
    /// The `tags` JSONB column, keyed by the dimension name itself.
    Tag,
    /// The `issue_dimensions` rollup table (built in S3).
    Rollup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    Str,
    Enum(&'static [&'static str]),
    Int,
    Bool,
    /// Accepts `2s` / `500ms` / bare milliseconds.
    Duration,
    /// Accepts `-7d` relative or ISO-8601 absolute.
    Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexClass {
    /// Backed by an index that serves this predicate directly.
    Indexed,
    /// Not indexed, but reached only after an indexed predicate has bounded the
    /// candidate set (same table, cheap to evaluate per row).
    Bounded,
    /// Requires reading the value off every candidate row.
    Scan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimension {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub ty: ValueType,
    pub store: Store,
    pub ops: &'static [MatchOp],
    pub resources: &'static [Resource],
    pub index: IndexClass,
}

/// `is:<keyword>` expands to a predicate on `field` with `value`.
pub struct Shorthand {
    pub keyword: &'static str,
    pub field: &'static str,
    pub value: &'static str,
}

/// Spec §5: `is:unhandled` and `is:handled` both exclude NULL — rows ingested
/// before the `handled` column existed are *unknown*, not handled.
pub const SHORTHANDS: &[Shorthand] = &[
    Shorthand {
        keyword: "unresolved",
        field: "is",
        value: "unresolved",
    },
    Shorthand {
        keyword: "resolved",
        field: "is",
        value: "resolved",
    },
    Shorthand {
        keyword: "ignored",
        field: "is",
        value: "ignored",
    },
    Shorthand {
        keyword: "handled",
        field: "handled",
        value: "true",
    },
    Shorthand {
        keyword: "unhandled",
        field: "handled",
        value: "false",
    },
];

const OPS_EQ: &[MatchOp] = &[MatchOp::Eq, MatchOp::Ne, MatchOp::In, MatchOp::Has];
const OPS_TEXT: &[MatchOp] = &[
    MatchOp::Eq,
    MatchOp::Ne,
    MatchOp::In,
    MatchOp::Has,
    MatchOp::Like,
    MatchOp::Contains,
];
const OPS_ORD: &[MatchOp] = &[
    MatchOp::Eq,
    MatchOp::Ne,
    MatchOp::Gt,
    MatchOp::Gte,
    MatchOp::Lt,
    MatchOp::Lte,
    MatchOp::Has,
];
/// Exactly the three operators the pre-language `filter=workflow:<op>:<value>`
/// wire format accepted (`eq`, `neq`, `contains`). `Ne` is listed for
/// completeness even though the parser never emits it — `!workflow:x` arrives
/// as `Eq` with `negate = true`.
const OPS_WORKFLOW: &[MatchOp] = &[MatchOp::Eq, MatchOp::Ne, MatchOp::Contains];
const NO_ALIAS: &[&str] = &[];

const R_ISSUES: &[Resource] = &[Resource::Issues];
const R_OCC: &[Resource] = &[Resource::Occurrences];
const R_ISSUE_OCC: &[Resource] = &[Resource::Issues, Resource::Occurrences];
const R_EVENTS: &[Resource] = &[Resource::Events];
const R_OCC_EVENTS: &[Resource] = &[Resource::Occurrences, Resource::Events];
/// The `extra` set: the two event resources plus Transactions, which gained a
/// dev-supplied `extra` column in migration 0063. Kept separate from
/// [`R_OCC_EVENTS`] because `contexts` deliberately did NOT follow it there.
const R_OCC_EVENTS_TX: &[Resource] = &[
    Resource::Occurrences,
    Resource::Events,
    Resource::Transactions,
];
/// All three list resources S2c bridges onto the language. Only `workflow`
/// uses it — the one field every one of the three pre-language registries
/// (`ISSUE_FILTERS`/`ERROR_EVENT_FILTERS`/`EVENT_FILTERS`) accepts.
const R_ISSUE_OCC_EVENTS: &[Resource] =
    &[Resource::Issues, Resource::Occurrences, Resource::Events];
const R_TX: &[Resource] = &[Resource::Transactions];
const R_DEVICES: &[Resource] = &[Resource::Devices];
const R_PERSONS: &[Resource] = &[Resource::Persons];
const R_SESSIONS: &[Resource] = &[Resource::Sessions];

const LEVELS: &[&str] = &["debug", "info", "warning", "error", "fatal"];
const STATUSES: &[&str] = &["unresolved", "resolved", "ignored"];
const SYMBOLICATION: &[&str] = &[
    "pending",
    "processing",
    "symbolicated",
    "failed",
    "skipped",
    "unsupported",
];

pub const CATALOG: &[Dimension] = &[
    // ---- issues (own columns) ----
    Dimension {
        name: "is",
        aliases: &["status"],
        ty: ValueType::Enum(STATUSES),
        store: Store::Column("status"),
        ops: OPS_EQ,
        resources: R_ISSUES,
        index: IndexClass::Indexed,
    },
    Dimension {
        name: "level",
        aliases: NO_ALIAS,
        ty: ValueType::Enum(LEVELS),
        store: Store::Column("level"),
        ops: OPS_EQ,
        resources: R_ISSUE_OCC,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "type",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("type"),
        ops: OPS_TEXT,
        resources: R_ISSUES,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "culprit",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("culprit"),
        ops: OPS_TEXT,
        resources: R_ISSUES,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "title",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("title"),
        ops: OPS_TEXT,
        resources: R_ISSUES,
        index: IndexClass::Scan,
    },
    Dimension {
        name: "timesSeen",
        aliases: &["times_seen"],
        ty: ValueType::Int,
        store: Store::Column("times_seen"),
        ops: OPS_ORD,
        resources: R_ISSUES,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "usersSeen",
        aliases: &["users_seen"],
        ty: ValueType::Int,
        store: Store::Column("users_seen"),
        ops: OPS_ORD,
        resources: R_ISSUES,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "firstSeen",
        aliases: &["first_seen"],
        ty: ValueType::Timestamp,
        store: Store::Column("first_seen"),
        ops: OPS_ORD,
        resources: R_ISSUES,
        index: IndexClass::Indexed,
    },
    Dimension {
        name: "lastSeen",
        aliases: &["last_seen"],
        ty: ValueType::Timestamp,
        store: Store::Column("last_seen"),
        ops: OPS_ORD,
        resources: R_ISSUES,
        index: IndexClass::Indexed,
    },
    // **Reading the three passages below: `resolve_field`'s "step 4" no longer
    // exists.** Each widening of this dimension was argued for at the time as
    // the alternative to a bare `workflow` being read as a TAG KEY, which is
    // what an unrecognised field used to become. That fallback has since been
    // removed, so the failure mode a missing entry would produce today is a
    // loud 400 rather than a silent wrong answer. The entries stay exactly as
    // they are: a 400 on every existing `filter=workflow:…` bookmark is still
    // a regression, just an honest one. The history is kept because it records
    // WHY each resource was added, which is not recoverable from the list.
    //
    // S2c Task 4: NOT a new capability — the pre-language wire format already
    // had `filter=workflow:eq:X` on the issues list (`sauron_db::filter::
    // ISSUE_FILTERS`), and `routes/issues.rs` is now bridged through
    // `from_legacy`. Without an entry here, `resolve_field`'s step 4 would
    // reinterpret the bare field `workflow` as a TAG KEY, and every existing
    // `filter=workflow:…` bookmark would silently return the wrong rows —
    // no error, just a different answer. That is precisely the failure the
    // legacy bridge exists to prevent.
    //
    // `issues` carries no workflow column, so this lowers to a correlated
    // EXISTS into `error_events` (see `IssuesLower`), same shape as `tag`.
    // `Contains` is here because legacy `filter=workflow:contains:` existed;
    // `In`/`Like`/`Has` never did, so they are deliberately absent rather than
    // reusing `OPS_TEXT` — an op the old wire format never accepted has no
    // bookmark to keep working, and `has:workflow` is better as a loud
    // "unsupported op" than as a silent tag probe.
    //
    // **S2c Task 5 widened this to `R_ISSUE_OCC`**, for the same
    // silent-wrong-answer reason it was added at all: the per-issue
    // occurrences route (`/v1/apps/{id}/issues/{id}/events`) accepts
    // `filter=workflow:<op>:<value>` through `sauron_db::filter::
    // ERROR_EVENT_FILTERS`, and Task 5 bridges that route through
    // `from_legacy` too. Left `R_ISSUES`-only, every such bookmark would hit
    // `resolve_field`'s step-4 tag fallback and probe a TAG key named
    // `workflow` instead — the same failure this entry exists to prevent, one
    // level down the drill-down.
    //
    // One entry, two lowerings, and that is fine: `Store` names where the
    // value lives *for the resource being lowered*, and each `ResourceLower`
    // switches on it independently. `IssuesLower` maps
    // `Store::Column("workflow")` to a correlated EXISTS (there is no such
    // column on `issues`); `OccurrencesLower` maps it to the real
    // `error_events.workflow_name` column. Splitting it into two dimensions
    // with the same `name` would instead make `resolve_field`'s lookup
    // order-dependent.
    //
    // **S2c Task 6 widened it again, to `R_ISSUE_OCC_EVENTS`**, for the third
    // and last time and for the identical reason: the analytics Event Explorer
    // (`/v1/apps/{id}/events/list`) accepts `filter=workflow:<op>:<value>`
    // through `sauron_db::filter::EVENT_FILTERS`, and Task 6 bridges that route
    // through `from_legacy` too. Left off, every such bookmark would hit
    // `resolve_field`'s step-4 tag fallback and probe a TAG key named
    // `workflow` on `analytics_events.tags` — a 200 with the wrong rows, which
    // is worse than an error. `OPS_WORKFLOW` already matches what
    // `EVENT_FILTERS` grants the field (`OPS_STR` = eq/neq/contains), so no op
    // widening was needed with it.
    //
    // One entry, THREE lowerings now, and that is still fine: `Store` names
    // where the value lives *for the resource being lowered*, and each
    // `ResourceLower` switches on it independently. `IssuesLower` maps
    // `Store::Column("workflow")` to a correlated EXISTS (there is no such
    // column on `issues`); `OccurrencesLower` to `error_events.workflow_name`;
    // `EventsLower` to `analytics_events.workflow_name` — the last of the three
    // additionally carrying the `workflow_id IS NOT NULL` partial-index term on
    // its positive arms, because that is what the code it replaces measured as
    // worth 3,700x on the largest table in the system. Splitting this into
    // three dimensions with the same `name` would instead make `resolve_field`'s
    // lookup order-dependent.
    Dimension {
        name: "workflow",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("workflow"),
        ops: OPS_WORKFLOW,
        resources: R_ISSUE_OCC_EVENTS,
        index: IndexClass::Bounded,
    },
    // ---- issue-level rollups (S3: issue_dimensions) ----
    Dimension {
        name: "environment",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Rollup,
        ops: OPS_EQ,
        resources: R_ISSUES,
        index: IndexClass::Indexed,
    },
    Dimension {
        name: "release",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Rollup,
        ops: OPS_EQ,
        resources: R_ISSUES,
        index: IndexClass::Indexed,
    },
    Dimension {
        name: "handled",
        aliases: NO_ALIAS,
        ty: ValueType::Bool,
        store: Store::Rollup,
        ops: OPS_EQ,
        resources: R_ISSUES,
        index: IndexClass::Indexed,
    },
    // ---- error_events / occurrences ----
    Dimension {
        name: "handled",
        aliases: NO_ALIAS,
        ty: ValueType::Bool,
        store: Store::Column("handled"),
        ops: OPS_EQ,
        resources: R_OCC,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "environment",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("environment_id"),
        ops: OPS_EQ,
        resources: &[Resource::Occurrences, Resource::Events, Resource::Sessions],
        index: IndexClass::Indexed,
    },
    Dimension {
        name: "release",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("release"),
        ops: OPS_TEXT,
        resources: &[Resource::Occurrences, Resource::Events, Resource::Sessions],
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "distinctId",
        aliases: &["distinct_id"],
        ty: ValueType::Str,
        store: Store::Column("distinct_id"),
        ops: OPS_TEXT,
        resources: &[
            Resource::Occurrences,
            Resource::Events,
            Resource::Persons,
            Resource::Sessions,
            // Backed by `transactions_app_distinct_idx` (migration 000058), so
            // this keeps the `Indexed` class honest on this resource too.
            Resource::Transactions,
            // `issues` carries no `distinct_id` column: this lowers to a
            // correlated EXISTS into `error_events`, the same shape as
            // `workflow` (see `IssuesLower`). `Indexed` stays honest through
            // that indirection because `error_events_distinct_idx` is
            // `(app_id, distinct_id, occurred_at DESC)` — migration 000011
            // redefined it from migration 000001's `(project_id, …)` — and the
            // subquery re-asserts `e.app_id = issues.app_id`, so the index is
            // reachable from inside it. Verified with EXPLAIN, not assumed;
            // see the note in `resolve::effective_index`.
            Resource::Issues,
        ],
        index: IndexClass::Indexed,
    },
    // OPS_TEXT, not OPS_EQ: the legacy `EVENT_FILTERS` granted `session_id` the
    // full string operator set, so narrowing it here would reject shared URLs of
    // the form `filter=session_id:contains:…` outright.
    Dimension {
        name: "session",
        aliases: &["session_id"],
        ty: ValueType::Str,
        store: Store::Column("session_id"),
        ops: OPS_TEXT,
        // Transactions carry `session_id` and index it
        // (`transactions_app_session_idx`, migration 000013) — which is what
        // makes the Transactions list's Session column filterable rather than
        // merely readable.
        resources: &[
            Resource::Occurrences,
            Resource::Events,
            Resource::Sessions,
            Resource::Transactions,
        ],
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "deviceKey",
        aliases: &["device_key"],
        ty: ValueType::Str,
        store: Store::Column("device_key"),
        ops: OPS_EQ,
        // `Issues` lowers this to a correlated EXISTS into `error_events`,
        // where `error_events_app_device_env_idx`
        // (app_id, device_key, environment_id, occurred_at) is reachable by
        // prefix — the subquery already re-asserts `e.app_id = issues.app_id`,
        // which is that index's leading column. It used to name the narrower
        // `error_events_app_device_idx` (app_id, device_key); migration 0065
        // dropped that one as a strict prefix of this one, which serves the
        // same equality probe.
        resources: &[
            Resource::Occurrences,
            Resource::Devices,
            Resource::Sessions,
            Resource::Issues,
        ],
        index: IndexClass::Bounded,
    },
    // `R_ISSUE_OCC`, not `R_OCC`: `issues` has no `screen` column, so on that
    // resource this lowers to a correlated EXISTS into `error_events` — where
    // `Eq` can reach the partial `error_events_app_screen_time_idx` (it is
    // `WHERE screen IS NOT NULL`, which `screen = $1` implies) and
    // `Like`/`Contains` cannot, exactly as on Occurrences.
    Dimension {
        name: "screen",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("screen"),
        ops: OPS_TEXT,
        resources: R_ISSUE_OCC,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "symbolication",
        aliases: &["symbolication_status"],
        ty: ValueType::Enum(SYMBOLICATION),
        store: Store::Column("symbolication_status"),
        ops: OPS_EQ,
        resources: R_OCC,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "message",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("message"),
        ops: OPS_TEXT,
        resources: R_OCC,
        index: IndexClass::Scan,
    },
    // ---- JSON roots reachable by dynamic path ----
    Dimension {
        name: "context",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::JsonRoot {
            column: "context",
            prefix: "",
        },
        ops: OPS_TEXT,
        resources: &[Resource::Occurrences, Resource::Events, Resource::Sessions],
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "user",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::JsonRoot {
            column: "event_user",
            prefix: "",
        },
        ops: OPS_TEXT,
        resources: R_OCC,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "sdk",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::JsonRoot {
            column: "sdk",
            prefix: "",
        },
        ops: OPS_TEXT,
        resources: R_OCC,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "os",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::JsonRoot {
            column: "context",
            prefix: "os",
        },
        ops: OPS_TEXT,
        resources: R_OCC,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "browser",
        aliases: &["runtime"],
        ty: ValueType::Str,
        // The user-facing name and the stored key differ: `enrich.rs` writes the
        // browser under `context->runtime`, not `context->browser` (there is no
        // `browser` key in the enriched context at all). The dimension keeps the
        // name `browser` — with `runtime` as an alias — because that is the
        // familiar term, but the storage prefix must point at the key that
        // actually exists.
        store: Store::JsonRoot {
            column: "context",
            prefix: "runtime",
        },
        ops: OPS_TEXT,
        resources: R_OCC,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "device",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::JsonRoot {
            column: "context",
            prefix: "device",
        },
        ops: OPS_TEXT,
        resources: R_OCC,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "app",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::JsonRoot {
            column: "context",
            prefix: "app",
        },
        ops: OPS_TEXT,
        resources: R_OCC,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "contexts",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::JsonRoot {
            column: "contexts",
            prefix: "",
        },
        ops: OPS_TEXT,
        resources: R_OCC_EVENTS,
        index: IndexClass::Bounded,
    },
    // Transactions carry `extra` too (migration 0063), which is what makes
    // `extra.order_id:123` resolve on the transactions list. Still
    // `IndexClass::Bounded` there and deliberately unindexed in Postgres: the
    // probe is containment/ILIKE over freeform JSON of unbounded shape, and a
    // GIN on the highest-volume table would cost more write throughput than the
    // read buys.
    Dimension {
        name: "extra",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::JsonRoot {
            column: "extra",
            prefix: "",
        },
        ops: OPS_TEXT,
        resources: R_OCC_EVENTS_TX,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "properties",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::JsonRoot {
            column: "properties",
            prefix: "",
        },
        ops: OPS_TEXT,
        resources: R_EVENTS,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "traits",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::JsonRoot {
            column: "properties",
            prefix: "",
        },
        ops: OPS_TEXT,
        resources: R_PERSONS,
        index: IndexClass::Scan,
    },
    Dimension {
        name: "stack",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::JsonRoot {
            column: "stacktrace",
            prefix: "",
        },
        ops: OPS_TEXT,
        resources: R_OCC,
        index: IndexClass::Scan,
    },
    // ---- analytics events ----
    Dimension {
        name: "name",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("name"),
        ops: OPS_TEXT,
        resources: &[Resource::Events, Resource::Transactions],
        index: IndexClass::Indexed,
    },
    // ---- transactions ----
    Dimension {
        name: "op",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("op"),
        ops: OPS_TEXT,
        resources: R_TX,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "duration",
        aliases: &["duration_ms"],
        ty: ValueType::Duration,
        store: Store::Column("duration_ms"),
        ops: OPS_ORD,
        resources: &[Resource::Transactions, Resource::Sessions],
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "url",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("url"),
        ops: OPS_TEXT,
        resources: R_TX,
        index: IndexClass::Scan,
    },
    // Dotted canonical names, not a JSON root: these are real columns
    // (`transactions.http_status` / `.http_method`) and `lookup` matches the
    // full dotted string exactly.
    Dimension {
        name: "http.status",
        aliases: &["http_status"],
        ty: ValueType::Int,
        store: Store::Column("http_status"),
        ops: OPS_ORD,
        resources: R_TX,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "http.method",
        aliases: &["http_method"],
        ty: ValueType::Str,
        store: Store::Column("http_method"),
        ops: OPS_EQ,
        resources: R_TX,
        index: IndexClass::Bounded,
    },
    // ---- devices ----
    // `browser` on Devices is a real column, unlike the `context->browser` JSON
    // root used for Occurrences above — disjoint resources keep both legal.
    Dimension {
        name: "browser",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("browser"),
        ops: OPS_TEXT,
        resources: R_DEVICES,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "device.family",
        aliases: &["family"],
        ty: ValueType::Str,
        store: Store::Column("family"),
        ops: OPS_TEXT,
        resources: R_DEVICES,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "device.model",
        aliases: &["model"],
        ty: ValueType::Str,
        store: Store::Column("model"),
        ops: OPS_TEXT,
        resources: R_DEVICES,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "device.arch",
        aliases: &["arch"],
        ty: ValueType::Str,
        store: Store::Column("arch"),
        ops: OPS_TEXT,
        resources: R_DEVICES,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "os.name",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("os_name"),
        ops: OPS_TEXT,
        resources: R_DEVICES,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "os.version",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("os_version"),
        ops: OPS_TEXT,
        resources: R_DEVICES,
        index: IndexClass::Bounded,
    },
    // ---- sessions ----
    Dimension {
        name: "startedAt",
        aliases: &["started_at"],
        ty: ValueType::Timestamp,
        store: Store::Column("started_at"),
        ops: OPS_ORD,
        resources: R_SESSIONS,
        index: IndexClass::Indexed,
    },
    Dimension {
        name: "eventsCount",
        aliases: &["events_count"],
        ty: ValueType::Int,
        store: Store::Column("events_count"),
        ops: OPS_ORD,
        resources: R_SESSIONS,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "errorsCount",
        aliases: &["errors_count"],
        ty: ValueType::Int,
        store: Store::Column("errors_count"),
        ops: OPS_ORD,
        resources: R_SESSIONS,
        index: IndexClass::Bounded,
    },
];

/// Resources that carry a developer-supplied `tags` JSONB column.
const TAGGABLE: &[Resource] = &[
    Resource::Issues,
    Resource::Occurrences,
    Resource::Events,
    Resource::Transactions,
];

/// The synthetic dimension behind the `tag.<key>` prefix and the
/// `tag:<key>=<value>` escape hatch.
///
/// Deliberately NOT a member of `CATALOG` — it must never appear in
/// autocomplete or the generated docs table as a field literally named "tag".
///
/// It is reached only by those two EXPLICIT spellings. It used to also be where
/// any unrecognised field landed (spec §5 rule 3), which made every typo a
/// silent zero-row answer; `resolve_field` now rejects an unknown name instead.
pub const TAG_DIM: Dimension = Dimension {
    name: "tag",
    aliases: NO_ALIAS,
    ty: ValueType::Str,
    store: Store::Tag,
    ops: OPS_TEXT,
    resources: TAGGABLE,
    index: IndexClass::Indexed,
};

/// `Some` when this resource carries a developer-supplied `tags` column, and so
/// can answer a `tag.<key>` / `tag:<key>=<value>` predicate at all. Devices,
/// Persons and Sessions cannot, and are told so rather than being offered a tag
/// spelling that would match nothing.
///
/// Transactions joined the taggable set with migration 0063 — the `tags`
/// column and its `transactions_tags_gin` index exist, so the `Indexed` class
/// on [`TAG_DIM`] holds there too.
pub fn tag_dimension(r: Resource) -> Option<&'static Dimension> {
    if TAG_DIM.resources.contains(&r) {
        Some(&TAG_DIM)
    } else {
        None
    }
}

/// The synthetic dimension behind the `$label.<key>` / `@$label.<key>` prefix.
pub const LABEL_DIM: Dimension = Dimension {
    name: "$label",
    aliases: &["label"],
    ty: ValueType::Str,
    store: Store::Tag,
    ops: OPS_TEXT,
    resources: TAGGABLE,
    index: IndexClass::Indexed,
};

pub fn label_dimension(r: Resource) -> Option<&'static Dimension> {
    if LABEL_DIM.resources.contains(&r) {
        Some(&LABEL_DIM)
    } else {
        None
    }
}

pub fn dimensions_for(r: Resource) -> impl Iterator<Item = &'static Dimension> {
    CATALOG.iter().filter(move |d| d.resources.contains(&r))
}

/// Resolve a field name (canonical or alias) within a resource. Returns `None`
/// for unknown names — the resolver then tries the explicit `tag.<key>` prefix
/// and the JSON-path form, and rejects the name if neither applies.
pub fn lookup(field: &str, r: Resource) -> Option<&'static Dimension> {
    dimensions_for(r).find(|d| d.name == field || d.aliases.contains(&field))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_a_curated_field() {
        let d = lookup("level", Resource::Issues).unwrap();
        assert_eq!(d.name, "level");
        assert!(matches!(d.store, Store::Column("level")));
    }

    #[test]
    fn lookup_is_resource_scoped() {
        // `duration` is a transaction dimension and must not leak onto Issues.
        assert!(lookup("duration", Resource::Transactions).is_some());
        assert!(lookup("duration", Resource::Issues).is_none());
    }

    #[test]
    fn resolves_aliases() {
        // `distinctId` is the canonical spelling; the DB column name also works.
        let a = lookup("distinctId", Resource::Occurrences).unwrap();
        let b = lookup("distinct_id", Resource::Occurrences).unwrap();
        assert_eq!(a.name, b.name);
    }

    #[test]
    fn level_on_issues_is_the_column_not_the_rollup() {
        // Spec §6: exactly one meaning for `level:error` on Issues.
        let d = lookup("level", Resource::Issues).unwrap();
        assert!(matches!(d.store, Store::Column(_)));
        assert!(!matches!(d.store, Store::Rollup));
    }

    #[test]
    fn environment_on_issues_is_the_rollup() {
        let d = lookup("environment", Resource::Issues).unwrap();
        assert!(matches!(d.store, Store::Rollup));
        assert_eq!(d.index, IndexClass::Indexed);
    }

    #[test]
    fn json_roots_are_registered_for_dynamic_paths() {
        for root in ["extra", "contexts", "user", "os"] {
            assert!(
                lookup(root, Resource::Occurrences).is_some(),
                "missing JSON root `{root}`"
            );
        }
    }

    #[test]
    fn shorthands_cover_status_and_handled() {
        let names: Vec<_> = SHORTHANDS.iter().map(|s| s.keyword).collect();
        for k in ["unresolved", "resolved", "ignored", "handled", "unhandled"] {
            assert!(names.contains(&k), "missing shorthand `{k}`");
        }
    }

    #[test]
    fn handled_shorthands_target_the_handled_field() {
        let s = SHORTHANDS
            .iter()
            .find(|s| s.keyword == "unhandled")
            .unwrap();
        assert_eq!(s.field, "handled");
        assert_eq!(s.value, "false");
    }

    #[test]
    fn every_dimension_declares_at_least_one_op_and_resource() {
        for d in CATALOG {
            assert!(!d.ops.is_empty(), "`{}` declares no operators", d.name);
            assert!(
                !d.resources.is_empty(),
                "`{}` declares no resources",
                d.name
            );
        }
    }

    #[test]
    fn dimension_names_are_unique_within_a_resource() {
        for r in Resource::ALL {
            let mut seen = std::collections::HashSet::new();
            for d in dimensions_for(*r) {
                for key in std::iter::once(d.name).chain(d.aliases.iter().copied()) {
                    assert!(seen.insert(key), "`{key}` is declared twice for {:?}", r);
                }
            }
        }
    }

    #[test]
    fn enum_dimensions_list_their_options() {
        let d = lookup("is", Resource::Issues).unwrap();
        match d.ty {
            ValueType::Enum(opts) => assert!(opts.contains(&"unresolved")),
            _ => panic!("`is` should be an enum"),
        }
    }

    #[test]
    fn dimensions_for_filters_by_resource() {
        assert!(dimensions_for(Resource::Devices).any(|d| d.name == "browser"));
        assert!(!dimensions_for(Resource::Devices).any(|d| d.name == "culprit"));
    }

    #[test]
    fn dotted_names_resolve_as_whole_fields() {
        // `http.status` is a real column, matched exactly — not a JSON path.
        let d = lookup("http.status", Resource::Transactions).unwrap();
        assert!(matches!(d.store, Store::Column("http_status")));
    }

    #[test]
    fn browser_is_a_column_on_devices_and_json_on_occurrences() {
        assert!(matches!(
            lookup("browser", Resource::Devices).unwrap().store,
            Store::Column("browser")
        ));
        assert!(matches!(
            lookup("browser", Resource::Occurrences).unwrap().store,
            Store::JsonRoot {
                column: "context",
                ..
            }
        ));
    }

    #[test]
    fn tag_fallback_is_available_only_where_tags_exist() {
        assert!(tag_dimension(Resource::Issues).is_some());
        assert!(tag_dimension(Resource::Occurrences).is_some());
        assert!(tag_dimension(Resource::Events).is_some());
        // Joined the taggable set with migration 0063 (`transactions.tags` +
        // `transactions_tags_gin`).
        assert!(tag_dimension(Resource::Transactions).is_some());
        assert!(tag_dimension(Resource::Devices).is_none());
        assert!(tag_dimension(Resource::Persons).is_none());
        assert!(tag_dimension(Resource::Sessions).is_none());
    }

    #[test]
    fn extra_resolves_on_transactions_but_contexts_does_not() {
        // `extra` followed transactions in 0063; `contexts` deliberately did
        // not — a span that wants structure nests it inside `extra`.
        assert!(lookup("extra", Resource::Transactions).is_some());
        assert!(lookup("contexts", Resource::Transactions).is_none());
        // The event resources keep both.
        assert!(lookup("extra", Resource::Events).is_some());
        assert!(lookup("contexts", Resource::Occurrences).is_some());
    }

    #[test]
    fn tag_dim_is_not_in_the_public_catalog() {
        // It must not show up in autocomplete or the generated docs table.
        assert!(!CATALOG.iter().any(|d| std::ptr::eq(d, &TAG_DIM)));
        assert!(!CATALOG.iter().any(|d| std::ptr::eq(d, &LABEL_DIM)));
    }

    #[test]
    fn label_dimension_resolution() {
        assert!(label_dimension(Resource::Issues).is_some());
        assert!(label_dimension(Resource::Occurrences).is_some());
        assert!(label_dimension(Resource::Events).is_some());
        assert!(label_dimension(Resource::Sessions).is_none());
    }

    #[test]
    fn resolves_session_dimensions() {
        for dim_name in [
            "startedAt",
            "session",
            "distinctId",
            "deviceKey",
            "environment",
            "release",
            "eventsCount",
            "errorsCount",
            "duration",
            "context",
        ] {
            assert!(
                lookup(dim_name, Resource::Sessions).is_some(),
                "missing session dimension `{dim_name}`"
            );
        }
    }
}
