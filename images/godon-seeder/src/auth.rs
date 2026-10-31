use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::json;
use std::env;
use std::thread;
use std::time::Duration;

pub fn login_to_windmill(
    base_url: &str,
    email: &str,
    password: &str,
    max_retries: u32,
    retry_delay: u64,
) -> Result<String> {
    let client = Client::new();
    let url = format!("{}/api/auth/login", base_url);

    for attempt in 0..=max_retries {
        log::info!(
            "Logging into Windmill at: {} (attempt {}/{})",
            url,
            attempt + 1,
            max_retries + 1
        );

        match client
            .post(&url)
            .json(&json!({
                "email": email,
                "password": password
            }))
            .send()
        {
            Ok(response) => match response.text() {
                Ok(text) => {
                    let token = text.trim_matches('"').to_string();
                    if token.is_empty() || token == "null" {
                        if attempt == max_retries {
                            anyhow::bail!(
                                "Windmill login returned empty token after {} attempts",
                                max_retries + 1
                            );
                        }
                        log::warn!(
                            "Windmill login returned empty token, retrying in {} seconds...",
                            retry_delay
                        );
                        thread::sleep(Duration::from_secs(retry_delay));
                        continue;
                    }
                    log::info!("Successfully authenticated with Windmill");
                    return Ok(token);
                }
                Err(e) => {
                    if attempt == max_retries {
                        anyhow::bail!(
                            "Failed to read login response after {} attempts: {}",
                            max_retries + 1,
                            e
                        );
                    }
                    log::warn!(
                        "Failed to read login response: {}, retrying in {} seconds...",
                        e,
                        retry_delay
                    );
                    thread::sleep(Duration::from_secs(retry_delay));
                }
            },
            Err(e) => {
                if attempt == max_retries {
                    anyhow::bail!(
                        "Failed to send login request to Windmill after {} attempts: {}",
                        max_retries + 1,
                        e
                    );
                }
                log::warn!(
                    "Failed to send login request: {}, retrying in {} seconds...",
                    e,
                    retry_delay
                );
                thread::sleep(Duration::from_secs(retry_delay));
            }
        }
    }

    anyhow::bail!(
        "Failed to authenticate with Windmill after {} attempts",
        max_retries + 1
    )
}

/// The estate's worker lanes must be dispatchable by job tokens (a
/// verb spawning its executor, the engine spawning components). The
/// platform gates tagged dispatch on the global custom_tags setting,
/// and a clean-slate boot resets that setting to its default - so
/// every seed re-plants it. The value is the ARRAY form: the
/// platform's loader parses a plain list of tag names.
pub fn set_lane_tags() -> Result<()> {
    let base_url =
        env::var("WINDMILL_BASE_URL").unwrap_or_else(|_| "http://localhost:8000".to_string());
    let base_url = base_url.trim_end_matches("/api").to_string();
    let token = env::var("WM_TOKEN").context("WM_TOKEN must be set (login ran first)")?;

    let resp = Client::new()
        .post(format!("{}/api/settings/global/custom_tags", base_url))
        .bearer_auth(token)
        .json(&json!({
            "value": ["chromium", "controller-fast", "controller-slow", "robot"]
        }))
        .send()
        .context("send the custom_tags setting")?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {} planting the lane tags", resp.status());
    }
    log::info!("Lane tags planted: chromium, controller-fast, controller-slow, robot");
    Ok(())
}

pub fn setup_windmill_env(max_retries: u32, retry_delay: u64) -> Result<()> {
    let base_url =
        env::var("WINDMILL_BASE_URL").unwrap_or_else(|_| "http://localhost:8000".to_string());
    let base_url = base_url.trim_end_matches("/api").to_string();

    if env::var("WM_TOKEN").is_err() {
        let token = if let Ok(token) = env::var("WINDMILL_TOKEN") {
            token
        } else {
            let email =
                env::var("WINDMILL_EMAIL").unwrap_or_else(|_| "admin@windmill.dev".to_string());
            let password = env::var("WINDMILL_PASSWORD").unwrap_or_else(|_| "changeme".to_string());

            login_to_windmill(&base_url, &email, &password, max_retries, retry_delay)?
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

    Ok(())
}

pub fn get_base_url() -> String {
    env::var("WINDMILL_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8000".to_string())
        .trim_end_matches("/api")
        .to_string()
}

pub fn get_token() -> Option<String> {
    env::var("WM_TOKEN").ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{created, ok, reset, truncated, FakeWindmill, ENV_LOCK};

    #[test]
    fn base_url_trims_the_api_suffix_and_defaults_when_unset() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = env::var("WINDMILL_BASE_URL").ok();
        env::set_var("WINDMILL_BASE_URL", "http://windmill:8000/api");
        assert_eq!(get_base_url(), "http://windmill:8000");
        env::set_var("WINDMILL_BASE_URL", "http://windmill:8000");
        assert_eq!(get_base_url(), "http://windmill:8000");
        env::remove_var("WINDMILL_BASE_URL");
        assert_eq!(get_base_url(), "http://localhost:8000");
        if let Some(v) = saved {
            env::set_var("WINDMILL_BASE_URL", v);
        }
    }

    // ── login: the token contract, straight against the wire ──

    #[test]
    fn login_speaks_the_token_contract() {
        // the platform answers a JSON-quoted token; the quotes strip
        let f = FakeWindmill::start(vec![ok("\"tok123\"")]);
        let token = login_to_windmill(&f.url(), "a@b.c", "pw", 0, 0).unwrap();
        assert_eq!(token, "tok123");
        let body = &f.bodies("/api/auth/login")[0];
        assert_eq!(body["email"], json!("a@b.c"));
        assert_eq!(body["password"], json!("pw"));

        // an empty answer is a refusal, not a token
        let f = FakeWindmill::start(vec![ok("")]);
        assert!(login_to_windmill(&f.url(), "a@b.c", "pw", 0, 0).is_err());

        // and so is a literal null
        let f = FakeWindmill::start(vec![ok("null")]);
        assert!(login_to_windmill(&f.url(), "a@b.c", "pw", 0, 0).is_err());
    }

    #[test]
    fn login_retries_each_fault_class_to_the_declared_count() {
        // empty tokens: both attempts spent, the empty-token refusal said
        let f = FakeWindmill::start(vec![ok(""), ok("")]);
        let err = login_to_windmill(&f.url(), "a@b.c", "pw", 1, 0).unwrap_err();
        assert!(
            err.to_string().contains("empty token after 2 attempts"),
            "the refusal names its class: {err}"
        );
        assert_eq!(f.requests(), 2);

        // a body that stops short of its Content-Length: the read
        // fault retries too (the connection dies mid-message)
        let f = FakeWindmill::start_bytes(vec![truncated("part"), truncated("part")]);
        let err = login_to_windmill(&f.url(), "a@b.c", "pw", 1, 0).unwrap_err();
        assert!(
            err.to_string().contains("Failed to read login response"),
            "{err}"
        );
        assert_eq!(f.requests(), 2);

        // the connection dying mid-request: the send fault retries too
        let f = FakeWindmill::start(vec![reset(), reset()]);
        let err = login_to_windmill(&f.url(), "a@b.c", "pw", 1, 0).unwrap_err();
        assert!(
            err.to_string().contains("Failed to send login request"),
            "{err}"
        );
        assert_eq!(f.requests(), 2);
    }

    // ── the env doors: tags planted, tokens propagated ──

    #[test]
    fn the_env_doors_plant_tags_and_propagate_tokens() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(&str, Option<String>)> = [
            "WINDMILL_BASE_URL",
            "WM_TOKEN",
            "WM_WORKSPACE",
            "BASE_INTERNAL_URL",
            "WINDMILL_TOKEN",
            "WINDMILL_WORKSPACE",
            "WINDMILL_EMAIL",
            "WINDMILL_PASSWORD",
        ]
        .iter()
        .map(|k| (*k, env::var(k).ok()))
        .collect();

        // lane tags: the array form, on the settings door, bearer-spoken
        let f = FakeWindmill::start(vec![created()]);
        env::set_var("WINDMILL_BASE_URL", f.url());
        env::set_var("WM_TOKEN", "envtok");
        set_lane_tags().unwrap();
        assert_eq!(
            f.recorded(),
            vec![("POST".into(), "/api/settings/global/custom_tags".into())]
        );
        assert_eq!(f.bearers(), vec![Some("envtok".into())]);
        assert_eq!(
            f.bodies("/custom_tags")[0]["value"],
            json!(["chromium", "controller-fast", "controller-slow", "robot"])
        );

        // a refusing settings door is an error, not a shrug
        let f = FakeWindmill::start(vec![(500, String::new())]);
        env::set_var("WINDMILL_BASE_URL", f.url());
        let err = set_lane_tags().unwrap_err();
        assert!(err.to_string().contains("HTTP 500"), "{err}");

        // get_token reads the planted var verbatim
        env::set_var("WM_TOKEN", "verbatim-token");
        assert_eq!(get_token(), Some("verbatim-token".into()));

        // setup: a fresh env logs in once and plants all three vars
        env::remove_var("WM_TOKEN");
        env::remove_var("WINDMILL_TOKEN");
        env::remove_var("WM_WORKSPACE");
        env::remove_var("BASE_INTERNAL_URL");
        let f = FakeWindmill::start(vec![ok("\"setuptok\"")]);
        env::set_var("WINDMILL_BASE_URL", f.url());
        env::set_var("WINDMILL_EMAIL", "seed@x.y");
        env::set_var("WINDMILL_PASSWORD", "pw2");
        env::set_var("WINDMILL_WORKSPACE", "wsx");
        setup_windmill_env(0, 0).unwrap();
        assert_eq!(env::var("WM_TOKEN").unwrap(), "setuptok");
        assert_eq!(env::var("WM_WORKSPACE").unwrap(), "wsx");
        assert_eq!(env::var("BASE_INTERNAL_URL").unwrap(), f.url());
        assert_eq!(f.requests(), 1, "the login is the only traffic");

        for (k, v) in saved {
            match v {
                Some(v) => env::set_var(k, v),
                None => env::remove_var(k),
            }
        }
    }
}
