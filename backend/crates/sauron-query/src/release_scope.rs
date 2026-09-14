//! The `?release=` query parameter, expressed as a query-node rewrite so it
//! composes with every other predicate the planner already understands.
//! `Unknown` (wire literal `none`) is `!has:release`, i.e. `release IS NULL`.

use crate::catalog::lookup;
use crate::{MatchOp, ResolvedNode, ResolvedPredicate, Resource, TypedValue};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseFilter {
    All,
    One(String),
    Unknown,
}

/// Builds a `ResolvedPredicate` directly rather than going through
/// `resolve::resolve_pred` — this is a server-side rewrite, not caller-supplied
/// text, so there is no `Node`/`Predicate` to parse and therefore no `ops` gate
/// to pass: that gate exists to tell a *user* their query used an operator the
/// field does not offer, and there is no user query here. The same bypass the
/// range-expansion path in `resolve.rs` takes for `Between`.
///
/// (As it happens both `OPS_EQ` and `OPS_TEXT` — the two op sets `release` is
/// declared with, on Issues and everywhere else respectively — do list `Has`,
/// so the `Unknown` rewrite below would pass the gate anyway. That is a
/// coincidence of the current catalog, not the reason this is safe.)
pub fn with_release(
    node: ResolvedNode,
    resource: Resource,
    filter: &ReleaseFilter,
) -> ResolvedNode {
    let dim = lookup("release", resource)
        .expect("every searched list resource carries a `release` dimension (catalog parity test)");
    let pred = |op: MatchOp, value: TypedValue| ResolvedPredicate {
        dim,
        path: None,
        op,
        value,
        at: 0,
        // `dim.index`, not `resolve::effective_index`: the latter is private
        // to `resolve.rs` and downgrades only `Store::Tag` on Issues and
        // `Store::Rollup` — neither applies to `release`'s `Store::Column` on
        // every resource this filter reaches, so the two agree here.
        index: dim.index,
    };
    let extra = match filter {
        ReleaseFilter::All => return node,
        ReleaseFilter::One(v) => ResolvedNode::Pred(pred(MatchOp::Eq, TypedValue::Str(v.clone()))),
        ReleaseFilter::Unknown => ResolvedNode::Not(Box::new(ResolvedNode::Pred(pred(
            MatchOp::Has,
            TypedValue::Absent,
        )))),
    };
    ResolvedNode::And(vec![node, extra])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse, resolve, MatchOp, ResolvedNode, Resource, TypedValue};

    // `level` (the brief's original pick) is `R_ISSUE_OCC` — Issues and
    // Occurrences only, not Events — so it does not resolve here. `workflow`
    // is `R_ISSUE_OCC_EVENTS` and serves the same purpose: an arbitrary
    // pre-existing predicate for `with_release` to wrap.
    fn base() -> ResolvedNode {
        resolve(&parse("workflow:migration").unwrap(), Resource::Events).unwrap()
    }

    #[test]
    fn all_is_identity() {
        let n = base();
        assert_eq!(
            format!(
                "{:?}",
                with_release(n.clone(), Resource::Events, &ReleaseFilter::All)
            ),
            format!("{n:?}")
        );
    }

    #[test]
    fn one_ands_an_eq_predicate() {
        let out = with_release(
            base(),
            Resource::Events,
            &ReleaseFilter::One("1.4.0".into()),
        );
        let ResolvedNode::And(parts) = out else {
            panic!("expected And")
        };
        let ResolvedNode::Pred(p) = &parts[1] else {
            panic!("expected Pred")
        };
        assert_eq!(p.dim.name, "release");
        assert_eq!(p.op, MatchOp::Eq);
        assert_eq!(p.value, TypedValue::Str("1.4.0".into()));
    }

    #[test]
    fn unknown_ands_a_negated_has() {
        let out = with_release(base(), Resource::Sessions, &ReleaseFilter::Unknown);
        let ResolvedNode::And(parts) = out else {
            panic!("expected And")
        };
        let ResolvedNode::Not(inner) = &parts[1] else {
            panic!("expected Not")
        };
        let ResolvedNode::Pred(p) = &**inner else {
            panic!("expected Pred")
        };
        assert_eq!(p.op, MatchOp::Has);
    }

    #[test]
    fn every_list_resource_has_a_release_dimension() {
        for r in [
            Resource::Issues,
            Resource::Occurrences,
            Resource::Events,
            Resource::Sessions,
            Resource::Transactions,
        ] {
            assert!(crate::catalog::lookup("release", r).is_some(), "{r:?}");
        }
    }
}
