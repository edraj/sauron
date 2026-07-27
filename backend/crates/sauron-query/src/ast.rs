//! The syntactic tree. Holds raw strings only — no Sauron field knowledge and
//! no typed values. `resolve` turns this into the semantic tree.

/// Comparison chosen for a predicate. Derived from the value's leading
/// characters during resolution, not during parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchOp {
    Eq,
    /// Not currently produced by the parser: `!field:value` yields `Not(Eq)`.
    /// Reserved for the planner, which lowers that into a NULL-safe `Ne` so rows
    /// where the column IS NULL are not silently dropped from a negated match.
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    /// `field:[a,b,c]`
    In,
    /// `has:field` — key existence, no value
    Has,
    /// Wildcard match from an unquoted value containing `*`
    Like,
    /// `field:~text` — LITERAL substring match. Distinct from `Like` because the
    /// value is not scanned for wildcards at all, so a `*` in the user's own data
    /// stays a `*`. This is what the pre-language `contains` operator meant, and
    /// the legacy bridge maps onto it to keep existing shared URLs returning the
    /// same rows.
    Contains,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Predicate {
    pub field: String,
    /// Raw value exactly as typed, minus surrounding quotes. Operator prefixes
    /// (`>`, `>=`, …) and list brackets are still present; the resolver strips them.
    pub value: String,
    /// True when the value was quoted, which makes `*` literal rather than a wildcard.
    pub quoted: bool,
    pub at: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    Pred(Predicate),
    Text(String),
}

/// Bounds that keep a hostile query from becoming a planner problem. Chosen to
/// be far above any hand-written query and far below anything that costs real time.
pub const MAX_DEPTH: usize = 8;
pub const MAX_TERMS: usize = 64;
