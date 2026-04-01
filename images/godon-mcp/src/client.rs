use anyhow::{Context, Result};
use log::debug;
use reqwest::Client;
use std::time::Duration;

pub struct GodonClient {
    base_url: String,
    client: Client,
}

impl Clone for GodonClient {
    fn clone(&self) -> Self {
        Self {
            base_url: self.base_url.clone(),
            client: self.client.clone(),
        }
    }
}

impl GodonClient {
    pub fn new(hostname: String, port: u16, insecure: bool) -> Self {
        let scheme = if hostname.starts_with("https://") {
            "https"
        } else {
            "http"
        };
        let clean_host = hostname
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        let base_url = format!("{}://{}:{}", scheme, clean_host, port);

        let mut builder = Client::builder().timeout(Duration::from_secs(30));
        if insecure {
            builder = builder.danger_accept_invalid_certs(true);
        }
        let client = builder.build().expect("Failed to create HTTP client");

        Self { base_url, client }
    }

    pub async fn get(&self, path: &str) -> Result<serde_json::Value> {
        let url = format!("{}{}", self.base_url, path);
        debug!("GET {}", url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {}", path))?;
        self.handle(resp).await
    }

    pub async fn post(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let url = format!("{}{}", self.base_url, path);
        debug!("POST {} body={}", url, body);
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {}", path))?;
        self.handle(resp).await
    }

    pub async fn post_empty(&self, path: &str) -> Result<serde_json::Value> {
        let url = format!("{}{}", self.base_url, path);
        debug!("POST {}", url);
        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .with_context(|| format!("POST {}", path))?;
        self.handle(resp).await
    }

    pub async fn delete(&self, path: &str) -> Result<serde_json::Value> {
        let url = format!("{}{}", self.base_url, path);
        debug!("DELETE {}", url);
        let resp = self
            .client
            .delete(&url)
            .send()
            .await
            .with_context(|| format!("DELETE {}", path))?;
        self.handle(resp).await
    }

    async fn handle(&self, resp: reqwest::Response) -> Result<serde_json::Value> {
        let status = resp.status();
        let body = resp.text().await?;
        debug!("Response {}: {}", status, body);
        if !status.is_success() {
            let msg = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .or_else(|| v.get("message"))
                        .and_then(|m| m.as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| format!("HTTP {}", status));
            anyhow::bail!("godon-api: {}", msg);
        }
        if body.trim().is_empty() {
            return Ok(serde_json::json!({}));
        }
        Ok(serde_json::from_str(&body).context("parse godon-api response")?)
    }
}
