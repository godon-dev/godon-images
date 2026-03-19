use anyhow::{Context, Result};
use log::info;
use reqwest::blocking::Client;
use serde_json::json;
use std::env;
use wmill::Windmill;

use crate::types::{Breeder, BreederSummary, Credential};

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
        
        let folder = env::var("WINDMILL_FOLDER")
            .unwrap_or_else(|_| "controller".to_string());
        
        let client = Windmill::default()
            .context("Failed to initialize Windmill client from environment")?;
        
        Ok(Self { client, folder })
    }

    fn script_path(&self, name: &str) -> String {
        format!("f/{}/{}", self.folder, name)
    }

    fn run_script(&self, script_name: &str, args: serde_json::Value) -> Result<serde_json::Value> {
        let script_path = self.script_path(script_name);
        let result = self.client
            .run_script_sync(
                &script_path,
                false,
                args,
                None,
                Some(60),
                true,
                false,
            )
            .context(format!("Failed to run Windmill script: {}", script_path))?;

        if let Some(result_str) = result.get("result").and_then(|r| r.as_str()) {
            if result_str == "FAILURE" {
                let error_msg = result.get("error")
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

    pub fn list_breeders(&self) -> Result<Vec<BreederSummary>> {
        let response = self.run_script("breeders_get", json!({}))?;
        let data = Self::unwrap_data(response);

        if data.is_array() {
            let breeders: Vec<BreederSummary> = serde_json::from_value(data)
                .context("Failed to parse breeders list")?;
            Ok(breeders)
        } else {
            Ok(Vec::new())
        }
    }

    pub fn create_breeder(&self, breeder_config: serde_json::Value) -> Result<BreederSummary> {
        let args = json!({ "request_data": breeder_config });
        let response = self.run_script("breeder_create", args)?;
        let data = Self::unwrap_data(response);
        
        serde_json::from_value(data)
            .context("Failed to parse created breeder")
    }

    pub fn get_breeder(&self, breeder_id: &str) -> Result<Breeder> {
        let args = json!({ "request_data": { "breeder_id": breeder_id } });
        let response = self.run_script("breeder_get", args)?;
        let data = Self::unwrap_data(response);

        let breeder: Breeder = serde_json::from_value(data)
            .context("Failed to parse breeder")?;
        
        if breeder.id.is_empty() {
            anyhow::bail!("Invalid breeder response: missing id field");
        }

        Ok(breeder)
    }

    pub fn delete_breeder(&self, breeder_id: &str, force: bool) -> Result<()> {
        let mut request_data = json!({ "breeder_id": breeder_id });
        if force {
            request_data["force"] = json!(force);
        }
        
        let args = json!({ "request_data": request_data });
        self.run_script("breeder_delete", args)?;
        Ok(())
    }

    pub fn stop_breeder(&self, breeder_id: &str) -> Result<serde_json::Value> {
        let args = json!({ "request_data": { "breeder_id": breeder_id } });
        let response = self.run_script("breeder_stop", args)?;
        Ok(Self::unwrap_data(response))
    }

    pub fn start_breeder(&self, breeder_id: &str) -> Result<serde_json::Value> {
        let args = json!({ "request_data": { "breeder_id": breeder_id } });
        let response = self.run_script("breeder_start", args)?;
        Ok(Self::unwrap_data(response))
    }

    pub fn list_credentials(&self) -> Result<Vec<Credential>> {
        let response = self.run_script("credentials_get", json!({}))?;
        let data = Self::unwrap_data(response);

        if data.is_array() {
            let credentials: Vec<Credential> = serde_json::from_value(data)
                .context("Failed to parse credentials list")?;
            Ok(credentials)
        } else {
            Ok(Vec::new())
        }
    }

    pub fn create_credential(&self, credential_data: serde_json::Value) -> Result<Credential> {
        let name = credential_data.get("name")
            .and_then(|n| n.as_str())
            .context("Missing name field")?;
        
        let content = credential_data.get("content")
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

        serde_json::from_value(data)
            .context("Failed to parse created credential")
    }

    pub fn get_credential(&self, credential_id: &str) -> Result<Credential> {
        let args = json!({ "request_data": { "credentialId": credential_id } });
        let response = self.run_script("credential_get", args)?;
        let data = Self::unwrap_data(response);

        let credential: Credential = serde_json::from_value(data)
            .context("Failed to parse credential")?;

        let content = self.client
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
        
        let credential: Credential = serde_json::from_value(data)
            .context("Failed to parse credential for deletion")?;

        let _ = self.client.set_variable(String::new(), &credential.windmill_variable, false);

        let args = json!({ "request_data": { "credentialId": credential_id } });
        self.run_script("credential_delete", args)?;
        Ok(())
    }
}
