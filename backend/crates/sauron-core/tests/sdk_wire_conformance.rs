//! **Wire conformance: does what the SDKs emit actually deserialize?**
//!
//! Every SDK suite used to assert its envelope matched a hand-written golden
//! *literal committed beside it*, and this crate's own tests asserted a
//! hand-written golden literal too. Both sides could therefore agree
//! perfectly on a shape the real deserializer rejects — which is exactly what
//! happened: `js`'s `captureMessage` shipped `exception.type: null` against a
//! non-`Option` `String` with no `serde(default)`, and `flutter`'s `track`
//! shipped `distinct_id: null` against a non-`Option` `String`. Both are a 400
//! `invalid_envelope`.
//!
//! That is not "one event lost". The envelope is **all-or-nothing**: the whole
//! batch fails to parse, every SDK classifies 400 as a non-retryable drop, and
//! the default batch is 30 items. One bad item silently destroys 29 unrelated
//! events, with no log anywhere.
//!
//! So this test does the one thing no suite did: it feeds each SDK's real
//! emitted envelope through **this** crate's [`Envelope`] — the same
//! `serde` impl `sauron-ingest` calls — and fails if it does not deserialize.
//!
//! The fixtures in `sdks/wire-fixtures/` are captured off each SDK's own
//! transport by that SDK's suite (see the README there); they are not authored
//! by hand, and nothing in this file mirrors the wire types.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use sauron_core::envelope::{Envelope, EnvelopeItem};

/// Every SDK on the wire. Listed explicitly so **deleting** a fixture is a
/// failure rather than a silently smaller test run.
const EXPECTED_SDKS: &[&str] = &["js", "node", "python", "csharp", "flutter"];

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../sdks/wire-fixtures")
}

fn fixture_path(sdk: &str) -> PathBuf {
    fixtures_dir().join(format!("{sdk}.json"))
}

fn read_fixture(sdk: &str) -> String {
    let path = fixture_path(sdk);
    fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing wire fixture for the `{sdk}` SDK at {}: {e}\n\
             Regenerate it by running that SDK's suite — see \
             sdks/wire-fixtures/README.md. A fixture that cannot be read is a \
             conformance hole, not a skip.",
            path.display()
        )
    })
}

/// Parse a fixture into the REAL wire type, reporting the serde error verbatim.
fn parse(sdk: &str, raw: &str) -> Envelope {
    serde_json::from_str::<Envelope>(raw).unwrap_or_else(|e| {
        panic!(
            "the `{sdk}` SDK emits an envelope the ingest gateway CANNOT parse: {e}\n\
             This is a 400 `invalid_envelope` for the ENTIRE batch (default 30 \
             items), which every SDK drops without retrying.\n\
             Fixture: {}",
            fixture_path(sdk).display()
        )
    })
}

#[test]
fn every_sdk_ships_a_wire_fixture() {
    let dir = fixtures_dir();
    let found: BTreeSet<String> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".json").map(str::to_string)
        })
        .collect();

    let missing: Vec<&str> = EXPECTED_SDKS
        .iter()
        .copied()
        .filter(|s| !found.contains(*s))
        .collect();
    assert!(
        missing.is_empty(),
        "no wire fixture for {missing:?} in {} (found {found:?}). Every SDK on \
         the wire must prove its envelope parses; see \
         sdks/wire-fixtures/README.md to regenerate.",
        dir.display()
    );

    // A fixture nobody accounted for is also a problem: it means an SDK exists
    // that this test does not know to check.
    let unexpected: Vec<&String> = found
        .iter()
        .filter(|s| !EXPECTED_SDKS.contains(&s.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "unaccounted wire fixture(s) {unexpected:?} — add the SDK to EXPECTED_SDKS"
    );
}

#[test]
fn every_sdk_envelope_deserializes_into_the_real_wire_type() {
    for sdk in EXPECTED_SDKS {
        let raw = read_fixture(sdk);
        let env = parse(sdk, &raw);

        assert!(
            !env.header.sdk.name.is_empty(),
            "{sdk}: header.sdk.name is empty"
        );
        assert!(
            !env.header.sdk.version.is_empty(),
            "{sdk}: header.sdk.version is empty"
        );
        assert!(
            !env.items.is_empty(),
            "{sdk}: fixture carries no items, so it proves nothing. Drive the \
             SDK's real capture API in its emitter test."
        );
    }
}

/// Fields the pipeline dereferences. `serde` already rejects a missing/`null`
/// non-`Option`, but it happily accepts `""` — and an empty `distinct_id` or
/// exception type is silently dropped or mis-grouped downstream rather than
/// rejected, which is worse than a 400 because nothing surfaces at all.
#[test]
fn every_item_carries_the_identity_fields_the_pipeline_reads() {
    for sdk in EXPECTED_SDKS {
        let env = parse(sdk, &read_fixture(sdk));
        for (i, item) in env.items.iter().enumerate() {
            let at = format!("{sdk} item[{i}]");
            match item {
                EnvelopeItem::Error(e) => {
                    // An error item must carry its text SOMEWHERE. `js`'s
                    // `captureMessage` used to set neither `message` nor a
                    // usable `exception.type`, so once the wire-invalid
                    // `type: null` is gone the text can vanish just as easily.
                    let has_text = e.message.as_deref().is_some_and(|m| !m.trim().is_empty())
                        || e.exception
                            .as_ref()
                            .and_then(|x| x.value.as_deref())
                            .is_some_and(|v| !v.trim().is_empty());
                    assert!(
                        has_text,
                        "{at}: error item carries neither `message` nor \
                         `exception.value` — it would render as a blank issue"
                    );
                    if let Some(x) = &e.exception {
                        assert!(
                            !x.ty.trim().is_empty(),
                            "{at}: exception.type is empty; grouping keys off it"
                        );
                    }
                }
                EnvelopeItem::Event(ev) => {
                    assert!(!ev.name.trim().is_empty(), "{at}: event name is empty");
                    assert!(
                        !ev.distinct_id.trim().is_empty(),
                        "{at}: event `{}` has an empty distinct_id — \
                         `process_event` cannot attribute it to anyone",
                        ev.name
                    );
                }
                EnvelopeItem::Identify(id) => {
                    assert!(
                        !id.distinct_id.trim().is_empty(),
                        "{at}: identify with an empty distinct_id"
                    );
                }
                EnvelopeItem::Transaction(t) => {
                    assert!(!t.name.trim().is_empty(), "{at}: transaction name is empty");
                    assert!(!t.op.trim().is_empty(), "{at}: transaction op is empty");
                    assert!(
                        t.duration_ms.is_finite(),
                        "{at}: duration_ms is {} — NaN/Inf serializes as JSON \
                         `null` and fails the non-Option f64",
                        t.duration_ms
                    );
                }
                EnvelopeItem::BreadcrumbBatch(_) => {}
            }
        }
    }
}

/// A fixture that only carried, say, an `identify` would parse forever while
/// the item types that actually broke went unexercised. Pin the coverage.
#[test]
fn each_fixture_exercises_every_item_type_the_sdk_can_emit() {
    for sdk in EXPECTED_SDKS {
        let env = parse(sdk, &read_fixture(sdk));
        let kinds: BTreeSet<&str> = env
            .items
            .iter()
            .map(|i| match i {
                EnvelopeItem::Error(_) => "error",
                EnvelopeItem::Event(_) => "event",
                EnvelopeItem::Identify(_) => "identify",
                EnvelopeItem::Transaction(_) => "transaction",
                EnvelopeItem::BreadcrumbBatch(_) => "breadcrumb_batch",
            })
            .collect();

        for required in ["error", "event", "identify", "transaction"] {
            assert!(
                kinds.contains(required),
                "{sdk}: fixture has no `{required}` item (got {kinds:?}). Every \
                 item type this SDK can emit must be represented, or the \
                 conformance check has a blind spot exactly where the last two \
                 wire bugs lived."
            );
        }
    }
}

/// The two shapes that were live 400s. Named individually so a regression
/// reads as itself in CI output instead of as a generic parse failure.
#[test]
fn a_message_capture_survives_the_deserializer() {
    // Every SDK with a message-capture API emits it as an `error` item with an
    // empty stacktrace. The regression: `exception.type: null` (js) against a
    // non-`Option` `String`.
    for sdk in ["js", "node", "python", "csharp"] {
        let env = parse(sdk, &read_fixture(sdk));
        let message_like = env.items.iter().any(|i| match i {
            EnvelopeItem::Error(e) => match &e.exception {
                None => e.message.is_some(),
                Some(x) => x.stacktrace.is_empty(),
            },
            _ => false,
        });
        assert!(
            message_like,
            "{sdk}: fixture has no message-capture error item, so the \
             `exception.type: null` class is unguarded for this SDK"
        );
    }
}

#[test]
fn an_analytics_event_survives_the_deserializer() {
    // The regression: `distinct_id: null` (flutter, before `identify()`)
    // against a non-`Option` `String`. `serde` rejects the whole envelope, so
    // the SDK-emitted `$screen` and `$workflow_*` events took every unrelated
    // item in the batch down with them.
    for sdk in EXPECTED_SDKS {
        let env = parse(sdk, &read_fixture(sdk));
        assert!(
            env.items
                .iter()
                .any(|i| matches!(i, EnvelopeItem::Event(_))),
            "{sdk}: fixture has no `event` item"
        );
    }
}
