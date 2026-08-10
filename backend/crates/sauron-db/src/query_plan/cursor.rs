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
    };
    URL_SAFE_NO_PAD.encode(format!("{}|{}|{ty}:{val}", c.key, c.id))
}

/// Decode, and refuse a cursor minted under a sort other than `expected_key`
/// or carrying the wrong KIND of value for it.
///
/// `expect_temporal` is the sort's own `EventSort::is_temporal` /
/// `OccurrenceSort::is_temporal` (Issues has no textual ordering yet, so its
/// one call site always passes `true` — see its call site's comment). Taking
/// it as a parameter, the same way `expected_key` already is, keeps the enum
/// the single source of truth for which kind each column needs and means a
/// caller cannot serve a page built from a value of the wrong kind just by
/// forgetting to ask.
pub fn decode(s: &str, expected_key: &str, expect_temporal: bool) -> Result<Cursor, CursorError> {
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
        _ => return Err(CursorError::Malformed),
    };

    let got_temporal = matches!(value, CursorValue::Ts(_));
    if got_temporal != expect_temporal {
        return Err(CursorError::KindMismatch {
            key: key.to_string(),
            expected: if expect_temporal { "timestamp" } else { "text" },
            got: if got_temporal { "timestamp" } else { "text" },
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
    fn round_trips_a_timestamp_cursor() {
        let c = sample();
        assert_eq!(decode(&encode(&c), "occurred_at", true).unwrap(), c);
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
        assert_eq!(decode(&encode(&c), "name", false).unwrap(), c);
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
        let err = decode(&encode(&sample()), "name", true).unwrap_err();
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
        let CursorValue::Ts(ts) = decode(&encode(&c), "occurred_at", true).unwrap().value else {
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
        assert_eq!(decode(&encode(&c), "session_id", false).unwrap(), c);
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
        let err = decode(&s, "session_id", false).unwrap_err();
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
                decode(bad, "occurred_at", true).is_err(),
                "{bad} should not decode"
            );
        }
    }

    #[test]
    fn rejects_a_truncated_cursor() {
        let s = encode(&sample());
        assert!(decode(&s[..s.len() - 3], "occurred_at", true).is_err());
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
            decode(&s, "occurred_at", true).unwrap_err(),
            CursorError::Malformed
        );
    }
}
