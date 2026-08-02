//! Every environment key `config.rs` reads must be documented in `.env.example`.
//!
//! Thirteen new variables land in one slice and roughly thirty across the
//! programme, each needing a row in `.env.example`, `docker-compose.yml`, the
//! relevant `packaging/rpm/config/*.env` and the README table. Nothing enforced
//! any of that. This is the cheapest of the four to enforce and the one the other
//! three are usually copied from.
//!
//! A Rust test rather than a shell step in `ci.yml`: CI already runs
//! `cargo test --workspace`, so this needs no workflow change, and an engineer
//! can reproduce a failure with the same command they already use.

use std::collections::BTreeSet;

/// Keys deliberately absent from `.env.example`, each with the reason.
const EXEMPT: &[(&str, &str)] = &[
    (
        "DATABASE_URL",
        "composed by docker-compose from POSTGRES_USER/PASSWORD/DB, and set \
         per-service in packaging/rpm/config/sauron.env",
    ),
    (
        "REDIS_URL",
        "pinned to the compose service name; not an operator-facing knob there",
    ),
    (
        "SAURON_DEV",
        "a local-development escape hatch that makes tokens forgeable; documenting \
         it in a file operators copy is an invitation",
    ),
];

fn keys_read_by_config() -> BTreeSet<String> {
    let src = include_str!("../src/config.rs");
    let mut out = BTreeSet::new();
    // `parse::<u64>("MAIL_DRAIN_TICK_SECS", 60)` reads env identically to the
    // turbofish-free calls surrounding it, but a literal `parse("` scan walks
    // straight past it: the one call site that needed an explicit type would
    // have been the single key sitting outside this ratchet.
    for needle in ["var(", "parse(", "parse::<"] {
        let mut rest = src;
        while let Some(i) = rest.find(needle) {
            let mut after = &rest[i + needle.len()..];
            if needle == "parse::<" {
                // Step over the generic argument list: `u64>("KEY"` -> `"KEY"`.
                match after.find(">(") {
                    Some(j) => after = &after[j + 2..],
                    None => break,
                }
            }
            // rustfmt wraps any call whose key plus default-constant name
            // overflows 100 columns, moving the key to the NEXT line. Matching a
            // literal `parse("` walked straight past those, so a wrapped call
            // site was silently exempt from this ratchet -- and three of the six
            // personal-notification keys wrap, so half of them were exempt while
            // the gate still reported green.
            after = after.trim_start();
            if !after.starts_with('"') {
                // `v.parse().ok()`, or a call whose key is not a literal.
                rest = &rest[i + needle.len()..];
                continue;
            }
            after = &after[1..];
            match after.find('"') {
                Some(end) => {
                    out.insert(after[..end].to_string());
                    rest = &after[end..];
                }
                None => break,
            }
        }
    }
    out
}

#[test]
fn every_config_key_appears_in_env_example() {
    let example = include_str!("../../../../.env.example");
    let exempt: BTreeSet<&str> = EXEMPT.iter().map(|(k, _)| *k).collect();

    let mut missing: Vec<String> = Vec::new();
    for key in keys_read_by_config() {
        if exempt.contains(key.as_str()) {
            continue;
        }
        // Matched with the `=` (or as a commented `# KEY=`) so `SMTP_HOST` in a
        // prose sentence does not count as documentation.
        let documented = example.lines().any(|l| {
            l.trim_start()
                .trim_start_matches("# ")
                .starts_with(&format!("{key}="))
        });
        if !documented {
            missing.push(key);
        }
    }

    assert!(
        missing.is_empty(),
        "these config keys are read by config.rs but not documented in .env.example: {missing:?}\n\
         Add a line for each (a commented `# KEY=` counts), or add it to EXEMPT with a reason."
    );
}

#[test]
fn exemptions_are_still_read_by_config() {
    // An exemption for a key nobody reads any more is dead weight that makes the
    // list look longer and more negotiable than it is.
    let keys = keys_read_by_config();
    for (key, _) in EXEMPT {
        assert!(
            keys.contains(*key),
            "{key} is exempted but config.rs no longer reads it"
        );
    }
}
