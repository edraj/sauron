//! The JSON body POSTed to a monitor's `webhook_url` on each state change.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct WebhookPayload<'a> {
    pub monitor_id: Uuid,
    pub name: &'a str,
    pub project_id: Uuid,
    pub status: &'a str,
    pub previous_status: &'a str,
    pub at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incident_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<&'a str>,
    pub target: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use uuid::Uuid;

    #[test]
    fn serializes_expected_shape() {
        let p = WebhookPayload {
            monitor_id: Uuid::nil(),
            name: "api",
            project_id: Uuid::nil(),
            status: "down",
            previous_status: "up",
            at: chrono::Utc.timestamp_opt(0, 0).unwrap(),
            incident_id: Some(Uuid::nil()),
            cause: Some("HTTP 503"),
            target: "https://example.com",
        };
        let v: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert_eq!(v["status"], "down");
        assert_eq!(v["previous_status"], "up");
        assert_eq!(v["cause"], "HTTP 503");
        assert_eq!(v["target"], "https://example.com");
        assert_eq!(v["name"], "api");
    }
}
