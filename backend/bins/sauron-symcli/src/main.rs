//! `sauron-symcli` — upload source maps / symbol artifacts to a Sauron app.
//!
//! Thin wrapper over `POST /v1/apps/{app}/artifacts` (raw file body + query
//! params). Auth is a dashboard JWT bearer token.
//!
//! Usage:
//!   sauron-symcli upload-sourcemap --api <url> --token <jwt> --app <uuid> \
//!       --release <r> --name <minified-path> [--dist <d>] <file.map>
//!
//!   sauron-symcli upload-obfuscation-map --api <url> --token <jwt> --app <uuid> \
//!       --platform <android|ios> --debug-id <build-id> map.json
//!
//!   sauron-symcli upload --api <url> --token <jwt> --app <uuid> \
//!       --kind <js_sourcemap|dart_symbols|dart_obfuscation_map> \
//!       --platform <web|android|ios> \
//!       [--arch <a>] [--release <r>] [--dist <d>] [--name <n>] [--debug-id <id>] <file>
//!
//! An obfuscation map is what makes an obfuscated Dart EXCEPTION TYPE readable —
//! the symbols file only covers stack frames. Upload BOTH for an obfuscated
//! build, under the same `--debug-id`; the id is reported as `derived_debug_id`
//! in the response to the `upload-dart` for that build.
//!
//! (Dart split-debug-info directory walking + build-id derivation lands with the
//! Flutter pipeline in a later slice.)

use std::collections::HashMap;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Vec<String>) -> anyhow::Result<()> {
    let Some((cmd, rest)) = args.split_first() else {
        print_help();
        anyhow::bail!("missing subcommand");
    };

    match cmd.as_str() {
        "upload" => upload(rest, None, None).await,
        "upload-sourcemap" => upload(rest, Some("js_sourcemap"), Some("web")).await,
        "upload-dart" => upload(rest, Some("dart_symbols"), None).await,
        "upload-obfuscation-map" => upload(rest, Some("dart_obfuscation_map"), None).await,
        "-h" | "--help" | "help" => {
            print_help();
            Ok(())
        }
        other => {
            print_help();
            anyhow::bail!("unknown subcommand '{other}'");
        }
    }
}

/// Parse `--flag value` pairs and a single trailing positional (the file path).
fn parse(rest: &[String]) -> anyhow::Result<(HashMap<String, String>, String)> {
    let mut flags = HashMap::new();
    let mut positional = None;
    let mut i = 0;
    while i < rest.len() {
        let a = &rest[i];
        if let Some(name) = a.strip_prefix("--") {
            let val = rest
                .get(i + 1)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("flag --{name} needs a value"))?;
            flags.insert(name.to_string(), val);
            i += 2;
        } else {
            if positional.is_some() {
                anyhow::bail!("unexpected extra argument '{a}'");
            }
            positional = Some(a.clone());
            i += 1;
        }
    }
    let file = positional.ok_or_else(|| anyhow::anyhow!("missing <file> argument"))?;
    Ok((flags, file))
}

fn require<'a>(flags: &'a HashMap<String, String>, key: &str) -> anyhow::Result<&'a String> {
    flags
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("missing required --{key}"))
}

async fn upload(
    rest: &[String],
    kind_default: Option<&str>,
    platform_default: Option<&str>,
) -> anyhow::Result<()> {
    let (flags, file) = parse(rest)?;

    let api = require(&flags, "api")?.trim_end_matches('/').to_string();
    let token = require(&flags, "token")?;
    let app = require(&flags, "app")?;
    let kind = flags
        .get("kind")
        .map(String::as_str)
        .or(kind_default)
        .ok_or_else(|| anyhow::anyhow!("missing required --kind"))?;
    let platform = flags
        .get("platform")
        .map(String::as_str)
        .or(platform_default)
        .ok_or_else(|| anyhow::anyhow!("missing required --platform"))?;

    let bytes = std::fs::read(&file).map_err(|e| anyhow::anyhow!("cannot read '{file}': {e}"))?;

    // Build query params from the flags that were provided.
    let mut query: Vec<(&str, String)> = vec![
        ("kind", kind.to_string()),
        ("platform", platform.to_string()),
    ];
    for key in ["arch", "release", "dist", "name", "debug_id"] {
        // Accept both --debug-id and --debug_id spellings.
        let flag_key = key.replace('_', "-");
        if let Some(v) = flags.get(key).or_else(|| flags.get(&flag_key)) {
            query.push((key, v.clone()));
        }
    }

    let url = format!("{api}/v1/apps/{app}/artifacts");
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .query(&query)
        .body(bytes)
        .send()
        .await?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if status.is_success() {
        println!("{status}: {text}");
        Ok(())
    } else {
        anyhow::bail!("upload failed ({status}): {text}");
    }
}

fn print_help() {
    eprintln!(
        "sauron-symcli — upload source maps / symbol artifacts

USAGE:
  sauron-symcli upload-sourcemap --api <url> --token <jwt> --app <uuid> \\
      --release <r> --name <minified-path> [--dist <d>] <file.map>

  sauron-symcli upload-dart --api <url> --token <jwt> --app <uuid> \\
      --platform <android|ios> --arch <arm64|...> --debug-id <build-id> app.symbols

  sauron-symcli upload-obfuscation-map --api <url> --token <jwt> --app <uuid> \\
      --platform <android|ios> --debug-id <build-id> map.json

      The JSON from --extra-gen-snapshot-options=--save-obfuscation-map. This is
      what de-obfuscates the exception TYPE; upload-dart only covers frames. Use
      the same --debug-id as the upload-dart for this build.

  sauron-symcli upload --api <url> --token <jwt> --app <uuid> \\
      --kind <js_sourcemap|dart_symbols|dart_obfuscation_map> \\
      --platform <web|android|ios> \\
      [--arch <a>] [--release <r>] [--dist <d>] [--name <n>] [--debug-id <id>] <file>"
    );
}
