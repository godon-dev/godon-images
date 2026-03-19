use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;
use std::thread;
use std::time::Duration;
use walkdir::WalkDir;

use crate::auth::{get_base_url, get_token};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScriptSettings {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FlowSettings {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScriptSpec {
    #[serde(default)]
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub settings: ScriptSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FlowSpec {
    #[serde(default)]
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub settings: FlowSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ComponentConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub scripts: Vec<ScriptSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flows: Option<Vec<FlowSpec>>,
}

#[derive(Debug, Clone)]
pub struct ComponentInfo {
    pub config: ComponentConfig,
    pub directory: String,
}

pub fn parse_component_config(yaml_path: &Path) -> Result<ComponentConfig> {
    log::info!("Parsing component config: {}", yaml_path.display());

    let content = std::fs::read_to_string(yaml_path)
        .context(format!("Failed to read component config: {}", yaml_path.display()))?;

    let config: ComponentConfig = serde_yaml::from_str(&content)
        .context(format!("Failed to parse component YAML: {}", yaml_path.display()))?;

    log::info!("Parsed component '{}' with {} scripts", config.name, config.scripts.len());

    Ok(config)
}

pub fn discover_components(source_dirs: &[String]) -> Result<Vec<ComponentInfo>> {
    let mut components = Vec::new();

    for source_dir in source_dirs {
        let path = Path::new(source_dir);
        if !path.exists() {
            log::warn!("Source directory does not exist: {}", source_dir);
            continue;
        }

        log::info!("Scanning directory: {}", source_dir);

        for entry in WalkDir::new(source_dir).into_iter().filter_map(|e| e.ok()) {
            let entry_path = entry.path();
            if entry_path.file_name().map(|n| n == "component.yaml").unwrap_or(false) {
                log::info!("Found component config: {}", entry_path.display());
                match parse_component_config(entry_path) {
                    Ok(config) => {
                        let component_dir = entry_path
                            .parent()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_default();

                        components.push(ComponentInfo {
                            config,
                            directory: component_dir,
                        });
                    }
                    Err(e) => {
                        log::error!("Failed to parse component config {}: {}", entry_path.display(), e);
                    }
                }
            }
        }
    }

    Ok(components)
}

fn find_files_by_pattern(base_dir: &Path, pattern: &str) -> Vec<String> {
    let mut files = Vec::new();

    let pattern_path = base_dir.join(pattern);
    let pattern_path_str = pattern_path.to_string_lossy();

    if pattern_path.exists() && pattern_path.is_file() {
        files.push(pattern_path_str.to_string());
        return files;
    }

    let parent = pattern_path.parent().unwrap_or(base_dir);
    if !parent.exists() {
        log::warn!("Pattern directory does not exist: {}", parent.display());
        return files;
    }

    let file_pattern = pattern_path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("*");

    log::debug!("Searching for files matching pattern: {} in {}", pattern, parent.display());

    if let Ok(paths) = glob::glob(&parent.join(file_pattern).to_string_lossy()) {
        for entry in paths.flatten() {
            if entry.is_file() {
                files.push(entry.to_string_lossy().to_string());
                log::debug!("  Found matching file: {}", entry.display());
            }
        }
    }

    files
}

fn detect_language(file_path: &str) -> &'static str {
    let ext = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match ext {
        "py" => "python3",
        "js" => "deno",
        "go" => "go",
        "sh" => "bash",
        "sql" => "postgresql",
        "ts" => "nativets",
        "yml" | "yaml" => "ansible",
        _ => "python3",
    }
}

pub struct WindmillDeployer {
    client: Client,
    base_url: String,
    token: String,
    max_retries: u32,
    retry_delay: u64,
}

impl WindmillDeployer {
    pub fn new(max_retries: u32, retry_delay: u64) -> Result<Self> {
        let client = Client::new();
        let base_url = get_base_url();
        let token = get_token().context("No Windmill token available")?;

        Ok(Self {
            client,
            base_url,
            token,
            max_retries,
            retry_delay,
        })
    }

    fn auth_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", self.token).parse().unwrap(),
        );
        headers
    }

    pub fn workspace_exists(&self, workspace: &str) -> Result<bool> {
        let url = format!("{}/api/workspaces/exists", self.base_url);

        let response = self.client
            .post(&url)
            .headers(self.auth_headers())
            .json(&json!({ "id": workspace }))
            .send()
            .context("Failed to check workspace existence")?;

        let body = response.text().context("Failed to read workspace check response")?;
        Ok(body.trim().to_lowercase() == "true")
    }

    pub fn create_workspace(&self, workspace: &str) -> Result<()> {
        if self.workspace_exists(workspace)? {
            log::info!("Workspace already exists: {}", workspace);
            return Ok(());
        }

        log::info!("Creating workspace: {}", workspace);

        let url = format!("{}/api/workspaces/create", self.base_url);

        let response = self.client
            .post(&url)
            .headers(self.auth_headers())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&json!({ "id": workspace, "name": workspace }))
            .send()
            .context("Failed to create workspace")?;

        if response.status().is_success() || response.status().as_u16() == 409 {
            log::info!("Successfully created workspace: {}", workspace);
            Ok(())
        } else {
            anyhow::bail!("Failed to create workspace: {}", response.status())
        }
    }

    pub fn create_folder(&self, workspace: &str, folder_path: &str) -> Result<()> {
        let folder_name = folder_path.strip_prefix("f/").unwrap_or(folder_path);
        let top_level = folder_name.split('/').next().unwrap_or(folder_name);

        log::info!("Creating folder: {} in workspace: {}", top_level, workspace);

        let url = format!("{}/api/w/{}/folders/create", self.base_url, workspace);

        let response = self.client
            .post(&url)
            .headers(self.auth_headers())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&json!({ "name": top_level }))
            .send()
            .context("Failed to create folder")?;

        if response.status().is_success() {
            log::info!("Successfully created folder: {}", folder_path);
        } else if response.status().as_u16() == 409 || response.status().as_u16() == 400 {
            log::debug!("Folder already exists: {}", folder_path);
        }

        Ok(())
    }

    pub fn script_exists(&self, workspace: &str, script_path: &str) -> Result<bool> {
        let url = format!("{}/api/w/{}/scripts/exists/p/{}", self.base_url, workspace, script_path);

        let response = self.client
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .context("Failed to check script existence")?;

        if response.status().is_success() {
            let body = response.json::<bool>().context("Failed to parse script exists response")?;
            Ok(body)
        } else {
            Ok(false)
        }
    }

    pub fn flow_exists(&self, workspace: &str, flow_path: &str) -> Result<bool> {
        let url = format!("{}/api/w/{}/flows/exists/{}", self.base_url, workspace, flow_path);

        let response = self.client
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .context("Failed to check flow existence")?;

        if response.status().is_success() {
            let body = response.json::<bool>().context("Failed to parse flow exists response")?;
            Ok(body)
        } else {
            Ok(false)
        }
    }

    pub fn deploy_script(
        &self,
        workspace: &str,
        script_path: &str,
        content: &str,
        settings: &ScriptSettings,
        file_path: &str,
    ) -> Result<()> {
        let url = format!("{}/api/w/{}/scripts/create", self.base_url, workspace);

        let language = detect_language(file_path);

        let mut payload = json!({
            "path": script_path,
            "content": content,
            "language": language,
            "summary": &settings.summary,
            "description": &settings.description,
        });

        if let Some(timeout) = settings.timeout {
            payload["timeout"] = json!(timeout);
        }
        if let Some(tag) = &settings.tag {
            payload["tag"] = json!(tag);
        }

        let response = self.client
            .post(&url)
            .headers(self.auth_headers())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()
            .context("Failed to deploy script")?;

        if response.status().as_u16() == 201 {
            log::info!("Successfully deployed script: {}", script_path);
            Ok(())
        } else {
            anyhow::bail!("Failed to deploy script {}: {}", script_path, response.status())
        }
    }

    pub fn deploy_flow(
        &self,
        workspace: &str,
        flow_path: &str,
        flow_yaml: &str,
        settings: &FlowSettings,
    ) -> Result<()> {
        let url = format!("{}/api/w/{}/flows/create", self.base_url, workspace);

        let mut flow_value: serde_json::Value = serde_yaml::from_str(flow_yaml)
            .context("Failed to parse flow YAML")?;

        if !settings.summary.is_empty() {
            flow_value["summary"] = json!(settings.summary);
        } else {
            flow_value["summary"] = json!("");
        }

        if !settings.description.is_empty() {
            flow_value["description"] = json!(settings.description);
        } else {
            flow_value["description"] = json!("");
        }

        if let Some(msg) = &settings.deployment_message {
            flow_value["deployment_message"] = json!(msg);
        }

        let mut payload = json!({ "path": flow_path });
        if let serde_json::Value::Object(ref mut map) = payload {
            if let serde_json::Value::Object(flow_map) = flow_value {
                for (k, v) in flow_map {
                    map.insert(k.clone(), v.clone());
                }
            }
        }

        let response = self.client
            .post(&url)
            .headers(self.auth_headers())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()
            .context("Failed to deploy flow")?;

        if response.status().as_u16() == 201 {
            log::info!("Successfully deployed flow: {}", flow_path);
            Ok(())
        } else {
            anyhow::bail!("Failed to deploy flow {}: {}", flow_path, response.status())
        }
    }

    pub fn deploy_script_with_retry(
        &self,
        workspace: &str,
        script_path: &str,
        content: &str,
        settings: &ScriptSettings,
        file_path: &str,
    ) -> Result<()> {
        if self.script_exists(workspace, script_path)? {
            log::info!("Script already exists, skipping deployment: {}", script_path);
            return Ok(());
        }

        for attempt in 0..=self.max_retries {
            match self.deploy_script(workspace, script_path, content, settings, file_path) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if attempt == self.max_retries {
                        log::error!(
                            "Failed to deploy script {} after {} attempts: {}",
                            script_path,
                            attempt + 1,
                            e
                        );
                        return Err(e);
                    }
                    log::warn!("Attempt {} failed for script {}: {}", attempt + 1, script_path, e);
                    log::info!("Retrying in {} seconds...", self.retry_delay);
                    thread::sleep(Duration::from_secs(self.retry_delay));
                }
            }
        }

        unreachable!()
    }

    pub fn deploy_flow_with_retry(
        &self,
        workspace: &str,
        flow_path: &str,
        flow_yaml: &str,
        settings: &FlowSettings,
    ) -> Result<()> {
        if self.flow_exists(workspace, flow_path)? {
            log::info!("Flow already exists, skipping deployment: {}", flow_path);
            return Ok(());
        }

        for attempt in 0..=self.max_retries {
            match self.deploy_flow(workspace, flow_path, flow_yaml, settings) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if attempt == self.max_retries {
                        log::error!(
                            "Failed to deploy flow {} after {} attempts: {}",
                            flow_path,
                            attempt + 1,
                            e
                        );
                        return Err(e);
                    }
                    log::warn!("Attempt {} failed for flow {}: {}", attempt + 1, flow_path, e);
                    log::info!("Retrying in {} seconds...", self.retry_delay);
                    thread::sleep(Duration::from_secs(self.retry_delay));
                }
            }
        }

        unreachable!()
    }

    pub fn deploy_component_scripts(
        &self,
        workspace: &str,
        component: &ComponentConfig,
        base_dir: &Path,
    ) -> u32 {
        log::info!("Deploying scripts for component: {}", component.name);

        let mut failures = 0u32;

        if !component.target.is_empty() {
            if let Err(e) = self.create_folder(workspace, &component.target) {
                log::debug!("Folder creation attempt failed: {}", e);
            }
        }

        for script_spec in &component.scripts {
            let mut script_files = Vec::new();

            if !script_spec.pattern.is_empty() {
                if let Some(ref path) = script_spec.path {
                    if !path.is_empty() {
                        let search_dir = base_dir.join(path);
                        script_files = find_files_by_pattern(&search_dir, &script_spec.pattern);
                    }
                }

                if script_files.is_empty() {
                    let pattern_path = base_dir.join(&script_spec.pattern);
                    if pattern_path.exists() && pattern_path.is_file() {
                        script_files.push(pattern_path.to_string_lossy().to_string());
                    } else if script_spec.path.is_none() || script_spec.path.as_ref().map_or(true, |p| p.is_empty()) {
                        script_files = find_files_by_pattern(base_dir, &script_spec.pattern);
                    }
                }
            } else if let Some(ref path) = script_spec.path {
                if !path.is_empty() {
                    script_files.push(base_dir.join(path).to_string_lossy().to_string());
                }
            } else {
                log::warn!("Script spec has neither pattern nor path, skipping");
                continue;
            }

            if script_files.is_empty() {
                log::warn!("No files found for script spec: {}", script_spec.pattern);
                continue;
            }

            for script_file in script_files {
                let script_path = Path::new(&script_file);
                let filename = script_path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown");

                let windmill_path = format!("f/{}/{}", component.target, filename);

                match std::fs::read_to_string(&script_file) {
                    Ok(content) => {
                        if let Err(e) = self.deploy_script_with_retry(
                            workspace,
                            &windmill_path,
                            &content,
                            &script_spec.settings,
                            &script_file,
                        ) {
                            log::error!("Failed to deploy script {} after retries: {}", script_file, e);
                            failures += 1;
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to read script file {}: {}", script_file, e);
                        failures += 1;
                    }
                }
            }
        }

        failures
    }

    pub fn deploy_component_flows(
        &self,
        workspace: &str,
        component: &ComponentConfig,
        base_dir: &Path,
    ) -> u32 {
        log::info!("Deploying flows for component: {}", component.name);

        let mut failures = 0u32;

        if !component.target.is_empty() {
            if let Err(e) = self.create_folder(workspace, &component.target) {
                log::debug!("Folder creation attempt failed: {}", e);
            }
        }

        let flows = match &component.flows {
            Some(f) => f,
            None => return 0,
        };

        for flow_spec in flows {
            let mut flow_files = Vec::new();

            if !flow_spec.pattern.is_empty() {
                let pattern_path = base_dir.join(&flow_spec.pattern);
                if pattern_path.exists() && pattern_path.is_file() {
                    flow_files.push(pattern_path.to_string_lossy().to_string());
                } else if let Some(ref path) = flow_spec.path {
                    if !path.is_empty() {
                        let search_dir = base_dir.join(path);
                        flow_files = find_files_by_pattern(&search_dir, &flow_spec.pattern);
                    }
                } else {
                    flow_files = find_files_by_pattern(base_dir, &flow_spec.pattern);
                }
            } else if let Some(ref path) = flow_spec.path {
                if !path.is_empty() {
                    flow_files.push(base_dir.join(path).to_string_lossy().to_string());
                }
            } else {
                log::warn!("Flow spec has neither pattern nor path, skipping");
                continue;
            }

            if flow_files.is_empty() {
                log::warn!("No files found for flow spec: {}", flow_spec.pattern);
                continue;
            }

            for flow_file in flow_files {
                let flow_path = Path::new(&flow_file);
                let filename = flow_path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown");

                let windmill_path = format!("f/{}/{}", component.target, filename);

                match std::fs::read_to_string(&flow_file) {
                    Ok(content) => {
                        if let Err(e) = self.deploy_flow_with_retry(
                            workspace,
                            &windmill_path,
                            &content,
                            &flow_spec.settings,
                        ) {
                            log::error!("Failed to deploy flow {} after retries: {}", flow_file, e);
                            failures += 1;
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to read flow file {}: {}", flow_file, e);
                        failures += 1;
                    }
                }
            }
        }

        failures
    }
}

pub fn seed_workspace(
    source_dirs: &[String],
    default_workspace: &str,
    max_retries: u32,
    retry_delay: u64,
) -> Result<u32> {
    log::info!("Starting component deployment");

    let mut total_failures = 0u32;

    let deployer = WindmillDeployer::new(max_retries, retry_delay)?;
    log::info!("Connected to Windmill");

    let components = discover_components(source_dirs)?;
    log::info!("Found {} components to deploy", components.len());

    let mut created_workspaces = HashSet::new();

    for component_info in components {
        let component = &component_info.config;
        log::info!("Deploying component: {}", component.name);

        let component_dir = Path::new(&component_info.directory);

        let target_workspace = if !component.workspace.is_empty() {
            component.workspace.clone()
        } else {
            default_workspace.to_string()
        };

        log::info!("Deploying to workspace: {}", target_workspace);

        if !created_workspaces.contains(&target_workspace) {
            log::info!("Ensuring workspace exists: {}", target_workspace);
            deployer.create_workspace(&target_workspace)?;
            created_workspaces.insert(target_workspace.clone());
        }

        if !component.scripts.is_empty() {
            let script_failures = deployer.deploy_component_scripts(&target_workspace, component, component_dir);
            total_failures += script_failures;
        }

        if let Some(ref flows) = component.flows {
            if !flows.is_empty() {
                let flow_failures = deployer.deploy_component_flows(&target_workspace, component, component_dir);
                total_failures += flow_failures;
            }
        }
    }

    if total_failures > 0 {
        log::error!("Component deployment completed with {} failures", total_failures);
    } else {
        log::info!("Component deployment completed successfully");
    }

    Ok(total_failures)
}
