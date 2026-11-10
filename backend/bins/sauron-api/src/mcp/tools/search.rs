//! `describe_search`: the search language's fields and tags for an app, so
//! Claude can write correct `query` strings for `trending_issues` and
//! `list_issue_events`.
//!
//! It serves what the dashboard's search box gets from
//! `GET /v1/apps/{id}/search/schema` (`routes::search::build_schema_response`
//! over the same catalog, `load_tag_keys` for the app's own tag keys), behind
//! the same `event:read`. Three example queries are built from the dimension
//! names the catalog really has, so they cannot drift from the language.

use rmcp::model::{CallToolResult, ContentBlock};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use sauron_auth::perm;
use sauron_query::{ReleaseFilter, Resource};

use crate::mcp::access::{
    deep_link, read_scope, resolve_app, within_deadline_saying, ToolError, TIMED_OUT_RETRY,
};
use crate::mcp::format::{clip, one_line, ECHO_MAX_CHARS};
use crate::routes::search::{
    build_schema_response, load_tag_keys, SchemaDimension, SchemaResponse,
};
use crate::AppState;

/// The contexts the tool describes: the two its `query` consumers take.
const CONTEXTS: &[&str] = &["issues", "occurrences"];
/// Tags listed, and sample values per tag.
const MAX_TAGS: usize = 30;
const MAX_SAMPLES: usize = 5;
const SAMPLE_MAX_CHARS: usize = 60;
const TAG_KEY_MAX_CHARS: usize = 80;

/// Arguments of `describe_search`.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct DescribeSearchArgs {
    /// The app: `org/project/app`, `project/app` (accepted when unique among
    /// the apps you can read in organizations you belong to), or the app's
    /// id. `list_apps` prints the references.
    pub app: String,
    /// What the query searches: `issues` (default; the `query` of
    /// `trending_issues`) or `occurrences` (the events of one issue; the
    /// `query` of `list_issue_events`).
    pub context: Option<String>,
}

fn parse_context(raw: Option<&str>) -> Result<(&'static str, Resource), ToolError> {
    match raw.map(str::trim) {
        None | Some("issues") => Ok(("issues", Resource::Issues)),
        Some("occurrences") => Ok(("occurrences", Resource::Occurrences)),
        Some(other) => Err(ToolError::BadArgument(format!(
            "context must be one of: {} (got {:?})",
            CONTEXTS.join(", "),
            clip(other, ECHO_MAX_CHARS)
        ))),
    }
}

pub(crate) async fn describe_search(
    state: &AppState,
    user_id: Uuid,
    args: DescribeSearchArgs,
) -> Result<CallToolResult, ToolError> {
    // No window or environment argument: nothing to narrow.
    within_deadline_saying(state, TIMED_OUT_RETRY, async {
        let (context, resource) = parse_context(args.context.as_deref())?;
        let mut conn = crate::routes::db(state).await?;
        let target = resolve_app(&mut conn, user_id, &args.app).await?;
        // The dashboard's schema handler authorizes on `event:read`.
        let (scope, _) = read_scope(&mut conn, user_id, &target, perm::EVENT_READ, None).await?;
        let tags = load_tag_keys(state, &mut conn, target.app.id, context, resource).await;
        drop(conn);
        let schema = build_schema_response(context, resource, tags);
        // The Issues page, where `issues` queries go into the search box
        // (`render` prints it for that context only).
        let link = deep_link(
            &state.cfg,
            &target,
            "/issues",
            &scope.env,
            &ReleaseFilter::All,
        );
        let text = render(&target.reference(), context, &schema, link.as_deref());
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    })
    .await
}

fn render(app_ref: &str, context: &str, schema: &SchemaResponse, link: Option<&str>) -> String {
    let consumer = match context {
        "occurrences" => "the `query` of list_issue_events",
        _ => "the `query` of trending_issues",
    };
    let mut out = vec![
        format!("Search language for {app_ref}, context {context} ({consumer})."),
        "Write `field:value` terms; terms separated by a space are ANDed, `OR` is also \
         accepted, `!field:value` negates, and `*` is a wildcard (`culprit:*checkout*`)."
            .to_string(),
        String::new(),
        "Dimensions (name · type · ops · options):".to_string(),
    ];
    for d in &schema.dimensions {
        out.push(format!("  {}", dimension_line(d)));
    }
    if !schema.variables.is_empty() {
        out.push(String::new());
        out.push("Variables (prefix a key: `@tag:key=value`):".to_string());
        for v in &schema.variables {
            out.push(format!("  {} · {}", v.prefix, v.description));
        }
    }
    out.push(String::new());
    if schema.available_tags.is_empty() {
        out.push("Tags: none seen in this app's recent events.".to_string());
    } else {
        out.push(format!(
            "Tags seen in this app's recent events ({}), with up to {MAX_SAMPLES} sample \
             values:",
            schema.available_tags.len()
        ));
        for t in schema.available_tags.iter().take(MAX_TAGS) {
            let samples = t
                .sample_values
                .as_deref()
                .unwrap_or_default()
                .iter()
                .take(MAX_SAMPLES)
                .map(|v| one_line(v, SAMPLE_MAX_CHARS))
                .collect::<Vec<_>>()
                .join(", ");
            let key = one_line(&t.key, TAG_KEY_MAX_CHARS);
            if samples.is_empty() {
                out.push(format!("  {key}"));
            } else {
                out.push(format!("  {key}: {samples}"));
            }
        }
        if schema.available_tags.len() > MAX_TAGS {
            out.push(format!(
                "  … {} more tag keys not shown",
                schema.available_tags.len() - MAX_TAGS
            ));
        }
    }
    let examples = example_queries(&schema.dimensions, &schema.available_tags);
    if !examples.is_empty() {
        out.push(String::new());
        out.push("Example queries:".to_string());
        for e in examples {
            out.push(format!("  {e}"));
        }
    }
    // Only `issues` queries are typed on the Issues page; an `occurrences`
    // query belongs to one issue's events, which this link does not open.
    if let (Some(link), "issues") = (link, context) {
        out.push(String::new());
        out.push(format!("Issues page: {link}"));
    }
    out.join("\n")
}

/// `name (alias a, b) · type · ops · options`.
fn dimension_line(d: &SchemaDimension) -> String {
    let mut name = d.name.clone();
    if !d.aliases.is_empty() {
        name.push_str(&format!(" (alias {})", d.aliases.join(", ")));
    }
    let mut line = format!("{name} · {} · {}", d.ty, d.ops.join(" "));
    if let Some(options) = &d.options {
        line.push_str(&format!(" · {}", options.join(", ")));
    }
    line
}

/// Three queries built from dimensions the catalog has: an enum equality
/// (`level:fatal` where `level` exists), a text match (`culprit:*checkout*`),
/// and the two together, plus a tag query when a tag was seen. Whatever the
/// resource lacks is skipped, so every example names a real dimension.
fn example_queries(
    dims: &[SchemaDimension],
    tags: &[crate::routes::search::TagInfo],
) -> Vec<String> {
    let find = |name: &str| dims.iter().find(|d| d.name == name);
    let enum_dim = find("level")
        .filter(|d| d.options.is_some())
        .or_else(|| dims.iter().find(|d| d.ty == "enum" && d.options.is_some()));
    let enum_example = enum_dim.and_then(|d| {
        let options = d.options.as_ref()?;
        let pick = ["fatal", "error"]
            .iter()
            .find(|o| options.iter().any(|x| x == *o))
            .map(|o| o.to_string())
            .or_else(|| options.first().cloned())?;
        Some(format!("{}:{pick}", d.name))
    });
    let text_dim = find("culprit")
        .filter(|d| d.ops.iter().any(|o| o == "like"))
        .or_else(|| {
            dims.iter()
                .find(|d| d.ty == "string" && d.ops.iter().any(|o| o == "like"))
        });
    let text_example = text_dim.map(|d| format!("{}:*checkout*", d.name));
    let mut out = Vec::new();
    out.extend(enum_example.clone());
    out.extend(text_example.clone());
    if let (Some(a), Some(b)) = (&enum_example, &text_example) {
        out.push(format!("{a} {b}"));
    }
    if out.len() < 3 {
        // A resource with tags can still offer a third.
        if let Some(t) = tags.iter().find(|t| {
            t.key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        }) {
            if let Some(v) = t.sample_values.as_ref().and_then(|v| {
                v.iter()
                    .find(|v| v.chars().all(|c| c.is_ascii_alphanumeric()))
            }) {
                out.push(format!("@tag:{}={v}", t.key));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(resource: Resource, context: &str) -> SchemaResponse {
        build_schema_response(context, resource, Vec::new())
    }

    #[test]
    fn context_argument_takes_issues_and_occurrences_only() {
        assert_eq!(parse_context(None).unwrap().0, "issues");
        assert_eq!(
            parse_context(Some(" occurrences ")).unwrap().0,
            "occurrences"
        );
        for bad in ["sessions", "events", "", "Issues"] {
            let e = parse_context(Some(bad)).unwrap_err();
            let msg = e.to_string();
            assert!(matches!(e, ToolError::BadArgument(_)), "{bad:?}");
            assert!(msg.contains("issues, occurrences"), "{msg}");
        }
    }

    /// Every example parses and resolves against its own resource, and uses
    /// only dimension names the catalog has for it.
    #[test]
    fn examples_are_valid_queries_over_real_dimensions() {
        for (resource, context) in [
            (Resource::Issues, "issues"),
            (Resource::Occurrences, "occurrences"),
        ] {
            let s = schema(resource, context);
            let examples = example_queries(&s.dimensions, &s.available_tags);
            assert!(examples.len() >= 3, "{context}: {examples:?}");
            for e in &examples {
                let node = sauron_query::parse(e)
                    .unwrap_or_else(|err| panic!("{context}: {e:?} does not parse: {err}"));
                sauron_query::resolve(&node, resource)
                    .unwrap_or_else(|err| panic!("{context}: {e:?} does not resolve: {err}"));
            }
        }
    }

    #[test]
    fn rendering_lists_dimensions_variables_tags_and_examples() {
        let mut s = schema(Resource::Issues, "issues");
        s.available_tags = vec![crate::routes::search::TagInfo {
            key: "flow".into(),
            sample_values: Some((1..=8).map(|i| format!("v{i}")).collect()),
        }];
        let text = render("acme/mobile/ios", "issues", &s, None);
        assert!(
            text.contains("Search language for acme/mobile/ios"),
            "{text}"
        );
        assert!(
            !text.contains("Issues page"),
            "no link without DASHBOARD_URL: {text}"
        );
        let linked = render(
            "acme/mobile/ios",
            "issues",
            &s,
            Some("https://sauron.example/?org=o&project=p&app=a#/issues"),
        );
        assert!(
            linked
                .ends_with("\nIssues page: https://sauron.example/?org=o&project=p&app=a#/issues"),
            "{linked}"
        );
        let occurrences = schema(Resource::Occurrences, "occurrences");
        let unlinked = render(
            "acme/mobile/ios",
            "occurrences",
            &occurrences,
            Some("https://sauron.example/?org=o&project=p&app=a#/issues"),
        );
        assert!(
            !unlinked.contains("Issues page"),
            "occurrences queries do not go into the Issues page: {unlinked}"
        );
        assert!(text.contains("  level"), "{text}");
        assert!(text.contains("fatal"), "{text}");
        // Up to five sample values.
        assert!(text.contains("flow: v1, v2, v3, v4, v5\n"), "{text}");
        assert!(!text.contains("v6"), "{text}");
        assert!(text.contains("Example queries:"), "{text}");
    }
}
