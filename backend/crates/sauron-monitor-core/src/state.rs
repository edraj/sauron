//! The up/down state machine (pure). Given the current persisted counters and a
//! fresh probe result, decide the new status and whether a transition fires.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Unknown,
    Up,
    Down,
    Paused,
}

pub fn status_str(s: Status) -> &'static str {
    match s {
        Status::Unknown => "unknown",
        Status::Up => "up",
        Status::Down => "down",
        Status::Paused => "paused",
    }
}

/// The result of one probe (network outcome already evaluated to up/down).
#[derive(Clone, Debug)]
pub struct ProbeResult {
    pub up: bool,
    pub status_code: Option<i32>,
    pub response_time_ms: Option<i32>,
    pub error: Option<String>,
}

/// The monitor's persisted state the decision needs.
#[derive(Clone, Debug)]
pub struct MonitorState {
    pub status: Status,
    pub consecutive_failures: i32,
    pub consecutive_successes: i32,
    pub failure_threshold: i32,
    pub recovery_threshold: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransitionKind {
    None,
    WentDown,
    WentUp,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub new_status: Status,
    pub consecutive_failures: i32,
    pub consecutive_successes: i32,
    pub transition: TransitionKind,
}

/// Apply one probe result to the monitor's state.
///
/// - A failure increments the failure counter and resets successes; once it
///   reaches `failure_threshold` and we are not already down, we go **down**
///   (fires from `Up` *and* `Unknown` — a service that starts down should alert).
/// - A success increments the success counter and resets failures; from `Down`
///   once it reaches `recovery_threshold` we go **up** (fires `WentUp`). From
///   `Unknown`, the first qualifying success sets `Up` **silently** (no false
///   "recovered" alert for something that was never known-down).
pub fn apply(state: &MonitorState, result: &ProbeResult) -> Outcome {
    let mut cf = state.consecutive_failures;
    let mut cs = state.consecutive_successes;
    if result.up {
        cs += 1;
        cf = 0;
    } else {
        cf += 1;
        cs = 0;
    }

    let mut new_status = state.status;
    let mut transition = TransitionKind::None;

    if result.up {
        if cs >= state.recovery_threshold {
            match state.status {
                Status::Down => {
                    new_status = Status::Up;
                    transition = TransitionKind::WentUp;
                }
                Status::Unknown => {
                    new_status = Status::Up; // silent bootstrap
                }
                _ => {}
            }
        }
    } else if state.status != Status::Down && cf >= state.failure_threshold {
        new_status = Status::Down;
        transition = TransitionKind::WentDown;
    }

    Outcome {
        new_status,
        consecutive_failures: cf,
        consecutive_successes: cs,
        transition,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(status: Status, cf: i32, cs: i32) -> MonitorState {
        MonitorState {
            status,
            consecutive_failures: cf,
            consecutive_successes: cs,
            failure_threshold: 2,
            recovery_threshold: 1,
        }
    }
    fn up() -> ProbeResult {
        ProbeResult {
            up: true,
            status_code: Some(200),
            response_time_ms: Some(5),
            error: None,
        }
    }
    fn down() -> ProbeResult {
        ProbeResult {
            up: false,
            status_code: None,
            response_time_ms: None,
            error: Some("timeout".into()),
        }
    }

    #[test]
    fn single_failure_does_not_trip_when_threshold_is_two() {
        let o = apply(&st(Status::Up, 0, 5), &down());
        assert_eq!(o.new_status, Status::Up);
        assert_eq!(o.consecutive_failures, 1);
        assert_eq!(o.transition, TransitionKind::None);
    }

    #[test]
    fn second_consecutive_failure_goes_down() {
        let o = apply(&st(Status::Up, 1, 0), &down());
        assert_eq!(o.new_status, Status::Down);
        assert_eq!(o.transition, TransitionKind::WentDown);
    }

    #[test]
    fn recovery_after_one_success() {
        let o = apply(&st(Status::Down, 4, 0), &up());
        assert_eq!(o.new_status, Status::Up);
        assert_eq!(o.transition, TransitionKind::WentUp);
        assert_eq!(o.consecutive_successes, 1);
        assert_eq!(o.consecutive_failures, 0);
    }

    #[test]
    fn unknown_first_success_is_silent() {
        let o = apply(&st(Status::Unknown, 0, 0), &up());
        assert_eq!(o.new_status, Status::Up);
        assert_eq!(o.transition, TransitionKind::None);
    }

    #[test]
    fn unknown_then_two_failures_goes_down() {
        let o1 = apply(&st(Status::Unknown, 0, 0), &down());
        assert_eq!(o1.new_status, Status::Unknown);
        assert_eq!(o1.transition, TransitionKind::None);
        let o2 = apply(&st(Status::Unknown, o1.consecutive_failures, 0), &down());
        assert_eq!(o2.new_status, Status::Down);
        assert_eq!(o2.transition, TransitionKind::WentDown);
    }

    #[test]
    fn flap_suppressed_down_then_up_stays_up() {
        // one failure, then success: never left Up, no incident
        let o1 = apply(&st(Status::Up, 0, 3), &down());
        assert_eq!(o1.new_status, Status::Up);
        let o2 = apply(&st(Status::Up, o1.consecutive_failures, 0), &up());
        assert_eq!(o2.new_status, Status::Up);
        assert_eq!(o2.transition, TransitionKind::None);
    }

    #[test]
    fn status_strings() {
        assert_eq!(status_str(Status::Unknown), "unknown");
        assert_eq!(status_str(Status::Up), "up");
        assert_eq!(status_str(Status::Down), "down");
        assert_eq!(status_str(Status::Paused), "paused");
    }
}
