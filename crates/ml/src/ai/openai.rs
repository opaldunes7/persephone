//! OpenAI's chat completions API, which Ollama, LM Studio and others also speak.

use persephone_core::UnixNanos;
use serde_json::{Value, json};

use super::{Request, error_message, send_with_retries};

pub(super) const OPENAI_URL: &str = "https://api.openai.com/v1";
pub(super) const OLLAMA_URL: &str = "http://localhost:11434/v1";

pub(super) fn complete(
    http: &reqwest::blocking::Client,
    base: &str,
    key: Option<&str>,
    model: &str,
    request: &Request,
) -> Result<String, String> {
    let base = base.trim_end_matches('/');
    if key.is_none() && base == OPENAI_URL {
        return Err(
            "there's no OpenAI API key: set OPENAI_API_KEY, or openai_api_key in ai.json".into(),
        );
    }
    let mut messages = Vec::new();
    if let Some(system) = &request.system {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.push(json!({"role": "user", "content": request.prompt}));
    let mut body = json!({"model": model, "messages": messages});
    if let Some(max) = request.max_tokens {
        // OpenAI's own API wants the newer name; local servers know the older one.
        let field = if base == OPENAI_URL {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body[field] = json!(max);
    }

    let url = format!("{base}/chat/completions");
    let response = send_with_retries(|| {
        let builder = http.post(&url).json(&body);
        match key {
            Some(key) => builder.bearer_auth(key),
            None => builder,
        }
    })?;
    let status = response.status().as_u16();
    let text = response.text().map_err(|e| e.to_string())?;
    if !(200..300).contains(&status) {
        return Err(error_message(status, &text));
    }
    let reply: Value =
        serde_json::from_str(&text).map_err(|e| format!("couldn't read {model}'s reply: {e}"))?;
    let choice = &reply["choices"][0];
    if choice["finish_reason"] == "content_filter" {
        return Err(format!("{model} declined to answer"));
    }
    match choice.pointer("/message/content").and_then(Value::as_str) {
        Some(content) if !content.is_empty() => Ok(content.to_owned()),
        _ => Err(format!("{model} replied without text")),
    }
}

pub(super) fn release_date(
    http: &reqwest::blocking::Client,
    base: &str,
    key: Option<&str>,
    model: &str,
) -> Option<UnixNanos> {
    let mut request = http.get(format!("{}/models/{model}", base.trim_end_matches('/')));
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    let response = request.send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let info: Value = response.json().ok()?;
    let seconds = info["created"].as_u64()?;
    seconds.checked_mul(1_000_000_000)
}
