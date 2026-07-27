//! Sauron search query language: lex → parse → resolve → classify.
//!
//! Deliberately free of database, async, and I/O dependencies. CI runs
//! `cargo test --workspace` with no Postgres or Redis service, so anything that
//! needs a live connection cannot be tested here. Keeping this crate pure is
//! what makes the grammar testable at all.

pub mod ast;
pub mod catalog;
pub mod cost;
pub mod error;
pub mod legacy;
pub mod parse;
pub mod render;
pub mod resolve;
pub mod token;

pub use ast::{MatchOp, Node, Predicate};
pub use catalog::{
    dimensions_for, lookup, tag_dimension, Dimension, IndexClass, Resource, Shorthand, Store,
    ValueType, CATALOG, SHORTHANDS, TAG_DIM,
};
pub use cost::{classify, Cost};
pub use error::QueryError;
pub use legacy::from_legacy;
pub use parse::parse;
pub use render::render;
pub use resolve::{resolve, ResolvedNode, ResolvedPredicate, TimeSpec, TypedValue};
pub use token::{lex, Token};
