//! Ad-hoc list filtering: a small whitelisted `field:op:value` model shared by
//! the API routes (which parse untrusted input) and the repo (which folds the
//! validated result into diesel boxed queries).

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op { Eq, Neq, Contains, Gt, Lt }

impl Op {
    pub fn parse(s: &str) -> Option<Op> {
        Some(match s {
            "eq" => Op::Eq,
            "neq" => Op::Neq,
            "contains" => Op::Contains,
            "gt" => Op::Gt,
            "lt" => Op::Lt,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType { Str, Enum, Num, Tag }

pub struct FieldSpec {
    pub key: &'static str,
    pub ty: FieldType,
    pub ops: &'static [Op],
    pub options: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFilter {
    pub field: &'static str,
    pub op: Op,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterError {
    Malformed,
    UnknownField(String),
    BadOp { field: String, op: String },
    BadValue { field: String },
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilterError::Malformed => write!(f, "filter must be field:op:value"),
            FilterError::UnknownField(x) => write!(f, "unknown filter field: {x}"),
            FilterError::BadOp { field, op } => write!(f, "operator {op} not allowed for field {field}"),
            FilterError::BadValue { field } => write!(f, "invalid value for filter field {field}"),
        }
    }
}

/// Parse + validate raw `field:op:value` strings against `allow`. Splits on the
/// first two ':' only (values may contain ':'). Rejects unknown fields,
/// disallowed operators, out-of-range enum values, and non-numeric numbers.
pub fn parse_filters(raw: &[String], allow: &[FieldSpec]) -> Result<Vec<ParsedFilter>, FilterError> {
    let mut out = Vec::with_capacity(raw.len());
    for item in raw {
        let mut parts = item.splitn(3, ':');
        let field = parts.next().unwrap_or("");
        let op_s = parts.next().ok_or(FilterError::Malformed)?;
        let raw_value = parts.next().ok_or(FilterError::Malformed)?;
        // The frontend encodes the value with `encodeURIComponent` before putting
        // it in the `filter=` query param; axum-extra's query parsing only
        // reverses the transport-level URL-encoding, so we still need to reverse
        // the frontend's encoding here. Use pure percent-decoding (not
        // form-urlencoded) so a literal `+` is preserved, mirroring
        // `decodeURIComponent`.
        let value = percent_encoding::percent_decode_str(raw_value)
            .decode_utf8_lossy()
            .into_owned();

        let spec = allow
            .iter()
            .find(|f| f.key == field)
            .ok_or_else(|| FilterError::UnknownField(field.to_string()))?;
        let op = Op::parse(op_s).ok_or_else(|| FilterError::BadOp {
            field: field.to_string(),
            op: op_s.to_string(),
        })?;
        if !spec.ops.contains(&op) {
            return Err(FilterError::BadOp { field: field.to_string(), op: op_s.to_string() });
        }
        match spec.ty {
            FieldType::Num => {
                value.parse::<i64>().map_err(|_| FilterError::BadValue { field: field.to_string() })?;
            }
            FieldType::Enum => {
                if !spec.options.contains(&value.as_str()) {
                    return Err(FilterError::BadValue { field: field.to_string() });
                }
            }
            FieldType::Str => {}
            FieldType::Tag => {
                // Value is `key=value`; split on the FIRST '=' and require both sides.
                match value.split_once('=') {
                    Some((k, v)) if !k.is_empty() && !v.is_empty() => {}
                    _ => return Err(FilterError::BadValue { field: field.to_string() }),
                }
            }
        }
        out.push(ParsedFilter { field: spec.key, op, value });
    }
    Ok(out)
}

const OPS_STR: &[Op] = &[Op::Eq, Op::Neq, Op::Contains];
const OPS_ENUM: &[Op] = &[Op::Eq, Op::Neq];
const OPS_NUM: &[Op] = &[Op::Eq, Op::Gt, Op::Lt];
const OPS_TAG: &[Op] = &[Op::Eq, Op::Contains];
const NO_OPTS: &[&str] = &[];

pub const ISSUE_FILTERS: &[FieldSpec] = &[
    FieldSpec { key: "level", ty: FieldType::Enum, ops: OPS_ENUM, options: &["debug", "info", "warning", "error", "fatal"] },
    FieldSpec { key: "status", ty: FieldType::Enum, ops: OPS_ENUM, options: &["unresolved", "resolved", "ignored"] },
    FieldSpec { key: "type", ty: FieldType::Str, ops: OPS_STR, options: NO_OPTS },
    FieldSpec { key: "culprit", ty: FieldType::Str, ops: OPS_STR, options: NO_OPTS },
    FieldSpec { key: "times_seen", ty: FieldType::Num, ops: OPS_NUM, options: NO_OPTS },
    FieldSpec { key: "users_seen", ty: FieldType::Num, ops: OPS_NUM, options: NO_OPTS },
    FieldSpec { key: "tag", ty: FieldType::Tag, ops: OPS_TAG, options: NO_OPTS },
];

// `environment` is validated as a free string here (valid values are per-app and
// dynamic); the repo resolves the name to an environment_id at query time.
pub const EVENT_FILTERS: &[FieldSpec] = &[
    FieldSpec { key: "name", ty: FieldType::Str, ops: OPS_STR, options: NO_OPTS },
    FieldSpec { key: "distinct_id", ty: FieldType::Str, ops: OPS_STR, options: NO_OPTS },
    FieldSpec { key: "session_id", ty: FieldType::Str, ops: OPS_STR, options: NO_OPTS },
    FieldSpec { key: "environment", ty: FieldType::Str, ops: OPS_ENUM, options: NO_OPTS },
    FieldSpec { key: "release", ty: FieldType::Str, ops: OPS_STR, options: NO_OPTS },
    FieldSpec { key: "tag", ty: FieldType::Tag, ops: OPS_TAG, options: NO_OPTS },
];

// Per-error-event occurrences (issue detail). Only the developer `tag` is
// filterable per-occurrence; issue-group fields (level/status/...) live on the
// issue, not the individual event.
pub const ERROR_EVENT_FILTERS: &[FieldSpec] = &[
    FieldSpec { key: "tag", ty: FieldType::Tag, ops: OPS_TAG, options: NO_OPTS },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_filters() {
        let raw = vec!["level:eq:error".to_string(), "times_seen:gt:100".to_string()];
        let got = parse_filters(&raw, ISSUE_FILTERS).unwrap();
        assert_eq!(got, vec![
            ParsedFilter { field: "level", op: Op::Eq, value: "error".into() },
            ParsedFilter { field: "times_seen", op: Op::Gt, value: "100".into() },
        ]);
    }

    #[test]
    fn value_may_contain_colons() {
        let got = parse_filters(&["culprit:contains:foo:bar".to_string()], ISSUE_FILTERS).unwrap();
        assert_eq!(got[0].value, "foo:bar");
    }

    #[test]
    fn percent_decodes_encoded_slash() {
        let got = parse_filters(&["culprit:contains:foo%2Fbar".to_string()], ISSUE_FILTERS).unwrap();
        assert_eq!(got[0].value, "foo/bar");
    }

    #[test]
    fn percent_decodes_encoded_at_sign() {
        let got = parse_filters(
            &["distinct_id:eq:user%40example.com".to_string()],
            EVENT_FILTERS,
        )
        .unwrap();
        assert_eq!(got[0].value, "user@example.com");
    }

    #[test]
    fn rejects_unknown_field() {
        assert_eq!(
            parse_filters(&["nope:eq:x".to_string()], ISSUE_FILTERS),
            Err(FilterError::UnknownField("nope".into()))
        );
    }

    #[test]
    fn rejects_disallowed_op() {
        // `contains` is not allowed on the enum field `level`
        assert!(matches!(
            parse_filters(&["level:contains:err".to_string()], ISSUE_FILTERS),
            Err(FilterError::BadOp { .. })
        ));
    }

    #[test]
    fn rejects_bad_enum_value() {
        assert!(matches!(
            parse_filters(&["status:eq:banana".to_string()], ISSUE_FILTERS),
            Err(FilterError::BadValue { .. })
        ));
    }

    #[test]
    fn rejects_non_numeric() {
        assert!(matches!(
            parse_filters(&["times_seen:gt:lots".to_string()], ISSUE_FILTERS),
            Err(FilterError::BadValue { .. })
        ));
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(
            parse_filters(&["level=error".to_string()], ISSUE_FILTERS),
            Err(FilterError::Malformed)
        );
    }

    #[test]
    fn parses_tag_filter() {
        let got = parse_filters(&["tag:eq:region=eu".to_string()], ISSUE_FILTERS).unwrap();
        assert_eq!(got, vec![ParsedFilter { field: "tag", op: Op::Eq, value: "region=eu".into() }]);
        let got2 = parse_filters(&["tag:contains:feature=check".to_string()], EVENT_FILTERS).unwrap();
        assert_eq!(got2[0].field, "tag");
        assert_eq!(got2[0].op, Op::Contains);
        assert_eq!(got2[0].value, "feature=check");
    }

    #[test]
    fn tag_value_keeps_extra_equals() {
        // Only the FIRST '=' splits key/value; the rest belongs to the value.
        let got = parse_filters(&["tag:eq:expr=a=b".to_string()], ISSUE_FILTERS).unwrap();
        assert_eq!(got[0].value, "expr=a=b");
    }

    #[test]
    fn rejects_tag_without_equals() {
        assert!(matches!(
            parse_filters(&["tag:eq:region".to_string()], ISSUE_FILTERS),
            Err(FilterError::BadValue { .. })
        ));
    }

    #[test]
    fn rejects_tag_empty_key_or_value() {
        assert!(matches!(
            parse_filters(&["tag:eq:=eu".to_string()], ISSUE_FILTERS),
            Err(FilterError::BadValue { .. })
        ));
        assert!(matches!(
            parse_filters(&["tag:eq:region=".to_string()], ISSUE_FILTERS),
            Err(FilterError::BadValue { .. })
        ));
    }

    #[test]
    fn rejects_tag_disallowed_op() {
        assert!(matches!(
            parse_filters(&["tag:gt:region=eu".to_string()], ISSUE_FILTERS),
            Err(FilterError::BadOp { .. })
        ));
    }
}
