//! The AI client against a stand-in HTTP server: no network, no keys.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
};

use serde_json::Value;

use super::*;

/// A request the stand-in server received.
#[derive(Clone, Debug)]
struct Seen {
    line: String,
    headers: HashMap<String, String>,
    body: String,
}

impl Seen {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// Answers each connection with the next canned (status, extra headers, body).
struct Server {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn serve(replies: Vec<(u16, &'static str, String)>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    thread::spawn(move || {
        for (status, extra, body) in replies {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut headers = HashMap::new();
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                let (name, value) = header.split_once(':').unwrap();
                headers.insert(name.trim().to_lowercase(), value.trim().to_owned());
            }
            let length = headers
                .get("content-length")
                .map_or(0, |l| l.parse().unwrap());
            let mut content = vec![0; length];
            reader.read_exact(&mut content).unwrap();
            log.lock().unwrap().push(Seen {
                line: line.trim_end().to_owned(),
                headers,
                body: String::from_utf8(content).unwrap(),
            });
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    Server { url, seen }
}

fn ai(config: Config, name: &str) -> (Ai, PathBuf) {
    let dir = std::env::temp_dir().join(format!("persephone-ai-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    (Ai::new(config, &dir).unwrap(), dir)
}

fn claude(server: &Server) -> Config {
    Config {
        anthropic_api_key: Some("test-key".into()),
        anthropic_base_url: Some(server.url.clone()),
        ..Config::default()
    }
}

fn question(model: &str) -> Request {
    Request {
        model: model.into(),
        system: Some("Answer LONG, SHORT or FLAT.".into()),
        prompt: "Closes: 1, 2, 3".into(),
        max_tokens: None,
        effort: Some(Effort::Low),
    }
}

const ANSWER: &str = r#"{"content":[{"type":"thinking","thinking":""},{"type":"text","text":"LONG"}],"stop_reason":"end_turn"}"#;

#[test]
fn asks_claude_once_then_answers_from_the_recording() {
    let server = serve(vec![(200, "", ANSWER.into())]);
    let (ai, dir) = ai(claude(&server), "claude");

    let reply = ai.complete(&question("claude-opus-5-5"), true).unwrap();
    assert_eq!(
        reply,
        Reply {
            text: "LONG".into(),
            cached: false
        }
    );
    let again = ai.complete(&question("claude-opus-5-5"), true).unwrap();
    assert_eq!(
        again,
        Reply {
            text: "LONG".into(),
            cached: true
        }
    );

    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "the second answer came from the recording");
    let sent = &seen[0];
    assert_eq!(sent.line, "POST /v1/messages HTTP/1.1");
    assert_eq!(sent.header("x-api-key"), Some("test-key"));
    assert_eq!(sent.header("anthropic-version"), Some("2026-10-21"));
    assert_eq!(
        sent.header("anthropic-beta"),
        Some("server-side-fallback-2026-07-01")
    );
    let body = sent.json();
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["max_tokens"], 16_000);
    assert_eq!(body["system"], "Answer LONG, SHORT or FLAT.");
    assert_eq!(body["messages"][0]["content"], "Closes: 1, 2, 3");
    assert_eq!(body["output_config"]["effort"], "low");
    assert_eq!(body["fallbacks"], "default");
    for gone in ["temperature", "top_p", "top_k"] {
        assert!(
            body.get(gone).is_none(),
            "{gone} is rejected by this API version"
        );
    }
    assert_eq!(ai.cache().len(), 1);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn haiku_has_no_server_side_fallback() {
    let server = serve(vec![(200, "", ANSWER.into())]);
    let (ai, dir) = ai(claude(&server), "haiku");
    ai.complete(&question("claude-haiku-5-5"), true).unwrap();
    let sent = server.seen.lock().unwrap()[0].clone();
    assert!(sent.json().get("fallbacks").is_none());
    assert_eq!(sent.header("anthropic-beta"), None);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn refusals_and_errors_say_what_happened() {
    let refusal = r#"{"content":[],"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"}}"#;
    let server = serve(vec![
        (200, "", refusal.into()),
        (401, "", r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#.into()),
        (200, "", r#"{"content":[{"type":"thinking","thinking":""}],"stop_reason":"max_tokens"}"#.into()),
    ]);
    let (ai, dir) = ai(claude(&server), "errors");
    let ask = |prompt: &str| {
        ai.complete(
            &Request {
                prompt: prompt.into(),
                ..question("claude-opus-5-5")
            },
            true,
        )
    };
    assert_eq!(
        ask("a").unwrap_err(),
        "claude-opus-5-5 declined to answer (cyber)"
    );
    assert_eq!(ask("b").unwrap_err(), "HTTP 401: invalid x-api-key");
    assert!(ask("c").unwrap_err().contains("max_tokens"));
    assert_eq!(ai.cache().len(), 0, "failures aren't recorded");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn overloads_are_retried() {
    let server = serve(vec![
        (
            529,
            "retry-after: 0\r\n",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#.into(),
        ),
        (200, "", ANSWER.into()),
    ]);
    let (ai, dir) = ai(claude(&server), "retry");
    assert_eq!(
        ai.complete(&question("claude-opus-5-5"), true)
            .unwrap()
            .text,
        "LONG"
    );
    assert_eq!(server.seen.lock().unwrap().len(), 2);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn offline_budgets_and_missing_keys() {
    let (offline, dir) = ai(
        Config {
            offline: true,
            ..Config::default()
        },
        "offline",
    );
    assert!(
        offline
            .complete(&question("claude-opus-5-5"), true)
            .unwrap_err()
            .contains("offline")
    );
    let _ = fs::remove_dir_all(dir);

    let (keyless, dir) = ai(Config::default(), "keyless");
    assert!(
        keyless
            .complete(&question("claude-opus-5-5"), false)
            .unwrap_err()
            .contains("used up")
    );
    assert!(
        keyless
            .complete(&question("claude-opus-5-5"), true)
            .unwrap_err()
            .contains("ANTHROPIC_API_KEY")
    );
    assert!(
        keyless
            .complete(&question("gpt-x"), true)
            .unwrap_err()
            .contains("unknown model")
    );
    assert_eq!(keyless.max_new_calls(), DEFAULT_MAX_NEW_CALLS);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn openai_compatible_servers() {
    let reply =
        r#"{"choices":[{"message":{"role":"assistant","content":"FLAT"},"finish_reason":"stop"}]}"#;
    let server = serve(vec![(200, "", reply.into()), (200, "", reply.into())]);
    let config = Config {
        openai_api_key: Some("sk-test".into()),
        openai_base_url: Some(format!("{}/v1", server.url)),
        ollama_base_url: Some(format!("{}/v1", server.url)),
        ..Config::default()
    };
    let (ai, dir) = ai(config, "openai");
    let request = |model: &str| Request {
        max_tokens: Some(50),
        ..question(model)
    };
    assert_eq!(
        ai.complete(&request("openai/gpt-test"), true).unwrap().text,
        "FLAT"
    );
    assert_eq!(
        ai.complete(&request("ollama/llama3.1"), true).unwrap().text,
        "FLAT"
    );

    let seen = server.seen.lock().unwrap();
    assert_eq!(seen[0].line, "POST /v1/chat/completions HTTP/1.1");
    assert_eq!(seen[0].header("authorization"), Some("Bearer sk-test"));
    let body = seen[0].json();
    assert_eq!(body["model"], "gpt-test");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"], "Closes: 1, 2, 3");
    assert_eq!(body["max_tokens"], 50, "a custom server gets max_tokens");
    assert_eq!(seen[1].header("authorization"), None, "Ollama needs no key");
    assert_eq!(seen[1].json()["model"], "llama3.1");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn release_dates_are_looked_up_once_and_kept() {
    let server = serve(vec![(
        200,
        "",
        r#"{"id":"claude-opus-5-5","created_at":"2026-04-01T00:00:00Z"}"#.into(),
    )]);
    let (ai, dir) = ai(claude(&server), "releases");
    let april = date_nanos("2026-04-01").unwrap();
    assert_eq!(ai.release_date("claude-opus-5-5"), Some(april));
    assert_eq!(ai.release_date("claude-opus-5-5"), Some(april));
    assert_eq!(server.seen.lock().unwrap().len(), 1);
    assert_eq!(
        server.seen.lock().unwrap()[0].line,
        "GET /v1/models/claude-opus-5-5 HTTP/1.1"
    );
    assert_eq!(
        ai.release_date("ollama/llama3.1"),
        None,
        "local models don't say"
    );

    // Kept on disk, so offline replays still know it.
    let offline = Ai::new(
        Config {
            offline: true,
            ..Config::default()
        },
        &dir,
    )
    .unwrap();
    assert_eq!(offline.release_date("claude-opus-5-5"), Some(april));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_tampered_recording_is_ignored() {
    let dir = std::env::temp_dir().join(format!("persephone-ai-tamper-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let cache = Cache::new(&dir);
    let request = question("claude-opus-5-5");
    cache.put(&request, "LONG").unwrap();
    assert_eq!(cache.get(&request).as_deref(), Some("LONG"));
    let other = Request {
        prompt: "something else".into(),
        ..request.clone()
    };
    assert_eq!(cache.get(&other), None);

    // Same file name, different question inside (a collision): not this question's answer.
    let path = cache.path(&request);
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("Closes: 1, 2, 3", "Closes: 9");
    fs::write(&path, text).unwrap();
    assert_eq!(cache.get(&request), None);
    cache.clear().unwrap();
    assert!(cache.is_empty());
}
