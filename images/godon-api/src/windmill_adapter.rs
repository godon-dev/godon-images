use anyhow::{Context, Result};
use log::info;
use reqwest::blocking::Client;
use serde_json::json;
use std::env;
use wmill::Windmill;

use crate::types::{Credential, Systemtender, SystemtenderSummary, Target};

fn login_to_windmill(base_url: &str, email: &str, password: &str) -> Result<String> {
    let client = Client::new();
    let url = format!("{}/api/auth/login", base_url);

    info!("Logging into Windmill at: {}", url);

    let response = client
        .post(&url)
        .json(&json!({
            "email": email,
            "password": password
        }))
        .send()
        .context("Failed to send login request to Windmill")?;

    let token = response.text().context("Failed to read login response")?;
    let token = token.trim_matches('"').to_string();

    if token.is_empty() || token == "null" {
        anyhow::bail!("Windmill login returned empty token");
    }

    info!("Successfully authenticated with Windmill");
    Ok(token)
}

pub struct WindmillClient {
    client: Windmill,
    folder: String,
}

impl WindmillClient {
    pub fn new() -> Result<Self> {
        let base_url = env::var("WINDMILL_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:8000/api".to_string());
        let base_url = base_url.trim_end_matches("/api").to_string();

        if env::var("WM_TOKEN").is_err() {
            let token = if let Ok(token) = env::var("WINDMILL_TOKEN") {
                token
            } else {
                let email =
                    env::var("WINDMILL_EMAIL").unwrap_or_else(|_| "admin@windmill.dev".to_string());
                let password =
                    env::var("WINDMILL_PASSWORD").unwrap_or_else(|_| "changeme".to_string());

                login_to_windmill(&base_url, &email, &password)?
            };
            env::set_var("WM_TOKEN", &token);
        }

        if env::var("WM_WORKSPACE").is_err() {
            if let Ok(workspace) = env::var("WINDMILL_WORKSPACE") {
                env::set_var("WM_WORKSPACE", &workspace);
            }
        }

        if env::var("BASE_INTERNAL_URL").is_err() {
            env::set_var("BASE_INTERNAL_URL", &base_url);
        }

        let folder = env::var("WINDMILL_FOLDER").unwrap_or_else(|_| "controller".to_string());

        let client =
            Windmill::default().context("Failed to initialize Windmill client from environment")?;

        Ok(Self { client, folder })
    }

    fn script_path(&self, name: &str) -> String {
        format!("f/{}/{}", self.folder, name)
    }

    fn run_script(&self, script_name: &str, args: serde_json::Value) -> Result<serde_json::Value> {
        self.run_script_with_timeout(script_name, args, Some(60))
    }

    /// Same synchronous run, with the caller's timeout policy. Some(secs)
    /// makes the wmill SDK cancel the job when it expires; None defers to
    /// the script's own configured timeout (component.yaml `timeout`, the
    /// single source of truth).
    fn run_script_with_timeout(
        &self,
        script_name: &str,
        args: serde_json::Value,
        timeout_secs: Option<u64>,
    ) -> Result<serde_json::Value> {
        let script_path = self.script_path(script_name);
        let result = self
            .client
            .run_script_sync(&script_path, false, args, None, timeout_secs, true, false)
            .context(format!("Failed to run Windmill script: {}", script_path))?;

        if let Some(result_str) = result.get("result").and_then(|r| r.as_str()) {
            if result_str == "FAILURE" {
                let error_msg = result
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("Unknown error");
                anyhow::bail!("Windmill script failed: {}", error_msg);
            }
        }

        Ok(result)
    }

    fn unwrap_data(response: serde_json::Value) -> serde_json::Value {
        response.get("data").cloned().unwrap_or(response)
    }

    pub fn list_systemtenders(&self) -> Result<Vec<SystemtenderSummary>> {
        let response = self.run_script("systemtenders_get", json!({}))?;
        let data = Self::unwrap_data(response);

        if data.is_array() {
            let systemtenders: Vec<SystemtenderSummary> =
                serde_json::from_value(data).context("Failed to parse systemtenders list")?;
            Ok(systemtenders)
        } else {
            Ok(Vec::new())
        }
    }

    /// The create contract's fast half (designs/2026-10-10): the
    /// controller plants the row in `creating`, dispatches the
    /// executor and returns at once — the seconds-long archive work
    /// never rides this connection. `duplicate: true` marks a name
    /// clash (the API answers 409 + the existing row).
    pub fn create_systemtender(
        &self,
        systemtender_config: serde_json::Value,
    ) -> Result<(SystemtenderSummary, bool)> {
        let args = json!({ "request_data": systemtender_config });
        let response = self.run_script("systemtender_create", args)?;
        let duplicate = response
            .get("duplicate")
            .and_then(|d| d.as_bool())
            .unwrap_or(false);
        let data = Self::unwrap_data(response);

        let summary: SystemtenderSummary =
            serde_json::from_value(data).context("Failed to parse created systemtender")?;
        Ok((summary, duplicate))
    }

    pub fn get_systemtender(&self, systemtender_id: &str) -> Result<Systemtender> {
        let args = json!({ "request_data": { "systemtender_id": systemtender_id } });
        let response = self.run_script("systemtender_get", args)?;
        let data = Self::unwrap_data(response);

        let systemtender: Systemtender =
            serde_json::from_value(data).context("Failed to parse systemtender")?;

        if systemtender.id.is_empty() {
            anyhow::bail!("Invalid systemtender response: missing id field");
        }

        Ok(systemtender)
    }

    /// DELETE is a marker, not a wait (designs/2026-10-03): enqueue the
    /// deletion executor and return its windmill job id. The client
    /// polls GET until 404. Inside the executor: one cancel (windmill
    /// enforces SIGTERM(5s)->SIGKILL, ~8s bounded), a 30s quiesce
    /// bound, the archive drop only after confirmed death, and the
    /// registry row removed last.
    pub fn enqueue_systemtender_delete(
        &self,
        systemtender_id: &str,
        force: bool,
    ) -> Result<uuid::Uuid> {
        let mut request_data = json!({ "systemtender_id": systemtender_id });
        if force {
            request_data["force"] = json!(force);
        }

        let args = json!({ "request_data": request_data });
        let script_path = self.script_path("systemtender_delete");
        self.client
            .run_script_async(&script_path, false, args, None)
            .context(format!(
                "Failed to enqueue Windmill script: {}",
                script_path
            ))
    }

    pub fn stop_systemtender(&self, systemtender_id: &str) -> Result<serde_json::Value> {
        let args = json!({ "request_data": { "systemtender_id": systemtender_id } });
        let response = self.run_script("systemtender_stop", args)?;
        Ok(Self::unwrap_data(response))
    }

    pub fn update_systemtender(
        &self,
        systemtender_id: &str,
        config: serde_json::Value,
        force: bool,
    ) -> Result<serde_json::Value> {
        let mut request_data = json!({ "systemtender_id": systemtender_id, "config": config });
        if force {
            request_data["force"] = json!(force);
        }
        let args = json!({ "request_data": request_data });
        let response = self.run_script("systemtender_update", args)?;
        Ok(Self::unwrap_data(response))
    }

    pub fn start_systemtender(&self, systemtender_id: &str) -> Result<serde_json::Value> {
        let args = json!({ "request_data": { "systemtender_id": systemtender_id } });
        let response = self.run_script("systemtender_start", args)?;
        Ok(Self::unwrap_data(response))
    }

    pub fn list_credentials(&self) -> Result<Vec<Credential>> {
        let response = self.run_script("credentials_get", json!({}))?;
        let data = Self::unwrap_data(response);

        if data.is_array() {
            let credentials: Vec<Credential> =
                serde_json::from_value(data).context("Failed to parse credentials list")?;
            Ok(credentials)
        } else {
            Ok(Vec::new())
        }
    }

    pub fn create_credential(&self, credential_data: serde_json::Value) -> Result<Credential> {
        let name = credential_data
            .get("name")
            .and_then(|n| n.as_str())
            .context("Missing name field")?;

        let content = credential_data
            .get("content")
            .and_then(|c| c.as_str())
            .context("Missing content field")?;

        let windmill_variable_path = format!("f/vars/{}", name);

        self.client
            .set_variable(content.to_string(), &windmill_variable_path, true)
            .context("Failed to create Windmill variable")?;

        let catalog_data = json!({
            "name": name,
            "credentialType": credential_data.get("credentialType").and_then(|t| t.as_str()).unwrap_or(""),
            "description": credential_data.get("description").and_then(|d| d.as_str()).unwrap_or(""),
            "windmillVariable": windmill_variable_path
        });

        let args = json!({ "request_data": catalog_data });
        let response = self.run_script("credential_create", args)?;
        let data = Self::unwrap_data(response);

        serde_json::from_value(data).context("Failed to parse created credential")
    }

    pub fn get_credential(&self, credential_id: &str) -> Result<Credential> {
        let args = json!({ "request_data": { "credentialId": credential_id } });
        let response = self.run_script("credential_get", args)?;
        let data = Self::unwrap_data(response);

        let credential: Credential =
            serde_json::from_value(data).context("Failed to parse credential")?;

        let content = self
            .client
            .get_variable_raw(&credential.windmill_variable)
            .context("Failed to get credential content from Windmill variable")?;

        Ok(Credential {
            content: Some(content),
            ..credential
        })
    }

    pub fn delete_credential(&self, credential_id: &str) -> Result<()> {
        let args_get = json!({ "request_data": { "credentialId": credential_id } });
        let response = self.run_script("credential_get", args_get)?;
        let data = Self::unwrap_data(response);

        let credential: Credential =
            serde_json::from_value(data).context("Failed to parse credential for deletion")?;

        let _ = self
            .client
            .set_variable(String::new(), &credential.windmill_variable, false);

        let args = json!({ "request_data": { "credentialId": credential_id } });
        self.run_script("credential_delete", args)?;
        Ok(())
    }

    pub fn list_targets(&self) -> Result<Vec<Target>> {
        let response = self.run_script("targets_get", json!({}))?;
        let data = Self::unwrap_data(response);

        if data.is_array() {
            let targets: Vec<Target> =
                serde_json::from_value(data).context("Failed to parse targets list")?;
            Ok(targets)
        } else {
            Ok(Vec::new())
        }
    }

    pub fn create_target(&self, target_data: serde_json::Value) -> Result<Target> {
        let args = json!({ "request_data": target_data });
        let response = self.run_script("target_create", args)?;
        let data = Self::unwrap_data(response);

        serde_json::from_value(data).context("Failed to parse created target")
    }

    pub fn get_target(&self, target_id: &str) -> Result<Target> {
        let args = json!({ "request_data": { "targetId": target_id } });
        let response = self.run_script("target_get", args)?;
        let data = Self::unwrap_data(response);

        serde_json::from_value(data).context("Failed to parse target")
    }

    pub fn delete_target(&self, target_id: &str) -> Result<()> {
        let args = json!({ "request_data": { "targetId": target_id } });
        self.run_script("target_delete", args)?;
        Ok(())
    }

    // ─── Wishes ────────────────────────────────────────────────
    // Backed by Windmill scripts (f/{folder}/steerwish_*); those scripts
    // own the wish records and their lifecycle events. Calculation
    // (plan, verdicts, vigilance) lives in causal, keyed by wish id -
    // this service never computes steering state.

    pub fn list_wishes(&self) -> Result<Vec<crate::types::WishSummary>> {
        let response = self.run_script("steerwishes_get", json!({}))?;
        let data = Self::unwrap_data(response);

        if data.is_array() {
            let wishes: Vec<crate::types::WishSummary> =
                serde_json::from_value(data).context("Failed to parse wishes list")?;
            Ok(wishes)
        } else {
            Ok(Vec::new())
        }
    }

    pub fn create_wish(&self, wish_data: serde_json::Value) -> Result<crate::types::Wish> {
        let args = json!({ "request_data": wish_data });
        let response = self.run_script("steerwish_create", args)?;
        let data = Self::unwrap_data(response);

        serde_json::from_value(data).context("Failed to parse declared wish")
    }

    pub fn get_wish(&self, wish_id: &str) -> Result<crate::types::Wish> {
        let args = json!({ "request_data": { "wish_id": wish_id } });
        let response = self.run_script("steerwish_get", args)?;
        let data = Self::unwrap_data(response);

        let wish: crate::types::Wish =
            serde_json::from_value(data).context("Failed to parse wish")?;

        if wish.id.is_empty() {
            anyhow::bail!("Invalid wish response: missing id field");
        }

        Ok(wish)
    }

    pub fn close_wish(&self, wish_id: &str) -> Result<crate::types::Wish> {
        let args = json!({ "request_data": { "wish_id": wish_id } });
        let response = self.run_script("steerwish_close", args)?;
        let data = Self::unwrap_data(response);

        serde_json::from_value(data).context("Failed to parse closed wish")
    }

    pub fn update_wish(
        &self,
        wish_id: &str,
        body: serde_json::Value,
    ) -> Result<crate::types::Wish> {
        let mut args = body;
        if let Some(obj) = args.as_object_mut() {
            obj.insert("wish_id".to_string(), serde_json::json!(wish_id));
        }
        let response = self.run_script("steerwish_update", json!({ "request_data": args }))?;
        let data = Self::unwrap_data(response);

        serde_json::from_value(data).context("Failed to parse corrected wish")
    }

    /// DELETE is a marker, not a wait (designs/2026-10-03, lifted to
    /// wishes): enqueue the deletion executor and return its windmill
    /// job id. The client polls GET until 404. Inside the executor:
    /// close first (idempotent on already-closed), causal trouble
    /// flags deletion-failed, registry rows removed last.
    pub fn enqueue_wish_delete(&self, wish_id: &str) -> Result<uuid::Uuid> {
        let args = json!({ "request_data": { "wish_id": wish_id } });
        let script_path = self.script_path("steerwish_delete");
        self.client
            .run_script_async(&script_path, false, args, None)
            .context(format!(
                "Failed to enqueue Windmill script: {}",
                script_path
            ))
    }
}

#[cfg(test)]
mod login_tests {
    use super::*;

    // ── login: the token door, tested directly — it takes its url ──

    fn fake_login(status_body: (u16, String)) -> (super::tests_support::FakeWindmill, String) {
        let f = super::tests_support::FakeWindmill::start(vec![status_body]);
        let url = f.base_url();
        (f, url)
    }

    #[test]
    fn login_posts_credentials_and_trims_the_quotes() {
        let (fake, url) = fake_login((200, "\"tok-1\"".into()));
        let token = login_to_windmill(&url, "e@x.io", "secret").unwrap();
        assert_eq!(token, "tok-1");
        let rec = fake.recorded();
        assert_eq!(rec[0].0, "POST");
        assert_eq!(rec[0].1, "/api/auth/login");
        let body = &fake.bodies("/api/auth/login")[0];
        assert_eq!(body["email"], serde_json::json!("e@x.io"));
        assert_eq!(body["password"], serde_json::json!("secret"));
    }

    #[test]
    fn login_refuses_null_and_empty_tokens() {
        let (_fake, url) = fake_login((200, "null".into()));
        assert!(login_to_windmill(&url, "e", "p").is_err());
        let (_fake, url) = fake_login((200, String::new()));
        assert!(login_to_windmill(&url, "e", "p").is_err());
    }
}

#[cfg(test)]
mod tests_support {
    // shared std-only listener: scripted (status, body) queue answered
    // in arrival order, requests recorded; dry script = 404 so a
    // mis-counted test fails loudly instead of hanging
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    pub struct Recorded {
        pub method: String,
        pub path: String,
        pub body: String,
    }

    pub struct FakeWindmill {
        port: u16,
        log: Arc<Mutex<Vec<Recorded>>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
        let resp = format!(
            "HTTP/1.1 {} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status,
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).ok();
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
        for line in lines {
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = v.trim().parse().unwrap_or(0);
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
        })
    }

    impl FakeWindmill {
        pub fn start(script: Vec<(u16, String)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let log = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let queue = Arc::new(Mutex::new(
                script
                    .into_iter()
                    .collect::<std::collections::VecDeque<_>>(),
            ));
            let srv_log = Arc::clone(&log);
            let srv_stop = Arc::clone(&stop);
            let srv_queue = Arc::clone(&queue);
            let handle = std::thread::spawn(move || {
                while !srv_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                                .ok();
                            if let Some(recorded) = read_request(&mut stream) {
                                srv_log.lock().unwrap().push(recorded);
                                let (status, body) = srv_queue
                                    .lock()
                                    .unwrap()
                                    .pop_front()
                                    .unwrap_or((404, String::new()));
                                write_response(&mut stream, status, &body);
                            }
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
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

        pub fn base_url(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }

        pub fn recorded(&self) -> Vec<(String, String)> {
            self.log
                .lock()
                .unwrap()
                .iter()
                .map(|r| (r.method.clone(), r.path.clone()))
                .collect()
        }

        pub fn bodies(&self, fragment: &str) -> Vec<serde_json::Value> {
            self.log
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.path.contains(fragment))
                .map(|r| serde_json::from_str(&r.body).unwrap_or(serde_json::Value::Null))
                .collect()
        }
    }

    impl Drop for FakeWindmill {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.thread.take() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
mod wire_tests {
    use super::tests_support::FakeWindmill;
    use super::*;

    fn ok(body: &str) -> (u16, String) {
        (200, body.to_string())
    }
    /// the POST answer: windmill returns the bare job uuid
    fn job() -> (u16, String) {
        (200, "123e4567-e89b-12d3-a456-426614174000".into())
    }
    /// the poll answer: completed, successful, carrying the script's output
    fn done(result: &str) -> (u16, String) {
        (
            200,
            format!(r#"{{"completed":true,"success":true,"result":{}}}"#, result),
        )
    }

    /// One env window, sequential scenarios — env is process-global.
    #[test]
    fn the_client_rides_the_declared_scripts() {
        let causal = FakeWindmill::start(vec![]);
        std::env::set_var("WINDMILL_BASE_URL", causal.base_url());
        std::env::set_var("WM_TOKEN", "test-token");
        std::env::set_var("WM_WORKSPACE", "ws");
        std::env::set_var("BASE_INTERNAL_URL", causal.base_url());
        std::env::set_var("WINDMILL_FOLDER", "controller");
        let _ = WindmillClient::new().unwrap(); // env window proven constructible

        // run_script: the exact job path (folder-slash path %-encoded),
        // args verbatim, response returned whole
        let f = FakeWindmill::start(vec![job(), done(r#"{"wrapped": 7}"#)]);
        std::env::set_var("WINDMILL_BASE_URL", f.base_url());
        std::env::set_var("BASE_INTERNAL_URL", f.base_url());
        let client = WindmillClient::new().unwrap();
        let out = client
            .run_script("discover", serde_json::json!({"a": 1}))
            .unwrap();
        assert_eq!(out, serde_json::json!({"wrapped": 7}));
        assert_eq!(
            f.recorded()[0],
            (
                "POST".into(),
                "/api/w/ws/jobs/run/p/f%2Fcontroller%2Fdiscover".into()
            )
        );
        assert_eq!(f.bodies("discover")[0], serde_json::json!({"a": 1}));
        assert!(
            f.recorded()[1].1.contains("123e4567"),
            "the poll rides the job uuid: {:?}",
            f.recorded()
        );

        // create: rides systemtender_create, reports the duplicate flag
        let f = FakeWindmill::start(vec![
            job(),
            done(
                r#"{"duplicate": true, "data": {"id":"st-1","name":"bench","status":"running","createdAt":"2026-10-31T10:00:00Z"}}"#,
            ),
        ]);
        std::env::set_var("WINDMILL_BASE_URL", f.base_url());
        std::env::set_var("BASE_INTERNAL_URL", f.base_url());
        let client = WindmillClient::new().unwrap();
        let (s, dup) = client
            .create_systemtender(serde_json::json!({"seed": 42}))
            .unwrap();
        assert!(dup);
        assert_eq!(s.id, "st-1");
        assert_eq!(s.name, "bench");
        assert_eq!(s.status, "running");
        assert_eq!(
            f.bodies("systemtender_create")[0],
            serde_json::json!({"request_data": {"seed": 42}})
        );
        // absent flag reads as not-duplicate
        let f = FakeWindmill::start(vec![
            job(),
            done(r#"{"data": {"id":"st-1","name":"bench","status":"running","createdAt":"t"}}"#),
        ]);
        std::env::set_var("WINDMILL_BASE_URL", f.base_url());
        std::env::set_var("BASE_INTERNAL_URL", f.base_url());
        let client = WindmillClient::new().unwrap();
        let (_, dup) = client.create_systemtender(serde_json::json!({})).unwrap();
        assert!(!dup);

        // get: the id rides request_data, the data object unwraps
        let f = FakeWindmill::start(vec![
            job(),
            done(
                r#"{"data": {"id":"st-9","name":"n","status":"paused","createdAt":"t","config":{"k":1}}}"#,
            ),
        ]);
        std::env::set_var("WINDMILL_BASE_URL", f.base_url());
        std::env::set_var("BASE_INTERNAL_URL", f.base_url());
        let client = WindmillClient::new().unwrap();
        let st = client.get_systemtender("st-9").unwrap();
        assert_eq!(st.id, "st-9");
        assert_eq!(st.config, serde_json::json!({"k": 1}));
        assert_eq!(
            f.bodies("systemtender_get")[0],
            serde_json::json!({"request_data": {"systemtender_id": "st-9"}})
        );

        // stop and start ride their scripts with the id
        {
            let f = FakeWindmill::start(vec![job(), done(r#"{"data": {"done": true}}"#)]);
            std::env::set_var("WINDMILL_BASE_URL", f.base_url());
            std::env::set_var("BASE_INTERNAL_URL", f.base_url());
            let client = WindmillClient::new().unwrap();
            let out = client.stop_systemtender("st-1").unwrap();
            assert_eq!(out, serde_json::json!({"done": true}));
            assert_eq!(
                f.bodies("systemtender_stop")[0],
                serde_json::json!({"request_data": {"systemtender_id": "st-1"}})
            );
        }
        {
            let f = FakeWindmill::start(vec![job(), done(r#"{"data": {"done": true}}"#)]);
            std::env::set_var("WINDMILL_BASE_URL", f.base_url());
            std::env::set_var("BASE_INTERNAL_URL", f.base_url());
            let client = WindmillClient::new().unwrap();
            let out = client.start_systemtender("st-1").unwrap();
            assert_eq!(out, serde_json::json!({"done": true}));
            assert_eq!(
                f.bodies("systemtender_start")[0],
                serde_json::json!({"request_data": {"systemtender_id": "st-1"}})
            );
        }

        // update: config rides along, force only when declared
        let f = FakeWindmill::start(vec![job(), done(r#"{"data": {"ok": 1}}"#)]);
        std::env::set_var("WINDMILL_BASE_URL", f.base_url());
        std::env::set_var("BASE_INTERNAL_URL", f.base_url());
        let client = WindmillClient::new().unwrap();
        client
            .update_systemtender("st-1", serde_json::json!({"x": 1}), false)
            .unwrap();
        let body = &f.bodies("systemtender_update")[0];
        assert_eq!(
            body["request_data"]["systemtender_id"],
            serde_json::json!("st-1")
        );
        assert_eq!(body["request_data"]["config"], serde_json::json!({"x": 1}));
        assert!(
            body["request_data"].get("force").is_none(),
            "force stays off the wire unless declared"
        );

        // delete enqueue: force rides only when true
        let f = FakeWindmill::start(vec![ok("{}")]);
        std::env::set_var("WINDMILL_BASE_URL", f.base_url());
        std::env::set_var("BASE_INTERNAL_URL", f.base_url());
        let client = WindmillClient::new().unwrap();
        let _ = client.enqueue_systemtender_delete("st-1", false);
        assert!(f.bodies("systemtender_delete")[0]["request_data"]
            .get("force")
            .is_none());
        let f = FakeWindmill::start(vec![ok("{}")]);
        std::env::set_var("WINDMILL_BASE_URL", f.base_url());
        std::env::set_var("BASE_INTERNAL_URL", f.base_url());
        let client = WindmillClient::new().unwrap();
        let _ = client.enqueue_systemtender_delete("st-1", true);
        assert_eq!(
            f.bodies("systemtender_delete")[0]["request_data"]["force"],
            serde_json::json!(true)
        );

        std::env::remove_var("WINDMILL_BASE_URL");
        std::env::remove_var("BASE_INTERNAL_URL");
    }
}
