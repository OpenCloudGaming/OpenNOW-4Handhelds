//! Connection to the `opennow-core` process: newline-delimited JSON requests on
//! its standard input, responses and events on its standard output. A reader
//! thread forwards parsed lines; the UI thread drains them once per frame.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

pub const PROTOCOL_VERSION: i64 = 5;
const MAXIMUM_LINE_BYTES: usize = 1024 * 1024;
const FIRST_BUSY_RETRY: Duration = Duration::from_millis(100);
const LAST_BUSY_RETRY: Duration = Duration::from_secs(1);

fn redact_log_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let normalized = key.to_ascii_lowercase().replace('_', "").replace('-', "");
                    let sensitive = [
                        "token",
                        "password",
                        "secret",
                        "authorization",
                        "cookie",
                        "devicecode",
                        "usercode",
                        "sessionid",
                        "verificationuri",
                        "challenge",
                    ]
                    .iter()
                    .any(|needle| normalized.contains(needle));
                    let url = normalized.ends_with("url");
                    let safe_value = if sensitive {
                        Value::String("[REDACTED]".to_owned())
                    } else if url {
                        value.as_str().map_or_else(
                            || redact_log_value(value),
                            |text| {
                                Value::String(
                                    text.split_once('?')
                                        .map_or(text, |(base, _)| base)
                                        .to_owned(),
                                )
                            },
                        )
                    } else {
                        redact_log_value(value)
                    };
                    (key.clone(), safe_value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_log_value).collect()),
        _ => value.clone(),
    }
}

const BRIEF_KEYS: &[&str] = &[
    "appId",
    "variantId",
    "title",
    "status",
    "phase",
    "zone",
    "serverIp",
    "transportMode",
    "resolution",
    "fps",
    "codec",
    "maxBitrateMbps",
    "key",
    "code",
    "message",
    "queuePosition",
    "hasNextPage",
    "limit",
    "region",
    "outcome",
    "value",
];

fn log_brief(kind: &str, value: &Value) {
    let safe = redact_log_value(value);
    let mut notes = Vec::new();
    brief_fields(&safe, 0, &mut notes);
    if notes.is_empty() {
        eprintln!("core: {kind}");
    } else {
        eprintln!("core: {kind} {}", notes.join(" "));
    }
}

fn brief_fields(value: &Value, depth: u8, notes: &mut Vec<String>) {
    let Value::Object(fields) = value else {
        return;
    };
    if depth > 3 || notes.len() >= 12 {
        return;
    }
    for (key, child) in fields {
        if notes.len() >= 12 || skipped_key(key) {
            continue;
        }
        match child {
            Value::String(text) if BRIEF_KEYS.contains(&key.as_str()) => {
                notes.push(format!("{key}={}", clip(text)));
            }
            Value::Number(number) if BRIEF_KEYS.contains(&key.as_str()) => {
                notes.push(format!("{key}={number}"));
            }
            Value::Bool(flag) if BRIEF_KEYS.contains(&key.as_str()) => {
                notes.push(format!("{key}={flag}"));
            }
            _ => {}
        }
    }
    for (key, child) in fields {
        if notes.len() >= 12 || skipped_key(key) {
            continue;
        }
        if let Value::Object(_) = child
            && depth < 3
            && key != "runtimeCapabilities"
            && key != "iceServers"
        {
            brief_fields(child, depth + 1, notes);
        }
    }
    for (key, child) in fields {
        if notes.len() >= 12 || skipped_key(key) {
            continue;
        }
        if let Value::Array(items) = child {
            notes.push(format!("{key}[{}]", items.len()));
        }
    }
}

fn skipped_key(key: &str) -> bool {
    key == "type" || key == "id" || key == "method" || key == "ok"
}

fn clip(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= 80 {
        return text;
    }
    let mut end = text.chars().take(77).collect::<String>();
    end.push_str("...");
    end
}

fn log_response(method: &str, value: &Value) {
    let id = value["id"].as_str().unwrap_or("?");
    let safe = redact_log_value(value);
    if safe["ok"].as_bool() == Some(true) {
        let mut notes = Vec::new();
        brief_fields(&safe["result"], 0, &mut notes);
        let detail = if notes.is_empty() {
            String::new()
        } else {
            format!(" {}", notes.join(" "))
        };
        eprintln!("core: response id={id} method={method} outcome=ok{detail}");
    } else {
        let code = safe["error"]["code"].as_str().unwrap_or("error");
        let message = safe["error"]["message"].as_str().map(clip).unwrap_or_default();
        if message.is_empty() {
            eprintln!("core: response id={id} method={method} outcome=error code={code}");
        } else {
            eprintln!(
                "core: response id={id} method={method} outcome=error code={code} message={message}"
            );
        }
    }
}

pub enum Message {
    Response { id: u64, result: Result<Value, CoreError> },
    Event { name: String, payload: Value },
}

#[derive(Clone, Debug)]
pub struct CoreError {
    pub code: String,
    pub message: String,
}

enum Line {
    Json(Value),
    Closed(String),
}

struct Pending {
    method: String,
    params: Value,
    deadline: Instant,
    retry_at: Option<Instant>,
    retry_delay: Duration,
}

pub struct Core {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Line>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    closed: Option<String>,
}

/// The core binary ships next to the front end; `OPENNOW_CORE` overrides it.
pub fn default_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("OPENNOW_CORE") {
        return PathBuf::from(path);
    }
    let name = if cfg!(windows) { "opennow-core.exe" } else { "opennow-core" };
    executable_dir().join(name)
}

pub fn executable_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

impl Core {
    /// Starts the core with its own data directory so it never contends for
    /// the lock held by a desktop OpenNOW on the same machine.
    pub fn spawn(binary: &Path, data_dir: &Path) -> Result<Self, String> {
        let mut child = Command::new(binary)
            .arg("--data-dir")
            .arg(data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("Could not start {}: {error}", binary.display()))?;
        let stdout = child.stdout.take().ok_or("core stdout unavailable")?;
        let (sender, lines) = channel();
        std::thread::Builder::new()
            .name("opennow-core-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) => {
                            let _ = sender.send(Line::Closed("The OpenNOW core exited".into()));
                            break;
                        }
                        Ok(_) if line.len() > MAXIMUM_LINE_BYTES => {
                            let _ = sender.send(Line::Closed("The core sent an oversized message".into()));
                            break;
                        }
                        Ok(_) => match serde_json::from_str(line.trim_end()) {
                            Ok(value) => {
                                if sender.send(Line::Json(value)).is_err() {
                                    break;
                                }
                            }
                            Err(_) => {
                                let _ = sender.send(Line::Closed("The core sent malformed data".into()));
                                break;
                            }
                        },
                        Err(error) => {
                            let _ = sender.send(Line::Closed(format!("Core connection failed: {error}")));
                            break;
                        }
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        let stdin = child.stdin.take();
        Ok(Self { child, stdin, lines, next_id: 0, pending: HashMap::new(), closed: None })
    }

    pub fn hello(&mut self) -> u64 {
        self.request(
            "core.hello",
            json!({"protocolVersion": PROTOCOL_VERSION, "shell": "handheld", "shellVersion": env!("CARGO_PKG_VERSION")}),
            Duration::from_secs(5),
        )
    }

    /// Sends a request and returns its id. Failures to write surface as an
    /// error response on the next `poll`.
    pub fn request(&mut self, method: &str, params: Value, timeout: Duration) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.pending.insert(
            id,
            Pending {
                method: method.to_owned(),
                params,
                deadline: Instant::now() + timeout,
                retry_at: Some(Instant::now()),
                retry_delay: FIRST_BUSY_RETRY,
            },
        );
        self.send_due();
        id
    }

    pub fn cancel(&mut self, id: u64) {
        if self.pending.remove(&id).is_some() {
            self.write(&json!({"type": "cancel", "id": id.to_string()}));
        }
    }

    /// The reason the connection ended, once it has.
    pub fn closed(&self) -> Option<&str> {
        self.closed.as_deref()
    }

    pub fn poll(&mut self) -> Vec<Message> {
        let mut messages = Vec::new();
        loop {
            match self.lines.try_recv() {
                Ok(Line::Json(value)) => self.receive(value, &mut messages),
                Ok(Line::Closed(reason)) => {
                    self.close(reason);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.close("The core connection closed".into());
                    break;
                }
            }
        }
        let now = Instant::now();
        let expired: Vec<u64> = self.pending.iter().filter(|(_, request)| now >= request.deadline).map(|(id, _)| *id).collect();
        for id in expired {
            let method = self.pending[&id].method.clone();
            eprintln!("core: response id={id} method={method} outcome=timeout");
            self.cancel(id);
            messages.push(Message::Response {
                id,
                result: Err(CoreError { code: "timeout".into(), message: format!("{method} timed out") }),
            });
        }
        if let Some(reason) = self.closed.clone() {
            for (id, _) in self.pending.drain() {
                messages.push(Message::Response {
                    id,
                    result: Err(CoreError { code: "core_unavailable".into(), message: reason.clone() }),
                });
            }
        }
        self.send_due();
        messages
    }

    fn receive(&mut self, value: Value, messages: &mut Vec<Message>) {
        match value["type"].as_str() {
            Some("response") => {
                let Some(id) = value["id"].as_str().and_then(|id| id.parse::<u64>().ok()) else {
                    return;
                };
                let Some(request) = self.pending.get_mut(&id) else {
                    return;
                };
                let method = request.method.clone();
                log_response(&method, &value);
                if value["ok"].as_bool() == Some(true) {
                    self.pending.remove(&id);
                    // A created cloud seat is deleted unless the shell accepts it promptly.
                    if method == "session.create" {
                        self.write(&json!({"type": "ack", "id": id.to_string()}));
                    }
                    messages.push(Message::Response { id, result: Ok(value["result"].clone()) });
                    return;
                }
                let error = CoreError {
                    code: value["error"]["code"].as_str().unwrap_or("error").to_owned(),
                    message: value["error"]["message"].as_str().unwrap_or("The core reported an error").to_owned(),
                };
                if error.code == "busy" {
                    request.retry_at = Some(Instant::now() + request.retry_delay);
                    request.retry_delay = (request.retry_delay * 2).min(LAST_BUSY_RETRY);
                    return;
                }
                self.pending.remove(&id);
                messages.push(Message::Response { id, result: Err(error) });
            }
            Some("event") => messages.push(Message::Event {
                name: value["name"].as_str().unwrap_or_default().to_owned(),
                payload: value["payload"].clone(),
            }),
            _ => {}
        }
    }

    fn send_due(&mut self) {
        if self.closed.is_some() {
            return;
        }
        let now = Instant::now();
        let due: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, request)| request.retry_at.is_some_and(|at| now >= at))
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            let request = self.pending.get_mut(&id).expect("due request is pending");
            request.retry_at = None;
            let line = json!({"type": "request", "id": id.to_string(), "method": request.method, "params": request.params});
            log_brief(
                &format!("request id={id} method={}", request.method),
                &line,
            );
            self.write(&line);
        }
    }

    fn write(&mut self, value: &Value) {
        let Some(stdin) = self.stdin.as_mut() else {
            return;
        };
        let mut line = value.to_string();
        line.push('\n');
        if let Err(error) = stdin.write_all(line.as_bytes()).and_then(|()| stdin.flush()) {
            self.close(format!("Could not reach the core: {error}"));
        }
    }

    fn close(&mut self, reason: String) {
        if self.closed.is_none() {
            eprintln!("core: {reason}");
            self.closed = Some(reason);
        }
        self.stdin = None;
    }
}

impl Drop for Core {
    /// Closing standard input ends the core's request loop; it is killed only
    /// if it does not exit promptly.
    fn drop(&mut self) {
        self.stdin = None;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
