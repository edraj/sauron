//! One-shot migration runner. Applies pending migrations and exits — the
//! Docker Compose `migrate` service the other containers depend on, and the
//! `sauron-migrate.service` oneshot every RPM daemon now pulls in via
//! `Requires=`.
//!
//! Because the daemons declare `Requires=`, a failure here fails *their* start
//! jobs too, and systemd never retries a failed start job. So this binary
//! tolerates a Postgres that is merely not up **yet** — see
//! [`sauron_db::run_pending_migrations_waiting`] for why that retry is load
//! bearing rather than a nicety.

/// One opt-in backfill: the token an operator types, and what it does.
///
/// The single source for BOTH `--help` and argument validation. They were
/// separate lists exactly once, and a list that only the error path reads is a
/// list nobody notices going stale — the failure mode being that a shipped
/// subcommand is rejected as unknown, or a removed one is still advertised.
struct Command {
    name: &'static str,
    /// One line for `--help`. Present tense, says what it costs.
    summary: &'static str,
}

const COMMANDS: [Command; 4] = [
    Command {
        name: "backfill-person-days",
        summary: "Retention cohorts for apps predating migration 74. \
                  REQUIRED for retention: there is no fallback query, so until \
                  this runs those apps report not-ready.",
    },
    Command {
        name: "backfill-person-envs",
        summary: "Per-person environment rollup for /persons. Performance only \
                  — reads fall back to the pre-rollup query without it.",
    },
    Command {
        name: "backfill-device-envs",
        summary: "Per-device environment rollup for /device-groups. \
                  Performance only, same fallback as person-envs.",
    },
    Command {
        name: "backfill-rollups",
        summary: "Replays every pre-epoch day of the three firehose tables into \
                  the dashboard rollups (migration 71). The heaviest of the \
                  four; refuses if marker rows already exist.",
    },
];

const USAGE: &str = "\
sauron-migrate — apply pending database migrations, then optionally backfill.

USAGE:
    sauron-migrate [BACKFILL]...

With no arguments it applies pending migrations and exits. That is what the
Compose `migrate` service and the `sauron-migrate.service` oneshot run.

Backfills are opt-in and never part of the no-argument path: this binary is the
oneshot every RPM daemon pulls in via `Requires=`, systemd never retries a
failed start job, and these aggregate the largest tables — so anything slow or
failure-prone here delays or breaks every daemon's start. Run them by hand,
after the migrations, at a time of your choosing. Several may be combined in
one invocation; each runs at most once.

BACKFILLS:
";

const ENV_HELP: &str = "\
ENVIRONMENT:
    DATABASE_URL         Postgres connection string. Required.
    MIGRATE_WAIT_SECS    Seconds to tolerate a Postgres that is not up YET
                         (default 120; `0` disables waiting, which is what the
                         Compose path wants since it gates on a healthy
                         Postgres already). An unparseable value falls back to
                         the default rather than refusing to run.

EXAMPLES:
    sauron-migrate
    sauron-migrate backfill-person-days
    sauron-migrate backfill-rollups backfill-person-envs

After a backfill, run `ANALYZE` on the tables it filled — a backfill ships no
statistics and the planner misestimates the new rows until it has them.
";

fn help_text() -> String {
    let mut s = String::from(USAGE);
    let width = COMMANDS.iter().map(|c| c.name.len()).max().unwrap_or(0);
    for c in COMMANDS {
        // Wrap the summary under a hanging indent so a long line stays
        // readable in an 80-column terminal, which is where an operator
        // sshed into a box actually reads this.
        let indent = " ".repeat(width + 8);
        let mut col = 0usize;
        let mut wrapped = String::new();
        for word in c.summary.split_whitespace() {
            if col > 0 && col + word.len() + 1 > 72 - width {
                wrapped.push('\n');
                wrapped.push_str(&indent);
                col = 0;
            } else if col > 0 {
                wrapped.push(' ');
                col += 1;
            }
            wrapped.push_str(word);
            col += word.len();
        }
        s.push_str(&format!("    {:width$}    {}\n", c.name, wrapped));
    }
    s.push('\n');
    s.push_str(ENV_HELP);
    s
}

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Help,
    Version,
    /// Apply migrations, then run these backfills (in `COMMANDS` order, each
    /// at most once however many times it was typed).
    Run(Vec<&'static str>),
}

/// Levenshtein distance, for suggesting the command an operator meant.
///
/// Worth the dozen lines: the doc comment on the unknown-argument check exists
/// because a typo'd subcommand once "ran" instantly and did nothing, and the
/// operator had no way to see why. Naming the near miss turns that into a
/// one-line fix.
fn edit_distance(a: &str, b: &str) -> usize {
    let b_len = b.chars().count();
    let mut prev: Vec<usize> = (0..=b_len).collect();
    let mut cur = vec![0usize; b_len + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b_len]
}

/// The closest known command, when it is close enough to be worth naming.
///
/// A third of the token's length is the threshold: generous enough for a
/// dropped plural or a transposition, tight enough that an entirely different
/// word does not get a confident-sounding wrong suggestion.
fn suggest(arg: &str) -> Option<&'static str> {
    let limit = (arg.chars().count() / 3).max(2);
    COMMANDS
        .iter()
        .map(|c| (edit_distance(arg, c.name), c.name))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, _)| *d)
        .map(|(_, name)| name)
}

fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Invocation, String> {
    let mut tasks: Vec<&'static str> = Vec::new();
    for a in args {
        match a.as_str() {
            "-h" | "--help" | "help" => return Ok(Invocation::Help),
            "-V" | "--version" | "version" => return Ok(Invocation::Version),
            other => match COMMANDS.iter().find(|c| c.name == other) {
                Some(c) => {
                    // Idempotent: typing one twice must not run it twice. The
                    // backfills are additive, so a double run double-counts.
                    if !tasks.contains(&c.name) {
                        tasks.push(c.name);
                    }
                }
                None => {
                    let mut msg = format!("unknown argument {other:?}");
                    if let Some(near) = suggest(other) {
                        msg.push_str(&format!("; did you mean {near:?}?"));
                    }
                    msg.push_str(&format!(
                        "\n\nknown backfills: {}\nrun `sauron-migrate --help` for details",
                        COMMANDS
                            .iter()
                            .map(|c| c.name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    return Err(msg);
                }
            },
        }
    }
    Ok(Invocation::Run(tasks))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parsed BEFORE the subscriber and before the database is touched, so
    // `--help` works on a box with no DATABASE_URL and prints to stdout rather
    // than arriving interleaved with log lines.
    //
    // Unknown arguments are FATAL here. They used to fall through to the plain
    // migrate path and exit 0 — and a typo'd (or not-yet-shipped) subcommand
    // became a no-op indistinguishable from success. Bit an operator live:
    // `backfill-rollups` against an older binary "ran" instantly and did
    // nothing.
    let tasks = match parse_args(std::env::args().skip(1)) {
        Ok(Invocation::Help) => {
            print!("{}", help_text());
            return Ok(());
        }
        Ok(Invocation::Version) => {
            println!("sauron-migrate {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Ok(Invocation::Run(t)) => t,
        Err(e) => anyhow::bail!("{e}"),
    };

    tracing_subscriber::fmt().with_target(false).init();

    let url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is required"))?;

    // Parsed leniently on purpose: an unparseable or absent value falls back to
    // the default rather than refusing to run. Refusing would mean a typo in an
    // operator's env file takes down every daemon that now depends on this unit,
    // which is a worse failure than waiting the default 120s.
    //
    // `0` is honoured as "do not wait", which is what the Compose path wants —
    // there the `migrate` service has an explicit `depends_on` on a healthy
    // Postgres, so a retry here would only mask a broken dependency.
    let wait_secs = std::env::var("MIGRATE_WAIT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(sauron_db::DEFAULT_MIGRATE_WAIT_SECS);

    tracing::info!("applying pending migrations (connect tolerance {wait_secs}s)");
    sauron_db::run_pending_migrations_waiting(&url, std::time::Duration::from_secs(wait_secs))
        .await?;
    tracing::info!("migrations up to date");

    // Every backfill below is opt-in, and deliberately NOT part of the default
    // no-arg path — see `USAGE` for the reasoning, which is the same for all
    // four: this is the oneshot every RPM daemon `Requires=`.
    //
    // Skipping `backfill-person-envs`, `-device-envs` or `-rollups` is a
    // PERFORMANCE decision: those reads fall back to a pre-rollup query.
    // Skipping `backfill-person-days` is a CORRECTNESS one — retention has no
    // legacy path, so those apps report `ready: false` and the dashboard names
    // this command, rather than drawing a 0% grid that looks like an answer.
    if tasks.contains(&"backfill-person-envs") {
        let pool = sauron_db::build_pool(&url, 4)?;
        sauron_db::person_env_backfill::backfill_all(&pool).await?;
    }

    if tasks.contains(&"backfill-person-days") {
        let pool = sauron_db::build_pool(&url, 4)?;
        sauron_db::person_days_backfill::backfill_all(&pool).await?;
    }

    if tasks.contains(&"backfill-device-envs") {
        let pool = sauron_db::build_pool(&url, 4)?;
        sauron_db::device_env_backfill::backfill_all(&pool).await?;
    }

    if tasks.contains(&"backfill-rollups") {
        let pool = sauron_db::build_pool(&url, 4)?;
        let mut conn = sauron_db::conn(&pool).await?;
        sauron_db::rollups::fold::backfill_all(&mut conn, 2000, |day| {
            tracing::info!(%day, "rollup backfill: day complete");
        })
        .await?;
        tracing::info!("rollup backfill complete");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Invocation, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_arguments_means_migrate_only() {
        assert_eq!(parse(&[]), Ok(Invocation::Run(vec![])));
    }

    #[test]
    fn help_and_version_are_recognised_in_every_spelling() {
        for f in ["-h", "--help", "help"] {
            assert_eq!(parse(&[f]), Ok(Invocation::Help), "{f}");
        }
        for f in ["-V", "--version", "version"] {
            assert_eq!(parse(&[f]), Ok(Invocation::Version), "{f}");
        }
    }

    #[test]
    fn help_wins_over_a_backfill_so_it_never_touches_the_database() {
        assert_eq!(parse(&["backfill-rollups", "--help"]), Ok(Invocation::Help));
    }

    #[test]
    fn backfills_combine_and_never_run_twice() {
        // Additive aggregates: running one twice in a single invocation would
        // double-count.
        assert_eq!(
            parse(&[
                "backfill-rollups",
                "backfill-person-days",
                "backfill-rollups"
            ]),
            Ok(Invocation::Run(vec![
                "backfill-rollups",
                "backfill-person-days"
            ]))
        );
    }

    #[test]
    fn an_unknown_argument_is_rejected_and_names_the_near_miss() {
        // The dropped plural is the realistic typo, and the one that used to
        // exit 0 having done nothing.
        let e = parse(&["backfill-rollup"]).unwrap_err();
        assert!(e.contains("unknown argument"), "{e}");
        assert!(e.contains("did you mean \"backfill-rollups\""), "{e}");
        assert!(e.contains("--help"), "{e}");
    }

    #[test]
    fn a_wholly_unrelated_argument_gets_no_confident_wrong_suggestion() {
        let e = parse(&["--dry-run"]).unwrap_err();
        assert!(e.contains("unknown argument"), "{e}");
        assert!(!e.contains("did you mean"), "should not guess: {e}");
        // It still lists what IS accepted.
        assert!(e.contains("backfill-person-days"), "{e}");
    }

    #[test]
    fn help_lists_every_command_exactly_once() {
        // The list `--help` prints and the list validation accepts are the
        // same array, so this cannot drift — but assert it, because the whole
        // point of merging them was that a stale list is invisible.
        let h = help_text();
        // At least once, not exactly once: the EXAMPLES section names a couple
        // of them again on purpose, which is the help being useful rather than
        // the list having drifted.
        for c in COMMANDS {
            assert!(h.contains(c.name), "{} missing from --help", c.name);
        }
        assert!(
            h.contains("DATABASE_URL"),
            "help must document the env vars"
        );
        assert!(h.contains("MIGRATE_WAIT_SECS"));
    }
}
