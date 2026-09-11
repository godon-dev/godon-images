//! Relay client for the causal service (the map's calculator).
//!
//! READS ONLY: this client exposes the map's answers - connectome,
//! curves, artifact, predictions, impact, causes. It never mutates the
//! map: /build and curve deletion have no door through the control API.
//! The api relays, never computes.

use anyhow::{Context, Result};
use serde_json::Value;
use std::time::Duration;

pub struct CausalClient {
    base_url: String,
    inner: reqwest::blocking::Client,
}

impl CausalClient {
    pub fn new() -> Result<Self> {
        let base_url = std::env::var("GODON_CAUSAL_URL")
            .unwrap_or_else(|_| "http://godon-godon-causal:9091".to_string());
        let inner = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .context("Failed to build causal HTTP client")?;
        Ok(Self { base_url, inner })
    }

    pub fn get(&self, path: &str) -> Result<(reqwest::StatusCode, Value)> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .inner
            .get(&url)
            .send()
            .context("causal unreachable")?;
        let status = resp.status();
        let body: Value = resp.json().unwrap_or(Value::Null);
        Ok((status, body))
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<(reqwest::StatusCode, Value)> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .inner
            .post(&url)
            .json(body)
            .send()
            .context("causal unreachable")?;
        let status = resp.status();
        let body: Value = resp.json().unwrap_or(Value::Null);
        Ok((status, body))
    }
}
