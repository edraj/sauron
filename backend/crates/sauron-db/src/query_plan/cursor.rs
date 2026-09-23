//! Opaque cursor for keyset pagination over a `(key, value, id)` triple.
//!
//! Opaque, not secret. It encodes only values the caller just received in the
//! response body, so there is nothing to hide and nothing to sign — it is
//! base64url purely so clients treat it as a token to echo back rather than a
//! structure to build themselves. Every list this slice touches orders by a
//! column with `id` as the tiebreaker, so one shape serves all of them.
//!
//! A cursor is a position within ONE ordering, so it carries the `key` it was
//! minted under alongside the `value` and `id`. Replayed against a different
//! column, the server would compare it to values of another type or meaning
//! and hand back wrong rows behind an HTTP 200 — nothing downstream could
//! tell. `decode` closes that off by taking the key the caller is about to
//! page by as a parameter and rejecting a mismatch outright, rather than
//! trusting every call site to remember a separate check.
//!
//! The key and the value's `t`/`s` type tag are independent fields on the
//! wire, so matching the key alone is not enough: a `session_id|<uuid>|t:…`
//! cursor passes the key check and still carries a timestamp where that
//! column's ordering needs text. `decode` also takes the sort's
//! `is_temporal` for exactly the same reason it takes the key — a check
//! left to each of three (and growing) call sites to remember separately is
//! a check that gets forgotten at exactly one of them.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorValue {
    Ts(DateTime<Utc>),
    Text(String),
    /// A count or other integer ordering — `issues.times_seen`/`users_seen`.
    Int(i64),
}

/// Which kind of value a sort column's cursor must carry.
///
/// Replaces the `expect_temporal: bool` [`decode`] used to take: two kinds fit
/// a bool, three do not, and a bool that silently lumped integers in with
/// text would let a forged `s:` payload page an integer ordering. Each sort
/// enum answers this through its own `cursor_kind()`, so the route never
/// spells the kind by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorKind {
    Timestamp,
    Text,
    Integer,
}

impl CursorKind {
    pub fn of(v: &CursorValue) -> Self {
        match v {
            CursorValue::Ts(_) => CursorKind::Timestamp,
            CursorValue::Text(_) => CursorKind::Text,
            CursorValue::Int(_) => CursorKind::Integer,
        }
    }

    /// The word the mismatch error uses for this kind.
    pub fn name(self) -> &'static str {
        match self {
            CursorKind::Timestamp => "timestamp",
            CursorKind::Text => "text",
            CursorKind::Integer => "integer",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    /// The sort column this position is a position WITHIN.
    pub key: String,
    pub value: CursorValue,
    pub id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorError {
    Malformed,
    BadTimestamp,
    BadUuid,
    KeyMismatch {
        expected: String,
        got: String,
    },
    /// The key matched, but the value's `t`/`s` wire tag does not match the
    /// KIND the caller says this sort requires. Key and type tag are
    /// independent fields on the wire — nothing but this check stops a
    /// `session_id|<uuid>|t:…` cursor from passing [`CursorError::KeyMismatch`]
    /// above and still carrying a `Ts` where `session_id`'s ordering needs
    /// `Text`. Read through `repo.rs`'s `ts_of`/`text_of` total fallback, that
    /// used to silently produce `UNIX_EPOCH` or `""` — a wrong-but-valid
    /// position instead of an error.
    KindMismatch {
        key: String,
        expected: &'static str,
        got: &'static str,
    },
}

impl std::fmt::Display for CursorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CursorError::Malformed => f.write_str("cursor is not a valid pagination token"),
            CursorError::BadTimestamp => f.write_str("cursor timestamp is invalid"),
            CursorError::BadUuid => f.write_str("cursor id is invalid"),
            CursorError::KeyMismatch { expected, got } => write!(
                f,
                "this cursor pages a list sorted by `{got}`, but the request sorts by \
                 `{expected}`; start from the first page after changing the sort"
            ),
            CursorError::KindMismatch { key, expected, got } => write!(
                f,
                "this cursor carries a {got} value, but sorting by `{key}` requires a \
                 {expected} value; the cursor does not match this sort — start from \
                 the first page"
            ),
        }
    }
}

/// `<key>|<uuid>|<type>:<value>`, base64url without padding.
///
/// The value is LAST and unescaped so it may contain the delimiter — event
/// names and session ids routinely do. Key and id are fixed-shape and parse
/// off the front, leaving the remainder to be taken whole.
pub fn encode(c: &Cursor) -> String {
    let (ty, val) = match &c.value {
        CursorValue::Ts(ts) => ("t", ts.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()),
        CursorValue::Text(s) => ("s", s.clone()),
        CursorValue::Int(n) => ("i", n.to_string()),
    };
    URL_SAFE_NO_PAD.encode(format!("{}|{}|{ty}:{val}", c.key, c.id))
}

/// Decode, and refuse a cursor minted under a sort other than `expected_key`
/// or carrying the wrong KIND of value for it.
///
/// `expect` is the sort's own `cursor_kind()` (`EventSort`, `OccurrenceSort`,
/// `TransactionSort`, `IssueSort` — all four expose it). Taking it as a
/// parameter, the same way `expected_key` already is, keeps the enum the
/// single source of truth for which kind each column needs and means a
/// caller cannot serve a page built from a value of the wrong kind just by
/// forgetting to ask.
pub fn decode(s: &str, expected_key: &str, expect: CursorKind) -> Result<Cursor, CursorError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| CursorError::Malformed)?;
    let text = String::from_utf8(bytes).map_err(|_| CursorError::Malformed)?;

    let (key, rest) = text.split_once('|').ok_or(CursorError::Malformed)?;
    let (id_s, payload) = rest.split_once('|').ok_or(CursorError::Malformed)?;
    let id = Uuid::parse_str(id_s).map_err(|_| CursorError::BadUuid)?;

    if key != expected_key {
        return Err(CursorError::KeyMismatch {
            expected: expected_key.to_string(),
            got: key.to_string(),
        });
    }

    let (ty, raw) = payload.split_once(':').ok_or(CursorError::Malformed)?;
    let value = match ty {
        "t" => CursorValue::Ts(
            DateTime::parse_from_rfc3339(raw)
                .map_err(|_| CursorError::BadTimestamp)?
                .with_timezone(&Utc),
        ),
        "s" => CursorValue::Text(raw.to_string()),
        "i" => CursorValue::Int(raw.parse().map_err(|_| CursorError::Malformed)?),
        _ => return Err(CursorError::Malformed),
    };

    let got = CursorKind::of(&value);
    if got != expect {
        return Err(CursorError::KindMismatch {
            key: key.to_string(),
            expected: expect.name(),
            got: got.name(),
        });
    }

    Ok(Cursor {
        key: key.to_string(),
        value,
        id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample() -> Cursor {
        Cursor {
            key: "occurred_at".into(),
            value: CursorValue::Ts(Utc.with_ymd_and_hms(2026, 8, 9, 12, 30, 45).unwrap()),
            id: Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
        }
    }

    #[test]
    fn round_trips_an_integer_cursor() {
        let c = Cursor {
            key: "times_seen".into(),
            value: CursorValue::Int(4210),
            id: Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
        };
        assert_eq!(
            decode(&encode(&c), "times_seen", CursorKind::Integer).unwrap(),
            c
        );
        // Negative and zero counts never occur, but the encoding must not
        // depend on that: a cursor is a position, not a validated count.
        let z = Cursor {
            value: CursorValue::Int(0),
            ..c.clone()
        };
        assert_eq!(
            decode(&encode(&z), "times_seen", CursorKind::Integer).unwrap(),
            z
        );
    }

    /// A forged `t:` payload under an integer ordering is refused the same way
    /// a forged `s:` one is under a timestamp ordering — the kind check is
    /// three-way, not "temporal or not".
    #[test]
    fn an_integer_ordering_refuses_a_timestamp_or_text_payload() {
        let ts = sample();
        let s = encode(&Cursor {
            key: "times_seen".into(),
            ..ts
        });
        let err = decode(&s, "times_seen", CursorKind::Integer).unwrap_err();
        assert!(
            matches!(
                err,
                CursorError::KindMismatch {
                    expected: "integer",
                    got: "timestamp",
                    ..
                }
            ),
            "{err:?}"
        );
        let text = Cursor {
            key: "times_seen".into(),
            value: CursorValue::Text("4210".into()),
            id: Uuid::nil(),
        };
        let err = decode(&encode(&text), "times_seen", CursorKind::Integer).unwrap_err();
        assert!(
            matches!(
                err,
                CursorError::KindMismatch {
                    expected: "integer",
                    got: "text",
                    ..
                }
            ),
            "{err:?}"
        );
        // And a non-numeric `i:` payload is malformed, not silently zero.
        let forged = URL_SAFE_NO_PAD.encode(format!("times_seen|{}|i:abc", Uuid::nil()));
        assert_eq!(
            decode(&forged, "times_seen", CursorKind::Integer).unwrap_err(),
            CursorError::Malformed
        );
    }

    #[test]
    fn round_trips_a_timestamp_cursor() {
        let c = sample();
        assert_eq!(
            decode(&encode(&c), "occurred_at", CursorKind::Timestamp).unwrap(),
            c
        );
    }

    #[test]
    fn round_trips_a_text_cursor() {
        let c = Cursor {
            key: "name".into(),
            value: CursorValue::Text("checkout|started".into()),
            id: sample().id,
        };
        // The delimiter appears INSIDE the value here on purpose: a text
        // cursor that split naively would truncate at the first `|` and page
        // from the wrong position.
        assert_eq!(decode(&encode(&c), "name", CursorKind::Text).unwrap(), c);
    }

    #[test]
    fn refuses_a_cursor_minted_under_a_different_sort() {
        // The defect this exists to stop: a cursor is a position within ONE
        // ordering. Compared against another column it yields wrong rows and
        // HTTP 200, which nothing downstream can detect.
        //
        // `expect_temporal: true` here is irrelevant to the outcome — the key
        // check runs, and fails, before the kind check ever would — but it is
        // what a real `name`-vs-`occurred_at` mismatch would carry from the
        // `occurred_at` side, so it is the realistic value to pin.
        let err = decode(&encode(&sample()), "name", CursorKind::Timestamp).unwrap_err();
        assert_eq!(
            err,
            CursorError::KeyMismatch {
                expected: "name".into(),
                got: "occurred_at".into()
            }
        );
    }

    #[test]
    fn preserves_sub_second_precision() {
        let c = Cursor {
            value: CursorValue::Ts(Utc.timestamp_micros(1_786_000_000_123_456).unwrap()),
            ..sample()
        };
        let CursorValue::Ts(ts) = decode(&encode(&c), "occurred_at", CursorKind::Timestamp)
            .unwrap()
            .value
        else {
            panic!("timestamp cursor decoded as text");
        };
        assert_eq!(CursorValue::Ts(ts), c.value);
    }

    #[test]
    fn an_empty_text_value_survives_the_round_trip() {
        // Nullable columns are coalesced to `""` before they reach the cursor,
        // so the empty string is a real position, not an absent one.
        let c = Cursor {
            key: "session_id".into(),
            value: CursorValue::Text(String::new()),
            id: sample().id,
        };
        assert_eq!(
            decode(&encode(&c), "session_id", CursorKind::Text).unwrap(),
            c
        );
    }

    #[test]
    fn refuses_a_cursor_whose_value_kind_does_not_match_the_sort() {
        // The defect this exists to stop, and the exact example from the
        // review that found it: `key` and the value's `t`/`s` type tag are
        // independent fields on the wire, so a cursor can pass the key check
        // above and still carry the wrong KIND of value for it — here, a
        // `session_id` cursor (a text column) hand-built with a `t:` tag.
        // Read through `repo.rs`'s `ts_of`/`text_of` total fallback, that used
        // to silently produce `UNIX_EPOCH` rather than an error: a
        // wrong-but-valid position, not a 400.
        let raw = format!("session_id|{}|t:1970-01-01T00:00:00Z", sample().id);
        let s = URL_SAFE_NO_PAD.encode(raw);
        let err = decode(&s, "session_id", CursorKind::Text).unwrap_err();
        assert_eq!(
            err,
            CursorError::KindMismatch {
                key: "session_id".into(),
                expected: "text",
                got: "timestamp",
            }
        );
    }

    #[test]
    fn is_url_safe() {
        // It travels in a query string; + and / would need escaping and the
        // padding = is a routine source of double-encoding bugs.
        let s = encode(&sample());
        assert!(
            !s.contains('+') && !s.contains('/') && !s.contains('='),
            "got {s}"
        );
    }

    #[test]
    fn rejects_garbage_rather_than_panicking() {
        // `decode` now parses strictly MORE structure than before — two
        // delimiters and a type tag, not one delimiter — so malformed-input
        // handling matters more than it did, not less. A cursor arrives in a
        // query string and so is attacker-reachable; this is the test that
        // keeps a malformed one a 400 instead of a panic turned 500.
        for bad in ["", "!!!!", "Zm9v", "e30", "########"] {
            assert!(
                decode(bad, "occurred_at", CursorKind::Timestamp).is_err(),
                "{bad} should not decode"
            );
        }
    }

    #[test]
    fn rejects_a_truncated_cursor() {
        let s = encode(&sample());
        assert!(decode(&s[..s.len() - 3], "occurred_at", CursorKind::Timestamp).is_err());
    }

    #[test]
    fn rejects_an_unknown_type_tag() {
        // `encode` only ever emits `t` or `s`. Hand-build the shape a
        // corrupted cursor — or a third variant that doesn't exist yet —
        // would take, so an unrecognised tag is proved to come back
        // `Malformed` rather than panicking on the unmatched arm or being
        // silently misread as one of the two known types.
        let raw = format!("occurred_at|{}|x:whatever", sample().id);
        let s = URL_SAFE_NO_PAD.encode(raw);
        assert_eq!(
            decode(&s, "occurred_at", CursorKind::Timestamp).unwrap_err(),
            CursorError::Malformed
        );
    }
}
