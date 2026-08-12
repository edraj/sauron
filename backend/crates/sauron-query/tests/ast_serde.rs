//! End-to-end tests for AST Serde, limits enforcement, semantic resolution, and JSON roundtripping.
//!
//! Tiers covered:
//! - Tier 1: serde::Serialize and serde::Deserialize derives for Node, Predicate, MatchOp.
//! - Tier 2: Depth limit (MAX_DEPTH=8), term limit (MAX_TERMS=64), unknown field rejection.
//! - Tier 3: Semantic resolution of @tag -> tag.<key>, @context.app_version -> context.app_version, @extra.level -> extra.level.
//! - Tier 4: AST JSON roundtrip matching frontend serialization format.

use sauron_query::ast::{MatchOp, Node, Predicate, MAX_DEPTH, MAX_TERMS};
use sauron_query::catalog::Resource;
use sauron_query::error::QueryError;
use sauron_query::parse::parse;
use sauron_query::resolve::{resolve, ResolvedNode, TypedValue};
use serde_json::{from_str, json, to_string};

// ---------------------------------------------------------------------------
// Tier 1: Serde derives for MatchOp, Predicate, Node
// ---------------------------------------------------------------------------

#[test]
fn test_match_op_serde_all_variants() {
    let ops = vec![
        MatchOp::Eq,
        MatchOp::Ne,
        MatchOp::Gt,
        MatchOp::Gte,
        MatchOp::Lt,
        MatchOp::Lte,
        MatchOp::In,
        MatchOp::Has,
        MatchOp::Like,
        MatchOp::Contains,
    ];

    for op in ops {
        let serialized = to_string(&op).expect("serialize MatchOp");
        let deserialized: MatchOp = from_str(&serialized).expect("deserialize MatchOp");
        assert_eq!(op, deserialized);
    }
}

#[test]
fn test_predicate_serde_roundtrip() {
    let pred = Predicate {
        field: "@context.app_version".to_string(),
        value: "3.0.2".to_string(),
        quoted: false,
        at: 10,
    };

    let serialized = to_string(&pred).expect("serialize Predicate");
    let deserialized: Predicate = from_str(&serialized).expect("deserialize Predicate");
    assert_eq!(pred, deserialized);

    // Test serde default values for missing quoted and at
    let json_str = r#"{"field":"level","value":"error"}"#;
    let pred_default: Predicate = from_str(json_str).expect("deserialize with defaults");
    assert_eq!(pred_default.field, "level");
    assert_eq!(pred_default.value, "error");
    assert!(!pred_default.quoted);
    assert_eq!(pred_default.at, 0);
}

#[test]
fn test_node_enum_serde_variants() {
    let p1 = Predicate {
        field: "@tag".to_string(),
        value: "v1".to_string(),
        quoted: false,
        at: 0,
    };
    let p2 = Predicate {
        field: "@extra.level".to_string(),
        value: "warn".to_string(),
        quoted: true,
        at: 5,
    };

    let nodes = vec![
        Node::Pred(p1.clone()),
        Node::Text("search string".to_string()),
        Node::Not(Box::new(Node::Pred(p2.clone()))),
        Node::And(vec![Node::Pred(p1.clone()), Node::Pred(p2.clone())]),
        Node::Or(vec![
            Node::And(vec![Node::Pred(p1.clone())]),
            Node::Not(Box::new(Node::Pred(p2.clone()))),
        ]),
    ];

    for node in nodes {
        let json_repr = to_string(&node).expect("serialize Node");
        let restored: Node = from_str(&json_repr).expect("deserialize Node");
        assert_eq!(node, restored);
    }
}

// ---------------------------------------------------------------------------
// Tier 2: Limits enforcement (depth, terms) and unknown field rejection
// ---------------------------------------------------------------------------

fn ast_depth(node: &Node) -> usize {
    match node {
        Node::And(children) | Node::Or(children) => {
            1 + children.iter().map(ast_depth).max().unwrap_or(0)
        }
        Node::Not(inner) => 1 + ast_depth(inner),
        Node::Pred(_) | Node::Text(_) => 1,
    }
}

fn ast_terms(node: &Node) -> usize {
    match node {
        Node::And(children) | Node::Or(children) => children.iter().map(ast_terms).sum(),
        Node::Not(inner) => ast_terms(inner),
        Node::Pred(_) | Node::Text(_) => 1,
    }
}

#[test]
fn test_ast_depth_limit_constants_and_enforcement() {
    assert_eq!(MAX_DEPTH, 8);

    // Build a nested string expression exceeding MAX_DEPTH (9 levels)
    let deep_query = "((((((((a:1))))))))";
    let res = parse(deep_query);
    assert!(res.is_err());
    match res.unwrap_err() {
        QueryError::TooDeep { max } => assert_eq!(max, MAX_DEPTH),
        other => panic!("expected TooDeep error, got {other:?}"),
    }

    // Expression within depth limit (8 levels or fewer) parses OK
    let valid_query = "((((((a:1))))))";
    assert!(parse(valid_query).is_ok());
}

#[test]
fn test_ast_term_limit_enforcement() {
    assert_eq!(MAX_TERMS, 64);

    // Build query string with 65 terms
    let terms: Vec<String> = (0..65).map(|i| format!("term{i}:val")).collect();
    let many_terms_query = terms.join(" ");
    let res = parse(&many_terms_query);
    assert!(res.is_err());
    match res.unwrap_err() {
        QueryError::TooManyTerms { max } => assert_eq!(max, MAX_TERMS),
        other => panic!("expected TooManyTerms error, got {other:?}"),
    }

    // 64 terms parses OK
    let ok_terms: Vec<String> = (0..64).map(|i| format!("t{i}:val")).collect();
    let ok_query = ok_terms.join(" ");
    let parsed_ok = parse(&ok_query);
    assert!(parsed_ok.is_ok());
    assert!(ast_terms(&parsed_ok.unwrap()) <= MAX_TERMS);
}

#[test]
fn test_unknown_field_rejection() {
    let node = Node::Pred(Predicate {
        field: "completely_invalid_field_xyz".to_string(),
        value: "bar".to_string(),
        quoted: false,
        at: 0,
    });

    let res = resolve(&node, Resource::Issues);
    assert!(res.is_err());
    match res.unwrap_err() {
        QueryError::UnknownField { field, .. } => {
            assert_eq!(field, "completely_invalid_field_xyz");
        }
        other => panic!("expected UnknownField error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Tier 3: Semantic resolution of @tag, @context.app_version, @extra.level
// ---------------------------------------------------------------------------

#[test]
fn test_semantic_resolution_tag_prefixes() {
    // 1. Bare @tag
    let node_tag = Node::Pred(Predicate {
        field: "@tag".to_string(),
        value: "v1".to_string(),
        quoted: false,
        at: 0,
    });
    let resolved_tag = resolve(&node_tag, Resource::Issues).expect("resolve @tag");
    if let ResolvedNode::Pred(pred) = resolved_tag {
        assert_eq!(pred.dim.name, "tag");
        assert_eq!(pred.path, Some("tag".to_string()));
    } else {
        panic!("expected ResolvedNode::Pred");
    }

    // 2. Explicit tag key: @tag.environment=production
    let node_tag_key = Node::Pred(Predicate {
        field: "@tag.environment".to_string(),
        value: "production".to_string(),
        quoted: false,
        at: 0,
    });
    let resolved_tag_key = resolve(&node_tag_key, Resource::Issues).expect("resolve @tag.environment");
    if let ResolvedNode::Pred(pred) = resolved_tag_key {
        assert_eq!(pred.dim.name, "tag");
        assert_eq!(pred.path, Some("environment".to_string()));
        assert_eq!(pred.value, TypedValue::Str("production".to_string()));
    } else {
        panic!("expected ResolvedNode::Pred");
    }
}

#[test]
fn test_semantic_resolution_context_app_version() {
    let node = Node::Pred(Predicate {
        field: "@context.app_version".to_string(),
        value: "3.0.2".to_string(),
        quoted: false,
        at: 0,
    });
    let resolved = resolve(&node, Resource::Issues).expect("resolve @context.app_version");
    if let ResolvedNode::Pred(pred) = resolved {
        assert_eq!(pred.dim.name, "context");
        assert_eq!(pred.path, Some("app_version".to_string()));
        assert_eq!(pred.value, TypedValue::Str("3.0.2".to_string()));
    } else {
        panic!("expected ResolvedNode::Pred");
    }
}

#[test]
fn test_semantic_resolution_extra_level() {
    let node = Node::Pred(Predicate {
        field: "@extra.level".to_string(),
        value: "warn".to_string(),
        quoted: false,
        at: 0,
    });
    let resolved = resolve(&node, Resource::Issues).expect("resolve @extra.level");
    if let ResolvedNode::Pred(pred) = resolved {
        assert_eq!(pred.dim.name, "extra");
        assert_eq!(pred.path, Some("level".to_string()));
        assert_eq!(pred.value, TypedValue::Str("warn".to_string()));
    } else {
        panic!("expected ResolvedNode::Pred");
    }
}

#[test]
fn test_semantic_resolution_label_prefix() {
    let node = Node::Pred(Predicate {
        field: "@$label.team".to_string(),
        value: "frontend".to_string(),
        quoted: false,
        at: 0,
    });
    let resolved = resolve(&node, Resource::Issues).expect("resolve @$label.team");
    if let ResolvedNode::Pred(pred) = resolved {
        assert_eq!(pred.dim.name, "$label");
        assert_eq!(pred.path, Some("team".to_string()));
        assert_eq!(pred.value, TypedValue::Str("frontend".to_string()));
    } else {
        panic!("expected ResolvedNode::Pred");
    }
}

// ---------------------------------------------------------------------------
// Tier 4: AST JSON roundtrip matching frontend serialization format
// ---------------------------------------------------------------------------

#[test]
fn test_ast_json_roundtrip_frontend_format() {
    // Equivalent to frontend: (@tag=v1 and @context.app_version=3.0.2) or (@extra.level=warn)
    let json_input = json!({
        "Or": [
            {
                "And": [
                    {
                        "Pred": {
                            "field": "@tag",
                            "value": "v1",
                            "quoted": false,
                            "at": 0
                        }
                    },
                    {
                        "Pred": {
                            "field": "@context.app_version",
                            "value": "3.0.2",
                            "quoted": false,
                            "at": 0
                        }
                    }
                ]
            },
            {
                "Pred": {
                    "field": "@extra.level",
                    "value": "warn",
                    "quoted": false,
                    "at": 0
                }
            }
        ]
    });

    let json_str = json_input.to_string();
    let node: Node = from_str(&json_str).expect("deserialize complex frontend AST JSON");

    // Re-serialize and deserialize back
    let re_serialized = to_string(&node).expect("re-serialize AST Node");
    let node_restored: Node = from_str(&re_serialized).expect("re-deserialize AST Node");
    assert_eq!(node, node_restored);

    // Resolve the deserialized AST Node
    let resolved = resolve(&node, Resource::Occurrences).expect("resolve deserialized AST");
    match resolved {
        ResolvedNode::Or(branches) => {
            assert_eq!(branches.len(), 2);
            // Branch 0 is ResolvedNode::And
            match &branches[0] {
                ResolvedNode::And(preds) => {
                    assert_eq!(preds.len(), 2);
                }
                other => panic!("expected ResolvedNode::And, got {other:?}"),
            }
            // Branch 1 is ResolvedNode::Pred (@extra.level=warn)
            match &branches[1] {
                ResolvedNode::Pred(pred) => {
                    assert_eq!(pred.dim.name, "extra");
                    assert_eq!(pred.path, Some("level".to_string()));
                }
                other => panic!("expected ResolvedNode::Pred, got {other:?}"),
            }
        }
        other => panic!("expected ResolvedNode::Or, got {other:?}"),
    }
}
