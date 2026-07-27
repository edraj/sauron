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
const NO_ALIAS: &[&str] = &[];

const R_ISSUES: &[Resource] = &[Resource::Issues];
const R_OCC: &[Resource] = &[Resource::Occurrences];
const R_ISSUE_OCC: &[Resource] = &[Resource::Issues, Resource::Occurrences];
const R_EVENTS: &[Resource] = &[Resource::Events];
const R_OCC_EVENTS: &[Resource] = &[Resource::Occurrences, Resource::Events];
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
        resources: R_OCC_EVENTS,
        index: IndexClass::Indexed,
    },
    Dimension {
        name: "release",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("release"),
        ops: OPS_TEXT,
        resources: R_OCC_EVENTS,
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "distinctId",
        aliases: &["distinct_id"],
        ty: ValueType::Str,
        store: Store::Column("distinct_id"),
        ops: OPS_TEXT,
        resources: &[Resource::Occurrences, Resource::Events, Resource::Persons],
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
        resources: &[Resource::Occurrences, Resource::Events, Resource::Sessions],
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "deviceKey",
        aliases: &["device_key"],
        ty: ValueType::Str,
        store: Store::Column("device_key"),
        ops: OPS_EQ,
        resources: &[Resource::Occurrences, Resource::Devices],
        index: IndexClass::Bounded,
    },
    Dimension {
        name: "screen",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::Column("screen"),
        ops: OPS_TEXT,
        resources: R_OCC,
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
    Dimension {
        name: "extra",
        aliases: NO_ALIAS,
        ty: ValueType::Str,
        store: Store::JsonRoot {
            column: "extra",
            prefix: "",
        },
        ops: OPS_TEXT,
        resources: R_OCC_EVENTS,
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
        resources: R_TX,
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
];

/// Resources that carry a developer-supplied `tags` JSONB column.
const TAGGABLE: &[Resource] = &[Resource::Issues, Resource::Occurrences, Resource::Events];

/// The synthetic dimension every unrecognised field resolves to (spec §5, rule 3).
/// Deliberately NOT a member of `CATALOG` — it must never appear in autocomplete
/// or the generated docs table as a field literally named "tag".
pub const TAG_DIM: Dimension = Dimension {
    name: "tag",
    aliases: NO_ALIAS,
    ty: ValueType::Str,
    store: Store::Tag,
    ops: OPS_TEXT,
    resources: TAGGABLE,
    index: IndexClass::Indexed,
};

/// `Some` when this resource supports the unknown-field-means-tag fallback.
/// Devices, Persons, Sessions and Transactions have no `tags` column, so an
/// unrecognised field there is a genuine error rather than a tag lookup.
pub fn tag_dimension(r: Resource) -> Option<&'static Dimension> {
    if TAG_DIM.resources.contains(&r) {
        Some(&TAG_DIM)
    } else {
        None
    }
}

pub fn dimensions_for(r: Resource) -> impl Iterator<Item = &'static Dimension> {
    CATALOG.iter().filter(move |d| d.resources.contains(&r))
}

/// Resolve a field name (canonical or alias) within a resource. Returns `None`
/// for unknown names — the resolver then falls back to a tag lookup.
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
        assert!(tag_dimension(Resource::Devices).is_none());
        assert!(tag_dimension(Resource::Persons).is_none());
        assert!(tag_dimension(Resource::Transactions).is_none());
    }

    #[test]
    fn tag_dim_is_not_in_the_public_catalog() {
        // It must not show up in autocomplete or the generated docs table.
        assert!(!CATALOG.iter().any(|d| std::ptr::eq(d, &TAG_DIM)));
    }
}
