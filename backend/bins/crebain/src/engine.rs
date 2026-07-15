//! The load engine: one tokio task per virtual user, each emitting on staggered
//! interval timers until a shared deadline. Every request's outcome flows over a
//! single mpsc channel to the metrics aggregator (one owner, no locks).

use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{interval_at, Interval, MissedTickBehavior};

use sauron_core::envelope::Envelope;

use crate::cli::RunConfig;
use crate::client::IngestClient;
use crate::dsn::Target;
use crate::generator::{self, ItemCounts};
use crate::metrics::{self, Sample, Summary};
use crate::user::VirtualUser;

/// Run the load to completion and return the aggregated [`Summary`].
pub async fn run(cfg: &RunConfig, target: &Target) -> anyhow::Result<Summary> {
    let client = IngestClient::new(target, cfg.gzip)?;
    let (tx, rx) = mpsc::unbounded_channel::<Sample>();

    let start = Instant::now();
    let aggregator = tokio::spawn(metrics::aggregate(rx, cfg.users, start));

    let deadline = tokio::time::Instant::now() + cfg.duration;
    let mut users = JoinSet::new();
    for i in 0..cfg.users {
        users.spawn(run_user(
            client.clone(),
            tx.clone(),
            i,
            cfg.users,
            cfg.event_interval,
            cfg.issue_interval,
            deadline,
        ));
    }
    // Only the user tasks hold senders now → the channel closes (and the
    // aggregator returns) once every user has stopped at the deadline.
    drop(tx);

    while let Some(res) = users.join_next().await {
        // A panicked user must not abort the whole run.
        if let Err(e) = res {
            if e.is_panic() {
                eprintln!("crebain: a user task panicked: {e}");
            }
        }
    }

    Ok(aggregator.await?)
}

#[allow(clippy::too_many_arguments)]
async fn run_user(
    client: IngestClient,
    tx: mpsc::UnboundedSender<Sample>,
    index: usize,
    n: usize,
    event_interval: Option<Duration>,
    issue_interval: Option<Duration>,
    deadline: tokio::time::Instant,
) {
    let mut user = VirtualUser::new(index);

    // One-time identify when the user first appears.
    send(&client, &tx, generator::identify_envelope(&user)).await;

    // Phase each user's cadence across the first period so N users don't all fire
    // at t=0 (a thundering herd).
    let mut ev = event_interval.map(|p| phased(p, index, n));
    let mut is = issue_interval.map(|p| phased(p, index, n));

    let mut ev_seq = 0u64;
    let mut is_seq = 0u64;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = maybe_tick(&mut ev) => {
                ev_seq += 1;
                user.advance_screen(ev_seq);
                send(&client, &tx, generator::event_envelope(&user, ev_seq)).await;
                user.events_sent += 1;
            }
            _ = maybe_tick(&mut is) => {
                is_seq += 1;
                send(&client, &tx, generator::issue_envelope(&user, is_seq)).await;
                user.issues_sent += 1;
            }
        }
    }
}

/// A tick source that is either a real interval or (when the rate is 0) a future
/// that never completes — so the `select!` branch is effectively disabled.
async fn maybe_tick(iv: &mut Option<Interval>) {
    match iv {
        Some(i) => {
            i.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// An interval whose first tick is offset by the user's slot fraction of the
/// period, and which skips (rather than bursts) missed ticks under backpressure.
fn phased(period: Duration, index: usize, n: usize) -> Interval {
    let frac = if n > 0 { index as f64 / n as f64 } else { 0.0 };
    let start = tokio::time::Instant::now() + period.mul_f64(frac);
    let mut iv = interval_at(start, period);
    iv.set_missed_tick_behavior(MissedTickBehavior::Skip);
    iv
}

async fn send(client: &IngestClient, tx: &mpsc::UnboundedSender<Sample>, env: Envelope) {
    let counts = ItemCounts::of(&env);
    let outcome = client.send(&env).await;
    // If the aggregator is gone the run is ending; dropping the sample is fine.
    let _ = tx.send(Sample { outcome, counts });
}
