//! Event enrichment: fold request-derived signals (User-Agent) into the
//! envelope context before it is persisted, and derive a stable device identity
//! used to roll signals up into the `devices` / `sessions` tables.

use serde_json::{json, Value};

use sauron_core::envelope::IngestJob;

/// Merge the SDK-provided context with a parsed User-Agent block and a derived
/// `device_key`.
pub fn enrich_context(job: &IngestJob) -> Value {
    let mut ctx = serde_json::to_value(&job.context).unwrap_or_else(|_| json!({}));

    if let Some(ua) = job.user_agent.as_deref() {
        let parser = woothee::parser::Parser::new();
        if let Some(r) = parser.parse(ua) {
            if let Value::Object(map) = &mut ctx {
                map.insert(
                    "ua".to_string(),
                    json!({
                        "name": r.name,
                        "category": r.category,
                        "os": r.os,
                        "os_version": r.os_version,
                        "browser_version": r.version,
                        "vendor": r.vendor,
                    }),
                );
            }
        }
    }

    // Stamp the derived device identity so the stored snapshot carries it too.
    let info = device_info(&ctx);
    if let (Value::Object(map), Some(key)) = (&mut ctx, info.device_key.clone()) {
        map.insert("device_key".to_string(), json!(key));
    }

    ctx
}

/// The normalized hardware/runtime descriptor extracted from an enriched
/// context, plus a stable key used to group signals by device.
#[derive(Debug, Clone, Default)]
pub struct DeviceInfo {
    pub device_key: Option<String>,
    pub family: Option<String>,
    pub model: Option<String>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub arch: Option<String>,
    pub browser: Option<String>,
}

/// Read a non-empty string at `ctx[a][b]`.
fn nested(ctx: &Value, a: &str, b: &str) -> Option<String> {
    ctx.get(a)
        .and_then(|v| v.get(b))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Extract the device descriptor from an enriched context. `device_key` prefers
/// an SDK-provided persistent install id (`device.device_id`), falling back to a
/// deterministic descriptor so web clients without an install id still cluster
/// by hardware/OS/browser. Returns `device_key: None` when nothing is known.
pub fn device_info(ctx: &Value) -> DeviceInfo {
    let family = nested(ctx, "device", "family");
    let model = nested(ctx, "device", "model");
    let arch = nested(ctx, "device", "arch");
    let os_name = nested(ctx, "os", "name").or_else(|| nested(ctx, "ua", "os"));
    let os_version = nested(ctx, "os", "version").or_else(|| nested(ctx, "ua", "os_version"));
    let browser = nested(ctx, "runtime", "name").or_else(|| nested(ctx, "ua", "name"));
    let device_id = nested(ctx, "device", "device_id");

    let device_key = device_id.or_else(|| {
        let parts: Vec<&str> = [&family, &model, &os_name, &arch, &browser]
            .into_iter()
            .filter_map(|o| o.as_deref())
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("|"))
        }
    });

    DeviceInfo {
        device_key,
        family,
        model,
        os_name,
        os_version,
        arch,
        browser,
    }
}
