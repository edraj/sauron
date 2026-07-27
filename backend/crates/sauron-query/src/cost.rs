//! Cost classification. The planner uses this to decide whether a query may run
//! over a long window or must have its window clamped.
//!
//! Replaces `sauron_db::repo::MAX_PAYLOAD_SEARCH_DAYS`, a constant that is
//! currently unreachable because every route passes an explicit `since`. A guard
//! that never fires is worse than no guard, because it reads as protection.

use crate::ast::MatchOp;
use crate::catalog::{IndexClass, Store};
use crate::resolve::{ResolvedNode, ResolvedPredicate};

/// Ordered cheapest to most expensive; `Ord` is what makes the `min`/`max`
/// combination rules below read directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cost {
    Indexed,
    Bounded,
    Scan,
}

impl From<IndexClass> for Cost {
    fn from(i: IndexClass) -> Cost {
        match i {
            IndexClass::Indexed => Cost::Indexed,
            IndexClass::Bounded => Cost::Bounded,
            IndexClass::Scan => Cost::Scan,
        }
    }
}

fn predicate_cost(p: &ResolvedPredicate) -> Cost {
    let base = Cost::from(p.index);
    match p.op {
        // No trigram index exists, so any wildcard or substring scan reads every
        // candidate row.
        MatchOp::Like | MatchOp::Contains => Cost::Scan,
        // An inequality cannot seek; it can only be checked per row.
        MatchOp::Ne => base.max(Cost::Bounded),
        // The tags GIN is jsonb_path_ops, which serves @> containment only —
        // key existence is not answerable from it.
        MatchOp::Has if matches!(p.dim.store, Store::Tag) => base.max(Cost::Bounded),
        _ => base,
    }
}

pub fn classify(node: &ResolvedNode) -> Cost {
    match node {
        // An indexed child bounds the candidate set for its siblings.
        ResolvedNode::And(v) => v.iter().map(classify).min().unwrap_or(Cost::Indexed),
        // Every branch must be evaluated, so the worst one dominates.
        ResolvedNode::Or(v) => v.iter().map(classify).max().unwrap_or(Cost::Indexed),
        // Negation can never seek, but it is no worse than the inner cost.
        ResolvedNode::Not(b) => classify(b).max(Cost::Bounded),
        ResolvedNode::Pred(p) => predicate_cost(p),
        ResolvedNode::Text(_) => Cost::Scan,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Resource;
    use crate::parse::parse;
    use crate::resolve::resolve;

    fn cost(q: &str, r: Resource) -> Cost {
        classify(&resolve(&parse(q).unwrap(), r).unwrap())
    }

    #[test]
    fn cost_is_ordered() {
        assert!(Cost::Indexed < Cost::Bounded);
        assert!(Cost::Bounded < Cost::Scan);
    }

    #[test]
    fn indexed_field_with_equality_is_indexed() {
        assert_eq!(cost("is:unresolved", Resource::Issues), Cost::Indexed);
    }

    #[test]
    fn unindexed_field_is_bounded() {
        assert_eq!(cost("culprit:handler", Resource::Issues), Cost::Bounded);
    }

    #[test]
    fn wildcard_never_uses_an_index() {
        // No pg_trgm in this codebase — spec §3 non-goal.
        assert_eq!(cost("distinctId:u_*", Resource::Occurrences), Cost::Scan);
    }

    #[test]
    fn literal_substring_is_a_scan() {
        // Same reason as a wildcard: no trigram index exists.
        assert_eq!(cost("culprit:~handler", Resource::Issues), Cost::Scan);
    }

    #[test]
    fn free_text_is_a_scan() {
        assert_eq!(cost("timeout", Resource::Issues), Cost::Scan);
    }

    #[test]
    fn and_takes_the_cheapest_child() {
        // The indexed status predicate bounds the set; `title` is then per-row.
        assert_eq!(
            cost("is:unresolved title:*boom*", Resource::Issues),
            Cost::Indexed
        );
    }

    #[test]
    fn or_takes_the_most_expensive_child() {
        // This is the OR footgun the guard exists to catch.
        assert_eq!(
            cost("is:unresolved OR title:*boom*", Resource::Issues),
            Cost::Scan
        );
    }

    #[test]
    fn negation_never_beats_bounded() {
        assert_eq!(cost("!is:resolved", Resource::Issues), Cost::Bounded);
    }

    #[test]
    fn negated_scan_stays_a_scan() {
        assert_eq!(cost("!title:*boom*", Resource::Issues), Cost::Scan);
    }

    #[test]
    fn empty_query_is_indexed() {
        assert_eq!(cost("", Resource::Issues), Cost::Indexed);
    }

    #[test]
    fn tag_equality_is_indexed_but_tag_existence_is_not() {
        // `tags` carries a jsonb_path_ops GIN, which serves @> containment only.
        // On Occurrences the `tags` column exists, so equality is a direct GIN
        // seek — unlike on Issues (see `tag_equality_is_indexed_on_occurrences_
        // but_not_on_issues` below), where there is no `tags` column at all.
        assert_eq!(
            cost("checkout_step:payment", Resource::Occurrences),
            Cost::Indexed
        );
        assert_eq!(cost("has:checkout_step", Resource::Issues), Cost::Bounded);
    }

    #[test]
    fn tag_equality_is_indexed_on_occurrences_but_not_on_issues() {
        // Occurrences hits the tags GIN directly.
        assert_eq!(
            cost("checkout_step:payment", Resource::Occurrences),
            Cost::Indexed
        );
        // Issues has no tags column, so this is a correlated EXISTS per candidate.
        assert_eq!(
            cost("checkout_step:payment", Resource::Issues),
            Cost::Bounded
        );
    }

    #[test]
    fn rollup_dimensions_are_not_yet_indexed() {
        // `issue_dimensions` does not exist until a later slice.
        assert_eq!(
            cost("environment:production", Resource::Issues),
            Cost::Bounded
        );
    }

    #[test]
    fn nested_groups_compose() {
        // AND of (indexed) with (OR of two scans) → the AND still bounds it.
        assert_eq!(
            cost("is:unresolved (title:*a* OR culprit:*b*)", Resource::Issues),
            Cost::Indexed
        );
    }
}
