//! Strategies that run in another process (Python with its ML stack, an agent framework,
//! anything that can open a socket), trading bar by bar in lockstep with Persephone.
//!
//! A `NAME.external` file in the plugins folder holds the address the process listens on:
//! `tcp://127.0.0.1:7878` (or just `127.0.0.1:7878`), or `unix:///path/to.sock`. Lines
//! starting with `#` are comments. The strategy then shows up with the plugins.
//!
//! The protocol is JSON, one object per line each way. Persephone connects, sends a request,
//! and waits for its answer before sending the next, so a backtest is as deterministic as the
//! process is:
//!
//! | Persephone sends | The strategy answers |
//! |---|---|
//! | `{"type":"describe"}` | `{"name":"…","description":"…","params":[{"name":"Fast","default":10,"min":1,"max":500,"integer":true}]}` |
//! | `{"type":"start","params":[10.0,30.0]}` | `{}` |
//! | `{"type":"bar","ts":…,"open":…,"high":…,"low":…,"close":…,"volume":…}` | `{"target":1}`: long; `-1` short, `0` flat, `null` keep |
//! | `{"type":"end"}` | `{"notes":[{"text":"…","warning":false}]}`, or `{}` |
//!
//! Describing gets a connection of its own; each backtest or live session opens another for
//! `start`, its bars, and `end` (a live session just closes it when it stops). Any answer may
//! be `{"error":"…"}` instead, which stops the run with that message. `ts` is the bar's close
//! time in nanoseconds since 1970, UTC; `params` leave out the position size, which Persephone
//! handles.

use std::{
    fmt,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    time::Duration,
};

use persephone_core::{
    Candle,
    indicator::Param,
    signal::{Note, Signal},
};
use serde_json::{Value, json};

/// How long to wait for the process to accept a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// How long one answer may take: a model call per bar can be slow, but a stuck process
/// shouldn't hang a backtest forever.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);

/// Where an external strategy listens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    Tcp(String),
    #[cfg(unix)]
    Unix(PathBuf),
}

impl Address {
    /// Reads an address from a `.external` file's text.
    ///
    /// # Errors
    ///
    /// Returns a message if there's no address, or it isn't TCP or a Unix socket.
    pub fn parse(text: &str) -> Result<Self, String> {
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .ok_or("holds no address (e.g. tcp://127.0.0.1:7878)")?;
        if let Some(path) = line.strip_prefix("unix://") {
            #[cfg(unix)]
            return Ok(Self::Unix(PathBuf::from(path)));
            #[cfg(not(unix))]
            return Err(format!("Unix sockets aren't available here: {path}"));
        }
        let host = line.strip_prefix("tcp://").unwrap_or(line);
        if host.contains("://") || !host.contains(':') {
            return Err(format!("`{line}` isn't tcp://HOST:PORT or unix://PATH"));
        }
        Ok(Self::Tcp(host.to_owned()))
    }

    fn connect(&self) -> Result<Connection, String> {
        let fail = |e: std::io::Error| format!("couldn't reach {self}: {e}");
        let (reader, writer): (Box<dyn Read + Send>, Box<dyn Write + Send>) = match self {
            Self::Tcp(host) => {
                let addr = host
                    .to_socket_addrs()
                    .map_err(fail)?
                    .next()
                    .ok_or_else(|| format!("{self} doesn't resolve"))?;
                let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(fail)?;
                stream.set_nodelay(true).map_err(fail)?;
                stream
                    .set_read_timeout(Some(ANSWER_TIMEOUT))
                    .map_err(fail)?;
                (
                    Box::new(stream.try_clone().map_err(fail)?),
                    Box::new(stream),
                )
            }
            #[cfg(unix)]
            Self::Unix(path) => {
                let stream = std::os::unix::net::UnixStream::connect(path).map_err(fail)?;
                stream
                    .set_read_timeout(Some(ANSWER_TIMEOUT))
                    .map_err(fail)?;
                (
                    Box::new(stream.try_clone().map_err(fail)?),
                    Box::new(stream),
                )
            }
        };
        Ok(Connection {
            reader: BufReader::new(reader),
            writer,
        })
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(host) => write!(f, "tcp://{host}"),
            #[cfg(unix)]
            Self::Unix(path) => write!(f, "unix://{}", path.display()),
        }
    }
}

struct Connection {
    reader: BufReader<Box<dyn Read + Send>>,
    writer: Box<dyn Write + Send>,
}

impl Connection {
    /// Sends one request and reads its answer.
    fn ask(&mut self, request: &Value) -> Result<Value, String> {
        let mut line = request.to_string();
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .and_then(|()| self.writer.flush())
            .map_err(|e| format!("couldn't send to the strategy: {e}"))?;
        let mut answer = String::new();
        let read = self
            .reader
            .read_line(&mut answer)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                    format!(
                        "the strategy didn't answer within {}s",
                        ANSWER_TIMEOUT.as_secs()
                    )
                }
                _ => format!("couldn't hear from the strategy: {e}"),
            })?;
        if read == 0 {
            return Err("the strategy closed the connection".into());
        }
        let answer: Value = serde_json::from_str(&answer)
            .map_err(|e| format!("the strategy's answer isn't JSON ({e}): {}", answer.trim()))?;
        if let Some(error) = answer.get("error") {
            let message = error
                .as_str()
                .map_or_else(|| error.to_string(), str::to_owned);
            return Err(format!("the strategy failed: {message}"));
        }
        Ok(answer)
    }
}

/// What an external strategy says about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Description {
    pub name: String,
    pub description: String,
    pub params: Vec<Param>,
}

/// Asks the strategy at `address` to describe itself.
///
/// # Errors
///
/// Returns a message if it can't be reached or answers something else.
pub fn describe(address: &Address) -> Result<Description, String> {
    let answer = address.connect()?.ask(&json!({"type": "describe"}))?;
    let name = answer
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.trim().is_empty())
        .ok_or("the description has no name")?
        .to_owned();
    let description = answer
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let params = match answer.get("params") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(params)) => params.iter().map(param).collect::<Result<_, _>>()?,
        Some(_) => return Err("`params` should be a list".into()),
    };
    Ok(Description {
        name,
        description,
        params,
    })
}

fn param(value: &Value) -> Result<Param, String> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .ok_or("a parameter has no name")?
        .to_owned();
    let number = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("parameter `{name}` has no {key}"))
    };
    let param = Param {
        default: number("default")?,
        min: number("min")?,
        max: number("max")?,
        integer: value
            .get("integer")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        name: name.clone().into(),
    };
    param
        .check(param.default)
        .map_err(|e| format!("its default doesn't fit: {e}"))?;
    Ok(param)
}

/// Connects to the strategy at `address` for one run and starts it with `params`.
///
/// # Errors
///
/// Returns a message if it can't be reached or won't start.
pub fn start(address: &Address, params: &[f64]) -> Result<Box<dyn Signal>, String> {
    let mut connection = address.connect()?;
    connection.ask(&json!({"type": "start", "params": params}))?;
    Ok(Box::new(ExternalSignal { connection }))
}

/// One run of an external strategy, over its own connection.
struct ExternalSignal {
    connection: Connection,
}

impl Signal for ExternalSignal {
    fn on_bar(&mut self, bar: &Candle) -> Result<Option<i8>, String> {
        let answer = self.connection.ask(&json!({
            "type": "bar",
            "ts": bar.ts,
            "open": bar.open,
            "high": bar.high,
            "low": bar.low,
            "close": bar.close,
            "volume": bar.volume,
        }))?;
        match answer.get("target") {
            None | Some(Value::Null) => Ok(None),
            Some(target) => match target.as_i64() {
                Some(t @ -1..=1) => Ok(Some(t as i8)),
                _ => Err(format!(
                    "the strategy wanted position {target}; use 1, -1, 0 or null"
                )),
            },
        }
    }

    fn notes(&mut self) -> Vec<Note> {
        let Ok(answer) = self.connection.ask(&json!({"type": "end"})) else {
            return Vec::new();
        };
        answer
            .get("notes")
            .and_then(Value::as_array)
            .map(|notes| {
                notes
                    .iter()
                    .filter_map(|note| {
                        Some(Note {
                            text: note.get("text")?.as_str()?.to_owned(),
                            warning: note
                                .get("warning")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_parse() {
        assert_eq!(
            Address::parse("# my model\n\ntcp://127.0.0.1:7878\n"),
            Ok(Address::Tcp("127.0.0.1:7878".into()))
        );
        assert_eq!(
            Address::parse("localhost:9000"),
            Ok(Address::Tcp("localhost:9000".into()))
        );
        #[cfg(unix)]
        assert_eq!(
            Address::parse("unix:///tmp/s.sock"),
            Ok(Address::Unix("/tmp/s.sock".into()))
        );
        assert!(Address::parse("# nothing").is_err());
        assert!(Address::parse("http://x:1").is_err());
        assert!(Address::parse("no-port").is_err());
        assert_eq!(Address::Tcp("h:1".into()).to_string(), "tcp://h:1");
    }

    #[test]
    fn params_need_a_name_and_a_default_in_range() {
        let ok =
            param(&json!({"name": "Fast", "default": 10, "min": 1, "max": 50, "integer": true}));
        assert_eq!(ok.unwrap().max, 50.0);
        assert!(param(&json!({"default": 1})).is_err());
        assert!(
            param(&json!({"name": "X", "default": 1}))
                .unwrap_err()
                .contains("min")
        );
        assert!(param(&json!({"name": "X", "default": 99, "min": 1, "max": 50})).is_err());
    }
}
