//! The CLI door: the built binary's exit code is the seed's verdict.
//! The child runs in its own process with its own env — no env lock
//! needed — against a local scripted listener.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

struct Mini {
    port: u16,
    paths: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl Mini {
    fn start(script: Vec<(u16, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let paths = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let mut answers = script
            .into_iter()
            .collect::<std::collections::VecDeque<_>>();

        let srv_paths = Arc::clone(&paths);
        let srv_stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !srv_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                            .ok();
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 4096];
                        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            match stream.read(&mut chunk) {
                                Ok(0) => break,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                                Err(_) => break,
                            }
                        }
                        let head = String::from_utf8_lossy(&buf).to_string();
                        if let Some(line) = head.split("\r\n").next() {
                            if let Some(path) = line.split_whitespace().nth(1) {
                                srv_paths.lock().unwrap().push(path.to_string());
                            }
                        }
                        let (status, body) = answers.pop_front().unwrap_or((404, String::new()));
                        let resp = format!(
                            "HTTP/1.1 {} x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            status,
                            body.len(),
                            body
                        );
                        stream.write_all(resp.as_bytes()).ok();
                        stream.flush().ok();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self { port, paths, stop }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }
}

impl Drop for Mini {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn fixture(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("godon-seeder-cli-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let comp = dir.join("comp");
    std::fs::create_dir_all(&comp).unwrap();
    std::fs::write(
        comp.join("component.yaml"),
        "name: comp\ntarget: t1\nworkspace: w1\nscripts:\n  - pattern: \"*.py\"\nschedules:\n  - script: duty\n    every_seconds: 60\n",
    )
    .unwrap();
    std::fs::write(comp.join("run.py"), "print(1)\n").unwrap();
    comp
}

fn run_seeder(dir: &std::path::Path, url: &str) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_godon-seeder"))
        .arg(dir)
        .arg("--max-retries")
        .arg("0")
        .arg("--retry-delay")
        .arg("0")
        .env_clear()
        .env("WINDMILL_BASE_URL", url)
        .env("WINDMILL_TOKEN", "cli-token")
        .output()
        .unwrap()
}

#[test]
fn the_binarys_exit_code_is_the_seed_verdict() {
    // the clean seed: tags planted, everything created — exit 0
    let mini = Mini::start(vec![
        (201, String::new()),  // lane tags
        (200, "false".into()), // workspace w1 absent
        (201, String::new()),  // create workspace
        (201, String::new()),  // create folder
        (200, "false".into()), // script absent
        (201, String::new()),  // create script
        (200, "true".into()),  // the scheduled script is planted
        (200, "false".into()), // the schedule is absent
        (201, String::new()),  // create schedule
    ]);
    let comp = fixture("happy");
    let out = run_seeder(&comp, &mini.url());
    assert!(
        out.status.success(),
        "a clean seed exits zero:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let paths = mini.paths();
    assert!(paths.iter().any(|p| p.contains("/custom_tags")));
    assert!(paths.iter().any(|p| p.contains("/workspaces/create")));
    assert!(paths.iter().any(|p| p.contains("/schedules/create")));

    // the failed seed: the script create refuses — exit 1
    let mini = Mini::start(vec![
        (201, String::new()),  // lane tags
        (200, "false".into()), // workspace absent
        (201, String::new()),  // create workspace
        (201, String::new()),  // create folder
        (200, "false".into()), // script absent
        (500, String::new()),  // create refused
    ]);
    let comp = fixture("failing");
    let out = run_seeder(&comp, &mini.url());
    assert!(
        !out.status.success(),
        "failures on the wire must fail the run"
    );
}
