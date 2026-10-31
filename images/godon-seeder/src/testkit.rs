//! The seeder's test kit: one local listener speaking the wire's
//! contracts, a log-capture oracle, and the env-window lock. Shared
//! by the test modules of this crate (component, auth); the CLI
//! integration test lives in its own process and keeps its own
//! minimal listener.
//!
//! Script entries are one (status, body) pair per expected request,
//! answered in arrival order. Two special shapes: status 0 drops the
//! connection after reading (the client sees a send error), and
//! `start_bytes` plants raw bodies — including invalid UTF-8 — for
//! the response-parsing fault paths. When the script runs dry the
//! listener answers 404, so a mis-counted test fails loudly on its
//! asserts instead of hanging.

use crate::component::WindmillDeployer;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Env is process-global and cargo test runs test fns in parallel:
/// every test that sets or reads the windmill env vars holds this
/// lock for its whole window.
pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

// ── the log-capture oracle ──
// Some branches speak only through their log lines (create_folder's
// status arms, seed_workspace's final verdict); the lines ARE the
// behavior there, so the tests read them back.

static LOG_LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LOG_INSTALL: OnceLock<()> = OnceLock::new();

struct Capture;

impl log::Log for Capture {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        if record.target().starts_with("godon_seeder") {
            LOG_LINES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("{}|{}", record.level(), record.args()));
        }
    }

    fn flush(&self) {}
}

/// Install the capturing logger (once per process) and read at
/// Debug: some pinned lines live below Info.
pub(crate) fn capture_logs() {
    LOG_INSTALL.get_or_init(|| {
        let _ = log::set_boxed_logger(Box::new(Capture));
        log::set_max_level(log::LevelFilter::Debug);
    });
}

/// Take the captured lines; assert on distinctive fragments only.
pub(crate) fn drain_logs() -> Vec<String> {
    let mut lines = LOG_LINES.lock().unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut *lines)
}

// ── the fake windmill ──

pub(crate) struct Recorded {
    pub method: String,
    pub path: String,
    pub body: String,
    pub bearer: Option<String>,
}

pub(crate) struct FakeWindmill {
    port: u16,
    log: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn write_response(stream: &mut TcpStream, status: u16, body: &[u8]) {
    if status == 0 {
        // The reset door: read, record, then drop the connection —
        // the client's send() reports the failure.
        return;
    }
    if status == 1 {
        // The truncation door: promise more body than is sent, then
        // drop — the client's response READ fails mid-message.
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len() + 16
        );
        stream.write_all(head.as_bytes()).ok();
        stream.write_all(body).ok();
        stream.flush().ok();
        return;
    }
    let reason = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        reason,
        body.len()
    );
    stream.write_all(head.as_bytes()).ok();
    stream.write_all(body).ok();
    stream.flush().ok();
}

fn read_request(stream: &mut TcpStream) -> Option<Recorded> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 1_048_576 {
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?.to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut content_length = 0usize;
    let mut bearer = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        } else if lower.strip_prefix("authorization:").is_some() {
            bearer = line[15..]
                .trim()
                .strip_prefix("Bearer ")
                .map(str::to_string);
        }
    }
    let header_end = head.find("\r\n\r\n").map(|i| i + 4).unwrap_or(buf.len());
    let mut body = buf[header_end.min(buf.len())..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    if body.len() > content_length {
        body.truncate(content_length);
    }
    Some(Recorded {
        method,
        path,
        body: String::from_utf8_lossy(&body).to_string(),
        bearer,
    })
}

impl FakeWindmill {
    pub(crate) fn start(script: Vec<(u16, String)>) -> Self {
        Self::start_bytes(
            script
                .into_iter()
                .map(|(s, b)| (s, b.into_bytes()))
                .collect(),
        )
    }

    /// Raw bodies: invalid UTF-8 reaches the client's response
    /// parser exactly as a real fault would.
    pub(crate) fn start_bytes(script: Vec<(u16, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let queue = Arc::new(Mutex::new(script.into_iter().collect::<VecDeque<_>>()));

        let srv_log = Arc::clone(&log);
        let srv_stop = Arc::clone(&stop);
        let srv_queue = Arc::clone(&queue);
        let handle = std::thread::spawn(move || {
            while !srv_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                        if let Some(recorded) = read_request(&mut stream) {
                            log_push(&srv_log, recorded);
                            let (status, body) = srv_queue
                                .lock()
                                .unwrap()
                                .pop_front()
                                .unwrap_or((404, Vec::new()));
                            write_response(&mut stream, status, &body);
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            port,
            log,
            stop,
            thread: Some(handle),
        }
    }

    pub(crate) fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub(crate) fn deployer(&self) -> WindmillDeployer {
        WindmillDeployer::with_base_url(&self.url(), 0, 0)
    }

    pub(crate) fn recorded(&self) -> Vec<(String, String)> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|r| (r.method.clone(), r.path.clone()))
            .collect()
    }

    pub(crate) fn bodies(&self, fragment: &str) -> Vec<serde_json::Value> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path.contains(fragment))
            .map(|r| serde_json::from_str(&r.body).unwrap_or(serde_json::Value::Null))
            .collect()
    }

    pub(crate) fn requests(&self) -> usize {
        self.log.lock().unwrap().len()
    }

    pub(crate) fn bearers(&self) -> Vec<Option<String>> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.bearer.clone())
            .collect()
    }
}

fn log_push(log: &Mutex<Vec<Recorded>>, recorded: Recorded) {
    log.lock().unwrap().push(recorded);
}

impl Drop for FakeWindmill {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn ok(body: &str) -> (u16, String) {
    (200, body.to_string())
}

pub(crate) fn created() -> (u16, String) {
    (201, String::new())
}

/// The reset door: the listener answers this request by dropping
/// the connection — the client's send fails as if the server died.
pub(crate) fn reset() -> (u16, String) {
    (0, String::new())
}

/// The truncation door (for `start_bytes`): a body that stops short
/// of its Content-Length — the client's response read fails.
pub(crate) fn truncated(body: &str) -> (u16, Vec<u8>) {
    (1, body.as_bytes().to_vec())
}
