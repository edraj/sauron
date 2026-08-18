//! The issue "culprit" — the one frame a human reads to know *where* a crash
//! happened, rendered as `function (location)`.
//!
//! It exists here, in the symbolication crate, rather than beside the ingest
//! pipeline's `build_title`, because it has to be derived TWICE from two
//! different frame types: once over the raw frames the SDK sent, and once over
//! the [`ResolvedFrame`]s symbolication produced. Those two answers are shown
//! in the same slot of the same table row, so a reader cannot tell which one
//! they are looking at — which makes any drift between the two selection rules
//! invisible rather than merely wrong. One rule, one place.

use crate::engine::ResolvedFrame;

/// The three fields the selection rule reads, borrowed from whatever frame type
/// the caller holds.
///
/// `location` is the caller's choice of `filename` or `module` — the raw frame
/// model carries both and prefers `filename`, a resolved frame only has
/// `filename` — so that preference stays with the type that has the fields
/// rather than being re-litigated here.
pub struct CulpritFrame<'a> {
    pub function: Option<&'a str>,
    pub location: Option<&'a str>,
    /// Appended to `location` as `:N` when both are present. Optional because a
    /// Dart frame that resolved to a function but no line has one and not the
    /// other, and `file:` with nothing after it reads as a truncation.
    pub lineno: Option<u32>,
    pub in_app: Option<bool>,
}

/// Pick the culprit frame and format it.
///
/// **Frames arrive with the crashing frame LAST** (the SDK/Sentry ordering the
/// rest of this codebase uses), hence the `rev()`: "top in-app frame" means the
/// in-app frame nearest the crash, not the one nearest `main`. Falling back to
/// the last frame rather than the first keeps that meaning when nothing is
/// flagged `in_app` — a trace of pure framework frames still points at where it
/// blew up.
///
/// The rendered form is `function (file:line)`, degrading a piece at a time:
/// `function (file)` with no line, `function` with no file, `? (file:line)`
/// with no name. The file and line are half the value of the string — "which
/// class" without "which line of it" still leaves the reader searching — so
/// they are part of the format rather than something a call site appends.
///
/// Returns `""` for an empty trace. That empty string is a real value, not
/// "unknown": it is what a message-only capture has, and `issues.culprit` is
/// `NOT NULL DEFAULT ''` precisely to hold it.
pub fn culprit_of(frames: &[CulpritFrame<'_>]) -> String {
    let frame = frames
        .iter()
        .rev()
        .find(|f| f.in_app == Some(true))
        .or_else(|| frames.last());
    match frame {
        Some(f) => {
            let func = f.function.unwrap_or("?");
            match (f.location, f.lineno) {
                (Some(loc), Some(line)) => format!("{func} ({loc}:{line})"),
                (Some(loc), None) => format!("{func} ({loc})"),
                (None, _) => func.to_string(),
            }
        }
        None => String::new(),
    }
}

/// [`culprit_of`] over symbolicated frames.
///
/// Only frames that actually resolved are considered. A `ResolvedFrame` that
/// fell through as a passthrough carries the SAME minified name the raw
/// derivation would have produced, so letting one win here would swap a
/// readable culprit for an unreadable one and report it as symbolicated. When
/// nothing resolved this returns `None`, and the caller keeps the raw culprit.
pub fn culprit_of_resolved(frames: &[ResolvedFrame]) -> Option<String> {
    let resolved: Vec<CulpritFrame<'_>> = frames
        .iter()
        .filter(|f| f.symbolicated)
        .map(|f| CulpritFrame {
            function: f.function.as_deref(),
            location: f.filename.as_deref(),
            lineno: f.lineno,
            in_app: f.in_app,
        })
        .collect();
    if resolved.is_empty() {
        return None;
    }
    Some(culprit_of(&resolved))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(function: &str, filename: &str, in_app: Option<bool>) -> ResolvedFrame {
        ResolvedFrame {
            function: Some(function.to_string()),
            filename: Some(filename.to_string()),
            lineno: Some(1),
            colno: None,
            in_app,
            symbolicated: true,
            context_line: None,
            pre_context: Vec::new(),
            post_context: Vec::new(),
            context_start_line: None,
        }
    }

    fn frame<'a>(function: &'a str, location: &'a str, in_app: Option<bool>) -> CulpritFrame<'a> {
        CulpritFrame {
            function: Some(function),
            location: Some(location),
            lineno: None,
            in_app,
        }
    }

    #[test]
    fn empty_trace_is_the_empty_string() {
        assert_eq!(culprit_of(&[]), "");
    }

    #[test]
    fn prefers_the_in_app_frame_nearest_the_crash() {
        // Crashing frame last: two in-app frames, the deeper one must win.
        let frames = vec![
            frame("main", "main.dart", Some(true)),
            frame("dispatch", "framework.dart", Some(false)),
            frame("checkout", "cart_bloc.dart", Some(true)),
            frame("_throw", "errors.dart", Some(false)),
        ];
        assert_eq!(culprit_of(&frames), "checkout (cart_bloc.dart)");
    }

    #[test]
    fn falls_back_to_the_deepest_frame_when_nothing_is_in_app() {
        let frames = vec![
            frame("main", "main.dart", None),
            frame("_throw", "errors.dart", None),
        ];
        assert_eq!(culprit_of(&frames), "_throw (errors.dart)");
    }

    #[test]
    fn a_frame_without_a_function_renders_a_placeholder_not_an_empty_pair() {
        let frames = vec![CulpritFrame {
            function: None,
            location: Some("cart_bloc.dart"),
            lineno: Some(88),
            in_app: Some(true),
        }];
        assert_eq!(culprit_of(&frames), "? (cart_bloc.dart:88)");
    }

    #[test]
    fn a_frame_without_a_location_renders_the_function_alone() {
        let frames = vec![CulpritFrame {
            function: Some("checkout"),
            location: None,
            // A line with no file to hang it on is not rendered: ":88" alone
            // names nothing.
            lineno: Some(88),
            in_app: Some(true),
        }];
        assert_eq!(culprit_of(&frames), "checkout");
    }

    #[test]
    fn the_line_number_rides_along_with_the_file() {
        let frames = vec![CulpritFrame {
            function: Some("checkout"),
            location: Some("lib/blocs/cart_bloc.dart"),
            lineno: Some(88),
            in_app: Some(true),
        }];
        assert_eq!(
            culprit_of(&frames),
            "checkout (lib/blocs/cart_bloc.dart:88)"
        );
    }

    #[test]
    fn resolved_derivation_ignores_passthrough_frames() {
        // The passthrough is the deepest frame AND the only in-app one, so both
        // halves of the selection rule would pick it if it were eligible --
        // and it carries the minified name the raw derivation already has.
        let mut passthrough = resolved("a", "app.min.js", Some(true));
        passthrough.symbolicated = false;
        let frames = vec![resolved("checkout", "cart.ts", Some(true)), passthrough];
        assert_eq!(
            culprit_of_resolved(&frames),
            Some("checkout (cart.ts:1)".to_string())
        );
    }

    #[test]
    fn resolved_derivation_is_none_when_nothing_resolved() {
        // Not `Some("")`: the caller must be able to tell "no symbolicated
        // culprit, keep the raw one" from "this event genuinely has no frames".
        let mut passthrough = resolved("a", "app.min.js", Some(true));
        passthrough.symbolicated = false;
        assert_eq!(culprit_of_resolved(&[passthrough]), None);
    }
}
