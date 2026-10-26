//! Claude, through the Messages API (raw HTTPS: there's no official Rust SDK).

use persephone_core::UnixNanos;
use serde_json::{Value, json};

use super::{Config, DEFAULT_MAX_TOKENS, Request, date_nanos, error_message, send_with_retries};

const ANTHROPIC_URL: &str = "https://api.anthropic.com";
const VERSION: &str = "2026-10-21";
/// Models whose declined requests are retried on another model server-side, by refusal
/// category, when asked to.
const WITH_FALLBACKS: [&str; 4] = [
    "claude-fable-5-1",
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-sonnet-5-5",
];
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const OAUTH_BETA: &str = "oauth-2025-04-20";

/// The credential header and any beta it needs.
fn credentials(config: &Config) -> Result<(&'static str, String, Option<&'static str>), String> {
    if let Some(key) = &config.anthropic_api_key {
        Ok(("x-api-key", key.clone(), None))
    } else if let Some(token) = &config.anthropic_auth_token {
        Ok(("authorization", format!("Bearer {token}"), Some(OAUTH_BETA)))
    } else {
        Err(
            "there's no Anthropic API key: set ANTHROPIC_API_KEY, or anthropic_api_key in ai.json"
                .into(),
        )
    }
}

fn base(config: &Config) -> &str {
    config
        .anthropic_base_url
        .as_deref()
        .unwrap_or(ANTHROPIC_URL)
        .trim_end_matches('/')
}

pub(super) fn complete(
    http: &reqwest::blocking::Client,
    config: &Config,
    model: &str,
    request: &Request,
) -> Result<String, String> {
    let (header, credential, auth_beta) = credentials(config)?;
    let mut betas: Vec<&str> = auth_beta.into_iter().collect();
    let mut body = json!({
        "model": model,
        "max_tokens": request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "messages": [{"role": "user", "content": request.prompt}],
    });
    if let Some(system) = &request.system {
        body["system"] = json!(system);
    }
    if let Some(effort) = request.effort {
        body["output_config"] = json!({"effort": effort.name()});
    }
    if WITH_FALLBACKS.contains(&model) {
        body["fallbacks"] = json!("default");
        betas.push(FALLBACK_BETA);
    }

    let url = format!("{}/v1/messages", base(config));
    let response = send_with_retries(|| {
        let builder = http
            .post(&url)
            .header("anthropic-version", VERSION)
            .header(header, &credential)
            .json(&body);
        if betas.is_empty() {
            builder
        } else {
            builder.header("anthropic-beta", betas.join(","))
        }
    })?;
    let status = response.status().as_u16();
    let text = response.text().map_err(|e| e.to_string())?;
    if !(200..300).contains(&status) {
        return Err(error_message(status, &text));
    }
    answer(model, &text)
}

/// The text of a Messages API reply, after checking it wasn't declined or cut short.
fn answer(model: &str, body: &str) -> Result<String, String> {
    let reply: Value =
        serde_json::from_str(body).map_err(|e| format!("couldn't read {model}'s reply: {e}"))?;
    let stop = reply["stop_reason"].as_str().unwrap_or_default();
    if stop == "refusal" {
        let category = reply
            .pointer("/stop_details/category")
            .and_then(Value::as_str)
            .unwrap_or("no category given");
        return Err(format!("{model} declined to answer ({category})"));
    }
    // Thinking blocks come back empty by default; the answer is in the text blocks.
    let text: String = reply["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect();
    if text.is_empty() {
        return Err(if stop == "max_tokens" {
            format!("{model} used up max_tokens before answering; allow it more")
        } else {
            format!("{model} replied without text")
        });
    }
    Ok(text)
}

pub(super) fn release_date(
    http: &reqwest::blocking::Client,
    config: &Config,
    model: &str,
) -> Option<UnixNanos> {
    let (header, credential, auth_beta) = credentials(config).ok()?;
    let mut request = http
        .get(format!("{}/v1/models/{model}", base(config)))
        .header("anthropic-version", VERSION)
        .header(header, credential);
    if let Some(beta) = auth_beta {
        request = request.header("anthropic-beta", beta);
    }
    let response = request.send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let info: Value = response.json().ok()?;
    date_nanos(info["created_at"].as_str()?)
}
