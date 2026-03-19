use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::json;
use std::env;

pub fn login_to_windmill(base_url: &str, email: &str, password: &str) -> Result<String> {
    let client = Client::new();
    let url = format!("{}/api/auth/login", base_url);

    log::info!("Logging into Windmill at: {}", url);

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

    log::info!("Successfully authenticated with Windmill");
    Ok(token)
}

pub fn setup_windmill_env() -> Result<()> {
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
