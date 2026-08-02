//! A fixed, CLOSED library of value-shape detectors.
//!
//! Hand-rolled byte scanners, not regex. `regex` is only a transitive
//! dependency today (via `validator`, `woothee`, `arrow-string`), so declaring
//! it is a workspace edit — and admin-authored patterns would mean accepting
//! ReDoS authored by an org admin against a shared worker.
//!
//! Detectors are opt-in per policy and get their own much shorter window,
//! because enabling them removes the SQL prefilter entirely: every row in the
//! window is shipped out of Postgres and every string leaf is scanned. That is
//! roughly 20x the CPU and 20x the bytes of key mode.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detector {
    Email,
    PhoneE164,
    Ipv4,
    Ipv6,
    Jwt,
    Iban,
    SsnUs,
    CreditCard,
}

pub const ALL_DETECTORS: [Detector; 8] = [
    Detector::Email,
    Detector::PhoneE164,
    Detector::Ipv4,
    Detector::Ipv6,
    Detector::Jwt,
    Detector::Iban,
    Detector::SsnUs,
    Detector::CreditCard,
];

impl Detector {
    pub fn id(self) -> &'static str {
        match self {
            Detector::Email => "email",
            Detector::PhoneE164 => "phone_e164",
            Detector::Ipv4 => "ipv4",
            Detector::Ipv6 => "ipv6",
            Detector::Jwt => "jwt",
            Detector::Iban => "iban",
            Detector::SsnUs => "ssn_us",
            Detector::CreditCard => "credit_card",
        }
    }

    pub fn from_id(s: &str) -> Option<Detector> {
        ALL_DETECTORS.into_iter().find(|d| d.id() == s)
    }

    pub fn matches(self, s: &str) -> bool {
        match self {
            Detector::Email => is_email(s),
            Detector::PhoneE164 => is_e164(s),
            Detector::Ipv4 => is_ipv4(s),
            Detector::Ipv6 => is_ipv6(s),
            Detector::Jwt => is_jwt(s),
            Detector::Iban => is_iban(s),
            Detector::SsnUs => is_ssn_us(s),
            Detector::CreditCard => is_credit_card(s),
        }
    }
}

/// The first enabled detector this value trips, in `ALL_DETECTORS` order.
/// First-wins rather than all-matches: a finding carries one detector, and
/// reporting the same path once per detector would multiply the findings
/// table by eight for no extra information.
pub fn detect_first(enabled: &[Detector], s: &str) -> Option<Detector> {
    ALL_DETECTORS
        .into_iter()
        .find(|d| enabled.contains(d) && d.matches(s))
}

/// Load a policy's `detectors` jsonb, dropping ids this build does not know.
/// An unknown id is a downgrade artifact, not a reason to fail the scan.
pub fn parse_detectors(v: &Value) -> Vec<Detector> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|i| i.as_str())
        .filter_map(Detector::from_id)
        .collect()
}

fn is_email(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 6 || s.len() > 254 || s.contains(char::is_whitespace) {
        return false;
    }
    let mut parts = s.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    if local.is_empty() || domain.len() < 4 {
        return false;
    }
    // A bare hostname is not an address; require a dot with labels either side.
    match domain.rsplit_once('.') {
        Some((host, tld)) => {
            !host.is_empty() && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

fn is_e164(s: &str) -> bool {
    let s = s.trim();
    let digits = s.strip_prefix('+').unwrap_or(s);
    // E.164 is 8..15 digits and never starts with 0. No separators: a value
    // with spaces or dashes is a formatted local number, and treating it as
    // E.164 flags every order reference that happens to be numeric.
    (8..=15).contains(&digits.len())
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !digits.starts_with('0')
}

fn is_ipv4(s: &str) -> bool {
    let mut n = 0;
    for part in s.trim().split('.') {
        n += 1;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        if part.parse::<u16>().unwrap_or(999) > 255 {
            return false;
        }
    }
    n == 4
}

fn is_ipv6(s: &str) -> bool {
    let s = s.trim();
    // At least two colons keeps `2001:db8` and `08:30` out; hex-or-empty
    // groups accept the `::` compressed form without a full parser.
    s.matches(':').count() >= 2
        && s.len() >= 3
        && s.split(':')
            .all(|g| g.len() <= 4 && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn is_jwt(s: &str) -> bool {
    let parts: Vec<&str> = s.trim().split('.').collect();
    parts.len() == 3
        // A real header/payload is base64url of at least a small JSON object.
        && parts[0].len() >= 8
        && parts[1].len() >= 8
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'=')
        })
}

fn is_iban(s: &str) -> bool {
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let b = compact.as_bytes();
    (15..=34).contains(&b.len())
        && b[0].is_ascii_uppercase()
        && b[1].is_ascii_uppercase()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4..].iter().all(|c| c.is_ascii_alphanumeric())
}

fn is_ssn_us(s: &str) -> bool {
    // Shadow with the trimmed slice before indexing. Slicing the ORIGINAL `s`
    // after length-checking `s.trim()` panics on a value like
    // "\u{3000}\u{3000}123-45-6789": byte 4 lands inside a multi-byte
    // whitespace char. Scan input is arbitrary developer-supplied JSON, so a
    // single such string leaf would abort the scan worker.
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() != 11 || b[3] != b'-' || b[6] != b'-' {
        return false;
    }
    if !b
        .iter()
        .enumerate()
        .all(|(i, c)| i == 3 || i == 6 || c.is_ascii_digit())
    {
        return false;
    }
    // Area 000/666/9xx and group/serial 00/0000 are never issued; excluding
    // them is what keeps `000-00-0000` placeholders out of the report.
    let area = &s[0..3];
    area != "000"
        && area != "666"
        && !area.starts_with('9')
        && &s[4..6] != "00"
        && &s[7..11] != "0000"
}

fn is_credit_card(s: &str) -> bool {
    let digits: Vec<u32> = s
        .chars()
        .filter(|c| !matches!(c, ' ' | '-'))
        .map(|c| c.to_digit(10).unwrap_or(u32::MAX))
        .collect();
    if !(13..=19).contains(&digits.len()) || digits.contains(&u32::MAX) {
        return false;
    }
    // Luhn. Without it every 16-digit order id is a "credit card" and the
    // report is unreadable, which is how a privacy scan gets ignored.
    let mut sum = 0;
    for (i, d) in digits.iter().rev().enumerate() {
        let mut v = *d;
        if i % 2 == 1 {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
    }
    sum % 10 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_accepts_a_plus_tag() {
        assert!(Detector::Email.matches("jane+receipts@acme.co.uk"));
        assert!(Detector::Email.matches("a@b.co"));
        assert!(!Detector::Email.matches("jane@acme"));
        assert!(!Detector::Email.matches("@acme.com"));
        assert!(!Detector::Email.matches("jane@@acme.com"));
        assert!(!Detector::Email.matches("not an email"));
    }

    #[test]
    fn e164_with_and_without_plus() {
        assert!(Detector::PhoneE164.matches("+213770123456"));
        assert!(Detector::PhoneE164.matches("447700900123"));
        assert!(!Detector::PhoneE164.matches("12345"));
        assert!(!Detector::PhoneE164.matches("+0123456789"));
        assert!(!Detector::PhoneE164.matches("+44 7700 900123"));
    }

    #[test]
    fn ip_detectors() {
        assert!(Detector::Ipv4.matches("192.168.1.10"));
        assert!(!Detector::Ipv4.matches("999.1.1.1"));
        assert!(!Detector::Ipv4.matches("1.2.3"));
        assert!(Detector::Ipv6.matches("2001:db8::1"));
        assert!(!Detector::Ipv6.matches("2001:db8"));
    }

    #[test]
    fn jwt_needs_three_base64url_segments() {
        assert!(Detector::Jwt.matches("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abcDEF-_123"));
        assert!(!Detector::Jwt.matches("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0"));
        assert!(!Detector::Jwt.matches("a.b.c"));
    }

    #[test]
    fn iban_and_ssn() {
        assert!(Detector::Iban.matches("DE89370400440532013000"));
        assert!(!Detector::Iban.matches("DE8937"));
        assert!(Detector::SsnUs.matches("123-45-6789"));
        assert!(!Detector::SsnUs.matches("000-45-6789"));
        assert!(!Detector::SsnUs.matches("12345678"));
    }

    /// Every detector length-checks the trimmed value, so every detector must
    /// also SLICE the trimmed value. Indexing the untrimmed string panics when
    /// the padding is multi-byte whitespace and an index lands mid-character —
    /// reachable from any developer-supplied string leaf, and a panic here
    /// aborts the scan worker rather than skipping one value.
    #[test]
    fn padded_values_do_not_panic() {
        assert!(Detector::SsnUs.matches("\u{3000}\u{3000}123-45-6789"));
        assert!(!Detector::SsnUs.matches("\u{3000}\u{3000}000-45-6789"));
        assert!(Detector::SsnUs.matches("  123-45-6789  "));
    }

    /// Luhn, not "16 digits". A non-Luhn 16-digit number is an order id, a
    /// device serial or a padded counter, and flagging those is what makes a
    /// detector-mode report unreadable.
    #[test]
    fn credit_card_requires_luhn() {
        assert!(Detector::CreditCard.matches("4111111111111111"));
        assert!(Detector::CreditCard.matches("4111 1111 1111 1111"));
        assert!(!Detector::CreditCard.matches("4111111111111112"));
        assert!(!Detector::CreditCard.matches("1234567890123456"));
    }

    /// The negative corpus that keeps detector mode usable. Every one of these
    /// is something a real payload is full of.
    #[test]
    fn negative_corpus_is_clean() {
        let corpus = [
            "550e8400-e29b-41d4-a716-446655440000",
            "2026-08-01T03:00:00Z",
            "ORD-2026-0001",
            "checkout_started",
            "1.2.3",
            "v1.14.0",
            "sha256:9f86d081884c7d659a2feaa0c55ad015",
            "",
            "0",
        ];
        for s in corpus {
            let hit = detect_first(&ALL_DETECTORS, s);
            assert!(hit.is_none(), "{s} was flagged as {hit:?}");
        }
    }

    #[test]
    fn ids_round_trip() {
        for d in ALL_DETECTORS {
            assert_eq!(Detector::from_id(d.id()), Some(d));
        }
        assert_eq!(Detector::from_id("nope"), None);
    }

    #[test]
    fn parse_drops_unknown_ids() {
        let v = serde_json::json!(["email", "nope", 7, "credit_card"]);
        assert_eq!(
            parse_detectors(&v),
            vec![Detector::Email, Detector::CreditCard]
        );
    }
}
