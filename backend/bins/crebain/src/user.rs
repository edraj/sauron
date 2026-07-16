//! A virtual user: a stable synthetic identity that emits signals for the whole
//! run. The pool is fixed at `--users`; identities never churn (the same N are
//! reused), matching the "reuse a user when the pool is full" model.

use serde_json::json;

/// Rotating pools of plausible values so different users look distinct.
const PLANS: &[&str] = &["free", "pro", "team", "enterprise"];
const COUNTRIES: &[&str] = &["US", "DE", "FR", "GB", "JP", "BR", "IN", "AU"];
pub const SCREENS: &[&str] = &["/", "/pricing", "/dashboard", "/settings", "/checkout"];

/// A single synthetic person.
#[derive(Debug, Clone)]
pub struct VirtualUser {
    pub index: usize,
    pub distinct_id: String,
    pub session_id: String,
    /// Current screen — advances as the user "navigates" between events.
    pub screen: &'static str,
    pub traits: serde_json::Value,
}

impl VirtualUser {
    pub fn new(index: usize) -> Self {
        let plan = PLANS[index % PLANS.len()];
        let country = COUNTRIES[index % COUNTRIES.len()];
        VirtualUser {
            index,
            distinct_id: format!("crebain-user-{index}"),
            session_id: format!("crebain-sess-{index}"),
            screen: SCREENS[index % SCREENS.len()],
            traits: json!({
                "plan": plan,
                "country": country,
                "synthetic": true,
                "cohort": index % 20,
            }),
        }
    }

    /// Advance the "current screen" so successive signals aren't all identical.
    pub fn advance_screen(&mut self, seq: u64) {
        self.screen = SCREENS[(self.index + seq as usize) % SCREENS.len()];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_ids_are_stable_and_unique() {
        let a = VirtualUser::new(0);
        let b = VirtualUser::new(1);
        assert_eq!(a.distinct_id, "crebain-user-0");
        assert_ne!(a.distinct_id, b.distinct_id);
        assert_eq!(VirtualUser::new(0).distinct_id, a.distinct_id);
    }
}
