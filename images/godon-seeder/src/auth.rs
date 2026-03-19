use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::json;
use std::env;
use std::thread;
use std::time::Duration;

pub fn login_to_windmill(base_url: &str, email: &str, password: &str, max_retries: u32, retry_delay: u64) -> Result<String> {
    let client = Client::new();
    let url = format!("{}/api/auth/login", base_url);

    for attempt in 0..=max_retries {
        log::info!("Logging into Windmill at: {} (attempt {}/{})", url, attempt + 1, max_retries + 1);

        match client
            .post(&url)
            .json(&json!({
                "email": email,
                "password": password
            }))
            .send()
        {
            Ok(response) => {
                match response.text() {
                    Ok(text) => {
                        let token = text.trim_matches('"').to_string();
                        if token.is_empty() || token == "null" {
                            if attempt == max_retries {
                                anyhow::bail!("Windmill login returned empty token after {} attempts", max_retries + 1);
                            }
                            log::warn!("Windmill login returned empty token, retrying in {} seconds...", retry_delay);
                            thread::sleep(Duration::from_secs(retry_delay));
                            continue;
                        }
                        log::info!("Successfully authenticated with Windmill");
                        return Ok(token);
                    }
                    Err(e) => {
                        if attempt == max_retries {
                            anyhow::bail!("Failed to read login response after {} attempts: {}", max_retries + 1, e);
                        }
                        log::warn!("Failed to read login response: {}, retrying in {} seconds...", e, retry_delay);
                        thread::sleep(Duration::from_secs(retry_delay));
                    }
                }
            }
            Err(e) => {
                if attempt == max_retries {
                    anyhow::bail!("Failed to send login request to Windmill after {} attempts: {}", max_retries + 1, e);
                }
                log::warn!("Failed to send login request: {}, retrying in {} seconds...", e, retry_delay);
                thread::sleep(Duration::from_secs(retry_delay));
            }
        }
    }

    anyhow::bail!("Failed to authenticate with Windmill after {} attempts", max_retries + 1)
}

pub fn setup_windmill_env(max_retries: u32, retry_delay: u64) -> Result<()> {
    let base_url = env::var("WINDMILL_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8000".to_string());
    let base_url = base_url.trim_end_matches("/api").to_string();

    if env::var("WM_TOKEN").is_err() {
        let token = if let Ok(token) = env::var("WINDMILL_TOKEN") {
            token
        } else {
            let email = env::var("WINDMILL_EMAIL")
                .unwrap_or_else(|_| "admin@windmill.dev".to_string());
            let password = env::var("WINDMILL_PASSWORD")
                .unwrap_or_else(|_| "changeme".to_string());

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
