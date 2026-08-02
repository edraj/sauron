//! What a message *is*, and how often the same person may receive one.
//!
//! This enum is the authority for `mail_outbox.kind`, which deliberately carries
//! no CHECK constraint: the value set keeps growing, and the slice that adds the
//! fifth kind must not also have to widen a CHECK on a table holding live
//! credentials. Kind and dedup window have to change together, so keeping both
//! here is what stops them drifting apart.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailKind {
    PasswordReset,
    NotificationDigest,
    PersonalNotification,
    SmtpTest,
}

impl MailKind {
    /// The value written to `mail_outbox.kind`. Stable wire strings: an operator
    /// requeueing a row by hand and a dedup probe both match on them.
    pub fn as_str(&self) -> &'static str {
        match self {
            MailKind::PasswordReset => "password_reset",
            MailKind::NotificationDigest => "notification_digest",
            MailKind::PersonalNotification => "personal_notification",
            MailKind::SmtpTest => "smtp_test",
        }
    }

    /// Per-recipient suppression window. `Duration::ZERO` disables it.
    ///
    /// This is the only chokepoint where a per-recipient cap can live. Treating
    /// it as "the relay's problem" is wrong: the relay is the operator's own, and
    /// it is what gets throttled and blacklisted. With a Redis limiter alone an
    /// unauthenticated attacker sends roughly 14k mails a day to one victim, and
    /// that limiter degrades to a *per-process* window on any Redis blip,
    /// multiplied by replica count.
    pub fn dedup_window(&self) -> Duration {
        match self {
            MailKind::PasswordReset => Duration::from_secs(300),
            MailKind::NotificationDigest => Duration::from_secs(900),
            MailKind::PersonalNotification => Duration::ZERO,
            MailKind::SmtpTest => Duration::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn wire_strings_are_stable_and_distinct() {
        let all = [
            MailKind::PasswordReset,
            MailKind::NotificationDigest,
            MailKind::PersonalNotification,
            MailKind::SmtpTest,
        ];
        let names: Vec<&str> = all.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "password_reset",
                "notification_digest",
                "personal_notification",
                "smtp_test",
            ]
        );
        // These strings are written into `mail_outbox.kind`, which has no CHECK.
        // Nothing in the database will notice a collision, so the test does.
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
    }

    #[test]
    fn dedup_windows_are_the_reviewed_values() {
        // 5 minutes: the backoff ladder (about 45 minutes) fits inside even the
        // shorter of PasswordReset's two token lifetimes, and 5 minutes is short
        // enough not to defeat a user who genuinely did not receive the first mail.
        assert_eq!(
            MailKind::PasswordReset.dedup_window(),
            Duration::from_secs(300)
        );
        // 15 minutes bounds how stale a delivered digest can be.
        assert_eq!(
            MailKind::NotificationDigest.dedup_window(),
            Duration::from_secs(900)
        );
        // ZERO, and it must stay zero. A user is capped at 20 notifications an
        // hour upstream, so a 15-minute window here would suppress roughly 16 of
        // them — and suppression is indistinguishable from success, because the
        // enqueue returns the same `Ok(None)` it returns for a deliberate discard.
        assert_eq!(
            MailKind::PersonalNotification.dedup_window(),
            Duration::ZERO
        );
        // An operator clicking "test" twice must get two mails.
        assert_eq!(MailKind::SmtpTest.dedup_window(), Duration::ZERO);
    }
}
