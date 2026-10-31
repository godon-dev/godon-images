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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag_env: Option<String>,
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

/// A declared schedule: one repeating trigger on a declared script's
/// path. Schedules are estate state like scripts - declared here,
/// planted here, never hand-set (designs/2026-10-18-worker-death-door).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScheduleSpec {
    #[serde(default)]
    pub script: String,
    #[serde(default)]
    pub every_seconds: i64,
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
    #[serde(default)]
    pub schedules: Vec<ScheduleSpec>,
}

#[derive(Debug, Clone)]
pub struct ComponentInfo {
    pub config: ComponentConfig,
    pub directory: String,
}

pub fn parse_component_config(yaml_path: &Path) -> Result<ComponentConfig> {
    log::info!("Parsing component config: {}", yaml_path.display());

    let content = std::fs::read_to_string(yaml_path).context(format!(
        "Failed to read component config: {}",
        yaml_path.display()
    ))?;

    let config: ComponentConfig = serde_yaml::from_str(&content).context(format!(
        "Failed to parse component YAML: {}",
        yaml_path.display()
    ))?;

    log::info!(
        "Parsed component '{}' with {} scripts",
        config.name,
        config.scripts.len()
    );

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
            if entry_path
                .file_name()
                .map(|n| n == "component.yaml")
                .unwrap_or(false)
            {
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
                        log::error!(
                            "Failed to parse component config {}: {}",
                            entry_path.display(),
                            e
                        );
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

    let file_pattern = pattern_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("*");

    log::debug!(
        "Searching for files matching pattern: {} in {}",
        pattern,
        parent.display()
    );

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

    /// A test seam: the same deployer pointed at a declared base url
    /// with a standing token, instead of the environment. The wire
    /// contract is the thing under test — the fakes speak real HTTP
    /// against a local listener, never a mocked client.
    #[cfg(test)]
    pub(crate) fn with_base_url(base_url: &str, max_retries: u32, retry_delay: u64) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.to_string(),
            token: "test-token".to_string(),
            max_retries,
            retry_delay,
        }
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

        let response = self
            .client
            .post(&url)
            .headers(self.auth_headers())
            .json(&json!({ "id": workspace }))
            .send()
            .context("Failed to check workspace existence")?;

        let body = response
            .text()
            .context("Failed to read workspace check response")?;
        Ok(body.trim().to_lowercase() == "true")
    }

    pub fn create_workspace(&self, workspace: &str) -> Result<()> {
        if self.workspace_exists(workspace)? {
            log::info!("Workspace already exists: {}", workspace);
            return Ok(());
        }

        log::info!("Creating workspace: {}", workspace);

        let url = format!("{}/api/workspaces/create", self.base_url);

        let response = self
            .client
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

        let response = self
            .client
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
        let url = format!(
            "{}/api/w/{}/scripts/exists/p/{}",
            self.base_url, workspace, script_path
        );

        let response = self
            .client
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .context("Failed to check script existence")?;

        if response.status().is_success() {
            let body = response
                .json::<bool>()
                .context("Failed to parse script exists response")?;
            Ok(body)
        } else {
            Ok(false)
        }
    }

    pub fn flow_exists(&self, workspace: &str, flow_path: &str) -> Result<bool> {
        let url = format!(
            "{}/api/w/{}/flows/exists/{}",
            self.base_url, workspace, flow_path
        );

        let response = self
            .client
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .context("Failed to check flow existence")?;

        if response.status().is_success() {
            let body = response
                .json::<bool>()
                .context("Failed to parse flow exists response")?;
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

        let response = self
            .client
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
            anyhow::bail!(
                "Failed to deploy script {}: {}",
                script_path,
                response.status()
            )
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

        let mut flow_value: serde_json::Value =
            serde_yaml::from_str(flow_yaml).context("Failed to parse flow YAML")?;

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

        let response = self
            .client
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
            log::info!(
                "Script already exists, skipping deployment: {}",
                script_path
            );
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
                    log::warn!(
                        "Attempt {} failed for script {}: {}",
                        attempt + 1,
                        script_path,
                        e
                    );
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
                    log::warn!(
                        "Attempt {} failed for flow {}: {}",
                        attempt + 1,
                        flow_path,
                        e
                    );
                    log::info!("Retrying in {} seconds...", self.retry_delay);
                    thread::sleep(Duration::from_secs(self.retry_delay));
                }
            }
        }

        unreachable!()
    }

    /// A schedule's cadence as windmill's six-field cron (seconds
    /// first: "0 * * * * *" is once a minute at :00). Whole seconds up
    /// to 59 step the seconds field; whole minutes step the minute
    /// field. Anything else refuses - a mis-sized cadence is a config
    /// fault, not a guess to round.
    pub fn every_to_cron(every_seconds: i64) -> Result<String> {
        if every_seconds <= 0 {
            anyhow::bail!("every_seconds must be positive, got {}", every_seconds);
        }
        if every_seconds < 60 {
            return Ok(format!("*/{} * * * * *", every_seconds));
        }
        if every_seconds % 60 == 0 {
            let minutes = every_seconds / 60;
            if minutes == 1 {
                return Ok("0 * * * * *".to_string());
            }
            if minutes < 60 {
                return Ok(format!("0 */{} * * * *", minutes));
            }
        }
        anyhow::bail!(
            "unsupported cadence {}s - whole seconds under a minute or whole minutes under an hour",
            every_seconds
        );
    }

    pub fn schedule_exists(&self, workspace: &str, schedule_path: &str) -> Result<bool> {
        let url = format!(
            "{}/api/w/{}/schedules/exists/{}",
            self.base_url, workspace, schedule_path
        );

        let response = self
            .client
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .context("Failed to check schedule existence")?;

        if response.status().is_success() {
            let body = response
                .json::<bool>()
                .context("Failed to parse schedule exists response")?;
            Ok(body)
        } else {
            Ok(false)
        }
    }

    pub fn deploy_schedule(
        &self,
        workspace: &str,
        schedule_path: &str,
        cron: &str,
        script_path: &str,
        summary: &str,
    ) -> Result<()> {
        let url = format!("{}/api/w/{}/schedules/create", self.base_url, workspace);

        let payload = json!({
            "path": schedule_path,
            "schedule": cron,
            "timezone": "UTC",
            "script_path": script_path,
            "is_flow": false,
            "enabled": true,
            "summary": summary,
        });

        let response = self
            .client
            .post(&url)
            .headers(self.auth_headers())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()
            .context("Failed to create schedule")?;

        let status = response.status().as_u16();
        if status == 200 || status == 201 {
            log::info!(
                "Successfully deployed schedule {} on {} ({})",
                schedule_path,
                script_path,
                cron
            );
            Ok(())
        } else {
            anyhow::bail!(
                "Failed to deploy schedule {}: {} - body: {:?}",
                schedule_path,
                response.status(),
                response.text().unwrap_or_default()
            )
        }
    }

    pub fn deploy_schedule_with_retry(
        &self,
        workspace: &str,
        schedule_path: &str,
        cron: &str,
        script_path: &str,
        summary: &str,
    ) -> Result<()> {
        if self.schedule_exists(workspace, schedule_path)? {
            log::info!("Schedule already exists, skipping: {}", schedule_path);
            return Ok(());
        }

        for attempt in 0..=self.max_retries {
            match self.deploy_schedule(workspace, schedule_path, cron, script_path, summary) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if attempt == self.max_retries {
                        log::error!(
                            "Failed to deploy schedule {} after {} attempts: {}",
                            schedule_path,
                            attempt + 1,
                            e
                        );
                        return Err(e);
                    }
                    log::warn!(
                        "Attempt {} failed for schedule {}: {}",
                        attempt + 1,
                        schedule_path,
                        e
                    );
                    log::info!("Retrying in {} seconds...", self.retry_delay);
                    thread::sleep(Duration::from_secs(self.retry_delay));
                }
            }
        }

        unreachable!()
    }

    /// Plant a component's declared schedules. Refuses loud on a
    /// missing name or cadence, and on a schedule whose script the
    /// same run did not plant - a trigger on a missing script is a
    /// wedge, not a warning.
    pub fn deploy_component_schedules(&self, workspace: &str, component: &ComponentConfig) -> u32 {
        let mut failures = 0u32;

        for spec in &component.schedules {
            if spec.script.trim().is_empty() {
                log::error!(
                    "schedule spec without a script name (target {}): every declared schedule needs its script",
                    component.target
                );
                failures += 1;
                continue;
            }
            if component.target.trim().is_empty() {
                log::error!(
                    "schedule '{}' carries no target - a schedule path derives from it",
                    spec.script
                );
                failures += 1;
                continue;
            }

            let script_path = format!("f/{}/{}", component.target, spec.script);
            let script_planted = match self.script_exists(workspace, &script_path) {
                Ok(exists) => exists,
                Err(e) => {
                    log::error!(
                        "schedule '{}': script existence check failed for {}: {}",
                        spec.script,
                        script_path,
                        e
                    );
                    failures += 1;
                    continue;
                }
            };
            if !script_planted {
                log::error!(
                    "schedule '{}' targets script {} which this run did not plant - refusing",
                    spec.script,
                    script_path
                );
                failures += 1;
                continue;
            }

            let cron = match Self::every_to_cron(spec.every_seconds) {
                Ok(c) => c,
                Err(e) => {
                    log::error!("schedule '{}': {}", spec.script, e);
                    failures += 1;
                    continue;
                }
            };

            let schedule_path = format!("{}_schedule", script_path);
            if let Err(e) = self.deploy_schedule_with_retry(
                workspace,
                &schedule_path,
                &cron,
                &script_path,
                &format!("{} - declared repeating trigger", component.name),
            ) {
                log::error!(
                    "Failed to deploy schedule {} after retries: {}",
                    schedule_path,
                    e
                );
                failures += 1;
            }
        }

        failures
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
            if script_spec.script.is_some()
                || script_spec.sandbox.is_some()
                || script_spec.tag_env.is_some()
            {
                failures += self.deploy_sandbox_script(workspace, component, script_spec);
                continue;
            }

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
                    } else if script_spec.path.is_none()
                        || script_spec.path.as_ref().is_none_or(|p| p.is_empty())
                    {
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
                            log::error!(
                                "Failed to deploy script {} after retries: {}",
                                script_file,
                                e
                            );
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

    fn deploy_sandbox_script(
        &self,
        workspace: &str,
        component: &ComponentConfig,
        spec: &ScriptSpec,
    ) -> u32 {
        let stem = match spec.script.as_deref() {
            Some(s) if !s.is_empty() => s,
            _ => {
                log::error!(
                    "sandbox spec without a script name (target {}): every declared script needs its name",
                    component.target
                );
                return 1;
            }
        };

        if !spec.pattern.is_empty() {
            log::error!(
                "script spec '{}' carries both script and pattern - ambiguous, refusing",
                stem
            );
            return 1;
        }

        let image = match spec.sandbox.as_deref() {
            Some(i) if !i.is_empty() => i,
            _ => {
                log::error!(
                    "script '{}' declares no sandbox image - a declared script has no file to pin one",
                    stem
                );
                return 1;
            }
        };

        let tag_env = match spec.tag_env.as_deref() {
            Some(e) if !e.is_empty() => e,
            _ => {
                log::error!(
                    "script '{}' declares no tag env - refusing an image ref that could drift to :latest",
                    stem
                );
                return 1;
            }
        };

        let tag = match std::env::var(tag_env) {
            Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
            _ => {
                log::error!(
                    "env {} for script '{}' is unset or empty - refusing to deploy an untagged sandbox image",
                    tag_env,
                    stem
                );
                return 1;
            }
        };

        // A sha256:-prefixed value pins the digest; anything else is a tag.
        let separator = if tag.starts_with("sha256:") { "@" } else { ":" };
        let content = format!("# sandbox {}{}{}\n", image, separator, tag);

        let windmill_path = format!("f/{}/{}", component.target, stem);
        let language_hint = format!("{}.sh", stem);

        log::info!(
            "Deploying declared sandbox script {} (image {}{}{})",
            windmill_path,
            image,
            separator,
            tag
        );

        match self.deploy_script_with_retry(
            workspace,
            &windmill_path,
            &content,
            &spec.settings,
            &language_hint,
        ) {
            Ok(()) => 0,
            Err(e) => {
                log::error!(
                    "Failed to deploy declared sandbox script {}: {}",
                    windmill_path,
                    e
                );
                1
            }
        }
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
            let script_failures =
                deployer.deploy_component_scripts(&target_workspace, component, component_dir);
            total_failures += script_failures;
        }

        if !component.schedules.is_empty() {
            let schedule_failures =
                deployer.deploy_component_schedules(&target_workspace, component);
            total_failures += schedule_failures;
        }

        if let Some(ref flows) = component.flows {
            if !flows.is_empty() {
                let flow_failures =
                    deployer.deploy_component_flows(&target_workspace, component, component_dir);
                total_failures += flow_failures;
            }
        }
    }

    if total_failures > 0 {
        log::error!(
            "Component deployment completed with {} failures",
            total_failures
        );
    } else {
        log::info!("Component deployment completed successfully");
    }

    Ok(total_failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("godon-seeder-test-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ── the cadence converter: the schedule grammar's one rule-set ──

    #[test]
    fn cron_whole_seconds_under_a_minute_step_the_seconds_field() {
        assert_eq!(
            WindmillDeployer::every_to_cron(30).unwrap(),
            "*/30 * * * * *"
        );
        assert_eq!(WindmillDeployer::every_to_cron(1).unwrap(), "*/1 * * * * *");
        assert_eq!(
            WindmillDeployer::every_to_cron(59).unwrap(),
            "*/59 * * * * *"
        );
    }

    #[test]
    fn cron_one_minute_is_the_plain_minute_tick() {
        assert_eq!(WindmillDeployer::every_to_cron(60).unwrap(), "0 * * * * *");
    }

    #[test]
    fn cron_whole_minutes_under_an_hour_step_the_minute_field() {
        assert_eq!(
            WindmillDeployer::every_to_cron(120).unwrap(),
            "0 */2 * * * *"
        );
        assert_eq!(
            WindmillDeployer::every_to_cron(59 * 60).unwrap(),
            "0 */59 * * * *"
        );
    }

    #[test]
    fn cron_refuses_zero_negative_and_non_whole_cadences() {
        assert!(WindmillDeployer::every_to_cron(0).is_err());
        assert!(WindmillDeployer::every_to_cron(-60).is_err());
        // 90s is neither whole seconds under a minute nor a whole minute
        assert!(WindmillDeployer::every_to_cron(90).is_err());
        // an hour and beyond is out of the supported band
        assert!(WindmillDeployer::every_to_cron(3600).is_err());
    }

    // ── the language map ──

    #[test]
    fn language_follows_the_extension() {
        assert_eq!(detect_language("run.py"), "python3");
        assert_eq!(detect_language("run.js"), "deno");
        assert_eq!(detect_language("run.go"), "go");
        assert_eq!(detect_language("run.sh"), "bash");
        assert_eq!(detect_language("run.sql"), "postgresql");
        assert_eq!(detect_language("run.ts"), "nativets");
        assert_eq!(detect_language("playbook.yml"), "ansible");
        assert_eq!(detect_language("playbook.yaml"), "ansible");
    }

    #[test]
    fn unknown_extensions_default_to_python() {
        assert_eq!(detect_language("run.rb"), "python3");
        assert_eq!(detect_language("no-extension"), "python3");
    }

    // ── pattern → files ──

    #[test]
    fn pattern_exact_file_returns_that_file() {
        let root = temp_root("exact");
        std::fs::write(root.join("a.ts"), "x").unwrap();
        let files = find_files_by_pattern(&root, "a.ts");
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("a.ts"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pattern_wildcard_returns_matching_files_only() {
        let root = temp_root("glob");
        std::fs::write(root.join("a.ts"), "x").unwrap();
        std::fs::write(root.join("b.ts"), "x").unwrap();
        std::fs::write(root.join("c.py"), "x").unwrap();
        let files = find_files_by_pattern(&root, "*.ts");
        assert_eq!(files.len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pattern_in_a_missing_directory_is_empty_not_an_error() {
        let root = temp_root("missing");
        let files = find_files_by_pattern(&root.join("nowhere"), "*.ts");
        assert!(files.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── component config parsing ──

    #[test]
    fn config_parses_the_full_shape() {
        let root = temp_root("cfgfull");
        let cfg_path = root.join("component.yaml");
        std::fs::write(
            &cfg_path,
            "name: causal\nworkspace: godon\ntarget: causal\n\
             scripts:\n  - pattern: '*.py'\n    path: src\n    settings:\n      summary: s\n      timeout: 30\n\
             schedules:\n  - script: duty\n    every_seconds: 120\n",
        )
        .unwrap();
        let cfg = parse_component_config(&cfg_path).unwrap();
        assert_eq!(cfg.name, "causal");
        assert_eq!(cfg.workspace, "godon");
        assert_eq!(cfg.target, "causal");
        assert_eq!(cfg.scripts.len(), 1);
        assert_eq!(cfg.scripts[0].pattern, "*.py");
        assert_eq!(cfg.scripts[0].path.as_deref(), Some("src"));
        assert_eq!(cfg.scripts[0].settings.summary, "s");
        assert_eq!(cfg.scripts[0].settings.timeout, Some(30));
        assert!(cfg.flows.is_none());
        assert_eq!(cfg.schedules.len(), 1);
        assert_eq!(cfg.schedules[0].script, "duty");
        assert_eq!(cfg.schedules[0].every_seconds, 120);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn config_defaults_when_fields_are_absent() {
        let root = temp_root("cfgdef");
        let cfg_path = root.join("component.yaml");
        std::fs::write(&cfg_path, "").unwrap();
        let cfg = parse_component_config(&cfg_path).unwrap();
        assert_eq!(cfg.name, "");
        assert_eq!(cfg.workspace, "");
        assert_eq!(cfg.target, "");
        assert!(cfg.scripts.is_empty());
        assert!(cfg.schedules.is_empty());
        assert!(cfg.flows.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn config_refuses_broken_yaml() {
        let root = temp_root("cfgbad");
        let cfg_path = root.join("component.yaml");
        std::fs::write(&cfg_path, "{not yaml").unwrap();
        assert!(parse_component_config(&cfg_path).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn script_settings_wire_shape_omits_unset_optionals() {
        let plain = ScriptSettings {
            summary: "s".into(),
            description: String::new(),
            timeout: None,
            tag: None,
        };
        let json = serde_json::to_string(&plain).unwrap();
        assert!(!json.contains("timeout"));
        assert!(!json.contains("tag"));
        let pinned = ScriptSettings {
            timeout: Some(45),
            tag: Some("robot".into()),
            ..plain.clone()
        };
        let json = serde_json::to_string(&pinned).unwrap();
        assert!(json.contains("\"timeout\":45"));
        assert!(json.contains("\"tag\":\"robot\""));
    }

    // ── discovery ──

    #[test]
    fn discovery_finds_nested_components_and_skips_broken_ones() {
        let root = temp_root("disc");
        std::fs::create_dir_all(root.join("alpha/inner")).unwrap();
        std::fs::create_dir_all(root.join("beta")).unwrap();
        std::fs::write(
            root.join("alpha/component.yaml"),
            "name: alpha\ntarget: a\n",
        )
        .unwrap();
        std::fs::write(
            root.join("alpha/inner/component.yaml"),
            "name: inner\ntarget: i\n",
        )
        .unwrap();
        std::fs::write(root.join("beta/component.yaml"), "{broken").unwrap();

        let found = discover_components(&[root.to_string_lossy().to_string()]).unwrap();
        let mut names: Vec<&str> = found.iter().map(|c| c.config.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["alpha", "inner"]);
        // each component carries its own directory
        for c in &found {
            let dir = std::path::Path::new(&c.directory);
            assert!(dir.join("component.yaml").exists());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovery_tolerates_a_missing_source_dir() {
        let root = temp_root("discmiss");
        let found =
            discover_components(&[root.join("absent").to_string_lossy().to_string()]).unwrap();
        assert!(found.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── the safety doors: refusals happen before any network call ──

    use std::env;

    fn deployer() -> WindmillDeployer {
        env::set_var("WM_TOKEN", "test-token");
        WindmillDeployer::new(0, 0).expect("deployer constructs with a token present")
    }

    fn spec(
        script: Option<&str>,
        pattern: &str,
        sandbox: Option<&str>,
        tag_env: Option<&str>,
    ) -> ScriptSpec {
        ScriptSpec {
            pattern: pattern.to_string(),
            script: script.map(str::to_string),
            sandbox: sandbox.map(str::to_string),
            tag_env: tag_env.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn sandbox_refuses_without_a_script_name() {
        let d = deployer();
        let cfg = ComponentConfig::default();
        // sandbox set but no script name: refuses without touching the network
        let failures = d.deploy_sandbox_script("w", &cfg, &spec(None, "", Some("img"), Some("T")));
        assert_eq!(failures, 1);
    }

    #[test]
    fn sandbox_refuses_script_plus_pattern_ambiguity() {
        let d = deployer();
        let cfg = ComponentConfig::default();
        let failures =
            d.deploy_sandbox_script("w", &cfg, &spec(Some("s"), "*.py", Some("img"), Some("T")));
        assert_eq!(failures, 1);
    }

    #[test]
    fn sandbox_refuses_missing_sandbox_or_tag_env() {
        let d = deployer();
        let cfg = ComponentConfig::default();
        // no sandbox image
        assert_eq!(
            d.deploy_sandbox_script("w", &cfg, &spec(Some("s"), "", None, Some("T"))),
            1
        );
        // no tag env - an image ref that could drift to :latest is refused
        assert_eq!(
            d.deploy_sandbox_script("w", &cfg, &spec(Some("s"), "", Some("img"), None)),
            1
        );
    }

    #[test]
    fn sandbox_refuses_when_the_tag_env_is_unset_or_blank() {
        let d = deployer();
        let cfg = ComponentConfig::default();
        env::remove_var("GODON_SEEDER_TEST_TAG");
        assert_eq!(
            d.deploy_sandbox_script(
                "w",
                &cfg,
                &spec(Some("s"), "", Some("img"), Some("GODON_SEEDER_TEST_TAG"))
            ),
            1
        );
        env::set_var("GODON_SEEDER_TEST_TAG", "   ");
        assert_eq!(
            d.deploy_sandbox_script(
                "w",
                &cfg,
                &spec(Some("s"), "", Some("img"), Some("GODON_SEEDER_TEST_TAG"))
            ),
            1
        );
        env::remove_var("GODON_SEEDER_TEST_TAG");
    }

    #[test]
    fn schedule_refuses_nameless_script_and_targetless_component() {
        let d = deployer();
        // a schedule without its script name
        let mut cfg = ComponentConfig {
            target: "t".into(),
            schedules: vec![ScheduleSpec {
                script: "  ".into(),
                every_seconds: 60,
            }],
            ..Default::default()
        };
        assert_eq!(d.deploy_component_schedules("w", &cfg), 1);
        // a schedule whose component carries no target
        cfg.target = String::new();
        cfg.schedules[0].script = "duty".into();
        assert_eq!(d.deploy_component_schedules("w", &cfg), 1);
    }

    use crate::testkit::{capture_logs, created, drain_logs, ok, reset, FakeWindmill, ENV_LOCK};

    #[test]
    fn workspace_existence_reads_the_body_verbatim() {
        let f = FakeWindmill::start(vec![ok("true")]);
        let d = f.deployer();
        assert!(d.workspace_exists("w1").unwrap());
        assert_eq!(
            f.recorded(),
            vec![("POST".into(), "/api/workspaces/exists".into())]
        );
        assert_eq!(f.bearers(), vec![Some("test-token".into())]);

        let f = FakeWindmill::start(vec![ok("false")]);
        let d = f.deployer();
        assert!(!d.workspace_exists("w1").unwrap());
    }

    #[test]
    fn workspace_is_created_only_when_absent() {
        // absent -> the create call goes out
        let f = FakeWindmill::start(vec![ok("false"), created()]);
        let d = f.deployer();
        d.create_workspace("w1").unwrap();
        assert_eq!(f.recorded()[1].1, "/api/workspaces/create");
        let body = &f.bodies("/api/workspaces/create")[0];
        assert_eq!(body["id"], json!("w1"));
        assert_eq!(body["name"], json!("w1"));

        // present -> no create traffic at all
        let f = FakeWindmill::start(vec![ok("true")]);
        let d = f.deployer();
        d.create_workspace("w1").unwrap();
        assert_eq!(f.recorded().len(), 1);

        // a conflict is tolerated as already-created
        let f = FakeWindmill::start(vec![ok("false"), (409, String::new())]);
        let d = f.deployer();
        assert!(d.create_workspace("w1").is_ok());

        // a server error is refused
        let f = FakeWindmill::start(vec![ok("false"), (500, String::new())]);
        let d = f.deployer();
        assert!(d.create_workspace("w1").is_err());
    }

    #[test]
    fn folder_creation_posts_only_the_top_level() {
        let f = FakeWindmill::start(vec![created()]);
        let d = f.deployer();
        d.create_folder("w1", "f/t1/sub/deep").unwrap();
        assert_eq!(f.recorded()[0].1, "/api/w/w1/folders/create");
        let body = &f.bodies("/folders/create")[0];
        assert_eq!(body["name"], json!("t1"));
    }

    #[test]
    fn existence_checks_follow_body_and_status() {
        // script: 200 true, 200 false, and a non-200 answers false
        let f = FakeWindmill::start(vec![ok("true")]);
        assert!(f.deployer().script_exists("w1", "f/t1/run").unwrap());
        let f = FakeWindmill::start(vec![ok("false")]);
        assert!(!f.deployer().script_exists("w1", "f/t1/run").unwrap());
        let f = FakeWindmill::start(vec![(404, String::new())]);
        assert!(!f.deployer().script_exists("w1", "f/t1/run").unwrap());
        assert_eq!(f.recorded()[0].0, "GET");

        // the flow door answers the same grammar
        let f = FakeWindmill::start(vec![ok("true"), ok("false")]);
        assert!(f.deployer().flow_exists("w1", "f/t1/flow").unwrap());
        assert!(!f.deployer().flow_exists("w1", "f/t1/flow").unwrap());

        // and the schedule door
        let f = FakeWindmill::start(vec![ok("true"), (404, String::new())]);
        assert!(f
            .deployer()
            .schedule_exists("w1", "f/t1/run_schedule")
            .unwrap());
        assert!(!f
            .deployer()
            .schedule_exists("w1", "f/t1/run_schedule")
            .unwrap());
    }

    #[test]
    fn script_deployment_posts_the_declared_shape_and_demands_201() {
        let f = FakeWindmill::start(vec![created()]);
        let d = f.deployer();
        d.deploy_script(
            "w1",
            "f/t1/run",
            "print(42)",
            &ScriptSettings {
                summary: "s".into(),
                description: "d".into(),
                timeout: None,
                tag: None,
            },
            "run.py",
        )
        .unwrap();
        let body = &f.bodies("/scripts/create")[0];
        assert_eq!(body["path"], json!("f/t1/run"));
        assert_eq!(body["content"], json!("print(42)"));
        assert_eq!(body["language"], json!("python3"));
        assert_eq!(body["summary"], json!("s"));
        assert_eq!(body["description"], json!("d"));
        assert!(body.get("timeout").is_none());
        assert!(body.get("tag").is_none());

        // optional fields ride only when declared
        let f = FakeWindmill::start(vec![created()]);
        let d = f.deployer();
        d.deploy_script(
            "w1",
            "f/t1/run",
            "x",
            &ScriptSettings {
                summary: "s".into(),
                description: "d".into(),
                timeout: Some(30),
                tag: Some("T".into()),
            },
            "run.ts",
        )
        .unwrap();
        let body = &f.bodies("/scripts/create")[0];
        assert_eq!(body["language"], json!("nativets"));
        assert_eq!(body["timeout"], json!(30));
        assert_eq!(body["tag"], json!("T"));

        // 201 is the only success — a plain 200 is a refusal
        let f = FakeWindmill::start(vec![ok("")]);
        let d = f.deployer();
        assert!(d
            .deploy_script("w1", "f/t1/run", "x", &ScriptSettings::default(), "run.py")
            .is_err());
    }

    #[test]
    fn flow_deployment_merges_the_declared_settings_into_the_yaml() {
        let yaml = "summary: original\ndescription: origdesc\nsteps: []\n";
        let f = FakeWindmill::start(vec![created()]);
        let d = f.deployer();
        d.deploy_flow(
            "w1",
            "f/t1/flow",
            yaml,
            &FlowSettings {
                summary: "S".into(),
                description: "D".into(),
                deployment_message: Some("M".into()),
            },
        )
        .unwrap();
        let body = &f.bodies("/flows/create")[0];
        assert_eq!(body["path"], json!("f/t1/flow"));
        assert_eq!(body["summary"], json!("S"));
        assert_eq!(body["description"], json!("D"));
        assert_eq!(body["deployment_message"], json!("M"));

        // empty settings blank the fields and carry no message
        let f = FakeWindmill::start(vec![created()]);
        let d = f.deployer();
        d.deploy_flow("w1", "f/t1/flow", yaml, &FlowSettings::default())
            .unwrap();
        let body = &f.bodies("/flows/create")[0];
        assert_eq!(body["summary"], json!(""));
        assert_eq!(body["description"], json!(""));
        assert!(body.get("deployment_message").is_none());

        // 201 or nothing
        let f = FakeWindmill::start(vec![ok("")]);
        assert!(f
            .deployer()
            .deploy_flow("w1", "f/t1/flow", yaml, &FlowSettings::default())
            .is_err());
    }

    #[test]
    fn schedule_deployment_posts_the_declared_trigger() {
        let f = FakeWindmill::start(vec![created()]);
        let d = f.deployer();
        d.deploy_schedule(
            "w1",
            "f/t1/run_schedule",
            "0 * * * * *",
            "f/t1/run",
            "the summary",
        )
        .unwrap();
        let body = &f.bodies("/schedules/create")[0];
        assert_eq!(body["path"], json!("f/t1/run_schedule"));
        assert_eq!(body["schedule"], json!("0 * * * * *"));
        assert_eq!(body["timezone"], json!("UTC"));
        assert_eq!(body["script_path"], json!("f/t1/run"));
        assert_eq!(body["is_flow"], json!(false));
        assert_eq!(body["enabled"], json!(true));
        assert_eq!(body["summary"], json!("the summary"));

        // 200 or 201 carry it; anything else refuses
        let f = FakeWindmill::start(vec![(500, "boom".into())]);
        assert!(f
            .deployer()
            .deploy_schedule("w1", "p", "0 * * * * *", "f/t1/run", "s")
            .is_err());
    }

    #[test]
    fn retries_attempt_the_declared_count_and_no_more() {
        // two refusals, then the door opens: three creates in total
        let f = FakeWindmill::start(vec![
            ok("false"),
            (500, String::new()),
            (500, String::new()),
            created(),
        ]);
        let d = WindmillDeployer::with_base_url(&f.url(), 2, 0);
        d.deploy_script_with_retry("w1", "f/t1/run", "x", &ScriptSettings::default(), "run.py")
            .unwrap();
        assert_eq!(f.recorded().len(), 4);
        assert_eq!(
            f.recorded()
                .iter()
                .filter(|(_, p)| p.contains("scripts/create"))
                .count(),
            3
        );

        // exhausted: exists + exactly max_retries+1 creates, then Err
        let f = FakeWindmill::start(vec![
            ok("false"),
            (500, String::new()),
            (500, String::new()),
            (500, String::new()),
        ]);
        let d = WindmillDeployer::with_base_url(&f.url(), 2, 0);
        assert!(d
            .deploy_script_with_retry("w1", "f/t1/run", "x", &ScriptSettings::default(), "run.py")
            .is_err());
        assert_eq!(
            f.recorded()
                .iter()
                .filter(|(_, p)| p.contains("scripts/create"))
                .count(),
            3
        );

        // already present: one exists check, zero creates
        let f = FakeWindmill::start(vec![ok("true")]);
        let d = WindmillDeployer::with_base_url(&f.url(), 2, 0);
        d.deploy_script_with_retry("w1", "f/t1/run", "x", &ScriptSettings::default(), "run.py")
            .unwrap();
        assert_eq!(f.recorded().len(), 1);
    }

    #[test]
    fn component_schedules_count_failures_exactly() {
        // the happy path: planted script, absent schedule, one create
        let f = FakeWindmill::start(vec![ok("true"), ok("false"), created()]);
        let d = f.deployer();
        let cfg = ComponentConfig {
            name: "comp".into(),
            target: "t1".into(),
            schedules: vec![ScheduleSpec {
                script: "duty".into(),
                every_seconds: 60,
            }],
            ..Default::default()
        };
        assert_eq!(d.deploy_component_schedules("w1", &cfg), 0);
        let body = &f.bodies("/schedules/create")[0];
        assert_eq!(body["path"], json!("f/t1/duty_schedule"));
        assert_eq!(body["schedule"], json!("0 * * * * *"));
        assert_eq!(body["script_path"], json!("f/t1/duty"));

        // a schedule whose script this run did not plant: one failure,
        // and no schedule traffic
        let f = FakeWindmill::start(vec![ok("false")]);
        let d = f.deployer();
        assert_eq!(d.deploy_component_schedules("w1", &cfg), 1);
        assert!(f
            .recorded()
            .iter()
            .all(|(_, p)| !p.contains("schedules/create")));

        // mixed: a bad cadence refuses without traffic, a good one
        // plants — exactly one failure. Both scripts answer planted
        // (two exists checks); only the good one reaches the schedule
        // door, because the cadence refusal spends no HTTP.
        let f = FakeWindmill::start(vec![ok("true"), ok("true"), ok("false"), created()]);
        let d = f.deployer();
        let cfg = ComponentConfig {
            name: "comp".into(),
            target: "t1".into(),
            schedules: vec![
                ScheduleSpec {
                    script: "odd".into(),
                    every_seconds: 90, // neither whole seconds nor whole minutes
                },
                ScheduleSpec {
                    script: "duty".into(),
                    every_seconds: 60,
                },
            ],
            ..Default::default()
        };
        assert_eq!(d.deploy_component_schedules("w1", &cfg), 1);
        assert_eq!(f.bodies("/schedules/create").len(), 1);
    }

    #[test]
    fn seed_workspace_walks_the_declared_component_end_to_end() {
        // one env, two scenarios, one process: the env is global, so
        // both rides share this test's single set_var window — and
        // the lock keeps every other env-window test out of it.
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        env::set_var("WM_TOKEN", "seed-token");

        // ── the happy component: workspace, folder, script, schedule ──
        let root = temp_root("seed-happy");
        let comp = root.join("comp");
        std::fs::create_dir_all(&comp).unwrap();
        std::fs::write(
            comp.join("component.yaml"),
            "name: comp\ntarget: t1\nworkspace: w1\nscripts:\n  - pattern: \"*.py\"\nschedules:\n  - script: duty\n    every_seconds: 60\n",
        )
        .unwrap();
        std::fs::write(comp.join("run.py"), "print(1)\n").unwrap();

        let f = FakeWindmill::start(vec![
            ok("false"), // workspace w1 does not exist
            created(),   // create workspace
            created(),   // create folder
            ok("false"), // planted script does not exist
            created(),   // create script
            ok("true"),  // the scheduled script is planted
            ok("false"), // the schedule does not exist
            created(),   // create schedule
        ]);
        env::set_var("WINDMILL_BASE_URL", f.url());
        let failures =
            seed_workspace(&[comp.to_string_lossy().to_string()], "default_ws", 0, 0).unwrap();
        assert_eq!(failures, 0, "the happy component seeds clean");
        let schedule = &f.bodies("/schedules/create")[0];
        assert_eq!(schedule["path"], json!("f/t1/duty_schedule"));
        assert_eq!(schedule["script_path"], json!("f/t1/duty"));
        // the workspace was asked before it was created
        assert_eq!(f.recorded()[0].1, "/api/workspaces/exists");
        assert_eq!(f.recorded()[1].1, "/api/workspaces/create");
        // the script create carried the file's language
        let script = &f.bodies("/scripts/create")[0];
        assert_eq!(script["language"], json!("python3"));

        // ── the cadence fault: everything plants, the schedule refuses ──
        let root = temp_root("seed-cadence");
        let comp = root.join("comp");
        std::fs::create_dir_all(&comp).unwrap();
        std::fs::write(
            comp.join("component.yaml"),
            "name: comp\ntarget: t1\nworkspace: w1\nscripts:\n  - pattern: \"*.py\"\nschedules:\n  - script: duty\n    every_seconds: 90\n",
        )
        .unwrap();
        std::fs::write(comp.join("run.py"), "print(1)\n").unwrap();

        let f = FakeWindmill::start(vec![
            ok("false"),
            created(),
            created(),
            ok("false"),
            created(),
            ok("true"), // planted, then the cadence refuses — no more traffic
        ]);
        env::set_var("WINDMILL_BASE_URL", f.url());
        let failures =
            seed_workspace(&[comp.to_string_lossy().to_string()], "default_ws", 0, 0).unwrap();
        assert_eq!(
            failures, 1,
            "the unspeakable cadence is exactly one failure"
        );
        assert!(f
            .recorded()
            .iter()
            .all(|(_, p)| !p.contains("schedules/create")));

        env::remove_var("WINDMILL_BASE_URL");
        env::remove_var("WM_TOKEN");
    }

    // ── the sandbox spec grammar: every guard on the declared-script path ──
    // The declared-script door refuses every half-declared shape loud
    // (a failure) and silent (zero HTTP) — an ambiguous spec must
    // never reach the wire.

    fn sb_spec(
        script: Option<&str>,
        pattern: &str,
        sandbox: Option<&str>,
        tag_env: Option<&str>,
    ) -> ScriptSpec {
        ScriptSpec {
            pattern: pattern.into(),
            path: None,
            script: script.map(str::to_string),
            sandbox: sandbox.map(str::to_string),
            tag_env: tag_env.map(str::to_string),
            settings: ScriptSettings::default(),
        }
    }

    #[test]
    fn sandbox_specs_refuse_every_half_declared_shape() {
        let comp = ComponentConfig {
            name: "comp".into(),
            target: "t1".into(),
            ..Default::default()
        };
        std::env::set_var("GODON_SEEDER_TEST_TAG_A", "v1");

        for (label, spec) in [
            (
                "no script name",
                sb_spec(None, "", Some("img"), Some("GODON_SEEDER_TEST_TAG_A")),
            ),
            (
                "empty script name",
                sb_spec(Some(""), "", Some("img"), Some("GODON_SEEDER_TEST_TAG_A")),
            ),
            (
                "script and pattern both set",
                sb_spec(
                    Some("run"),
                    "p/*",
                    Some("img"),
                    Some("GODON_SEEDER_TEST_TAG_A"),
                ),
            ),
            (
                "no sandbox image",
                sb_spec(Some("run"), "", None, Some("GODON_SEEDER_TEST_TAG_A")),
            ),
            (
                "empty sandbox image",
                sb_spec(Some("run"), "", Some(""), Some("GODON_SEEDER_TEST_TAG_A")),
            ),
            ("no tag env", sb_spec(Some("run"), "", Some("img"), None)),
            (
                "empty tag env",
                sb_spec(Some("run"), "", Some("img"), Some("")),
            ),
            ("tag env unset in the process", {
                std::env::remove_var("GODON_SEEDER_TEST_TAG_B");
                sb_spec(
                    Some("run"),
                    "",
                    Some("img"),
                    Some("GODON_SEEDER_TEST_TAG_B"),
                )
            }),
            ("tag env set to whitespace", {
                std::env::set_var("GODON_SEEDER_TEST_TAG_C", "   ");
                sb_spec(
                    Some("run"),
                    "",
                    Some("img"),
                    Some("GODON_SEEDER_TEST_TAG_C"),
                )
            }),
        ] {
            let f = FakeWindmill::start(vec![]);
            assert_eq!(
                f.deployer().deploy_sandbox_script("w1", &comp, &spec),
                1,
                "{label}"
            );
            assert_eq!(f.requests(), 0, "{label}: no HTTP may leave");
        }

        std::env::remove_var("GODON_SEEDER_TEST_TAG_A");
        std::env::remove_var("GODON_SEEDER_TEST_TAG_C");
    }

    #[test]
    fn a_declared_sandbox_script_pins_its_image_ref() {
        std::env::set_var("GODON_SEEDER_TEST_TAG_D", "v1.2.3");
        let comp = ComponentConfig {
            name: "comp".into(),
            target: "t1".into(),
            ..Default::default()
        };
        let spec = sb_spec(
            Some("run"),
            "",
            Some("registry/godon-img"),
            Some("GODON_SEEDER_TEST_TAG_D"),
        );

        let f = FakeWindmill::start(vec![ok("false"), created()]);
        assert_eq!(f.deployer().deploy_sandbox_script("w1", &comp, &spec), 0);
        let body = &f.bodies("/scripts/create")[0];
        assert_eq!(body["path"], json!("f/t1/run"));
        assert_eq!(
            body["content"],
            json!("# sandbox registry/godon-img:v1.2.3\n")
        );
        // the language hint is the stem with .sh — bash on the wire
        assert_eq!(body["language"], json!("bash"));

        // a sha256-prefixed env value pins the digest separator
        std::env::set_var("GODON_SEEDER_TEST_TAG_D", "sha256:abc123");
        let f = FakeWindmill::start(vec![ok("false"), created()]);
        assert_eq!(f.deployer().deploy_sandbox_script("w1", &comp, &spec), 0);
        assert_eq!(
            f.bodies("/scripts/create")[0]["content"],
            json!("# sandbox registry/godon-img@sha256:abc123\n")
        );

        std::env::remove_var("GODON_SEEDER_TEST_TAG_D");
    }

    #[test]
    fn sandbox_routing_opens_on_any_declared_field() {
        // any of the three declared fields routes the spec onto the
        // sandbox path; a spec only pattern/path never touches it
        let tag = "GODON_SEEDER_TEST_TAG_E";
        std::env::set_var(tag, "v1");

        for spec in [
            sb_spec(Some("run"), "", None, None),
            sb_spec(None, "", Some("img"), None),
            sb_spec(None, "", None, Some(tag)),
        ] {
            let f = FakeWindmill::start(vec![]);
            let comp = ComponentConfig {
                name: "comp".into(),
                target: String::new(), // no folder traffic: only the verdict counts
                scripts: vec![spec],
                ..Default::default()
            };
            assert_eq!(
                f.deployer()
                    .deploy_component_scripts("w1", &comp, Path::new("/nonexistent")),
                1
            );
            assert_eq!(f.requests(), 0);
        }

        // and the fully-declared spec spends its deploy on the wire
        let f = FakeWindmill::start(vec![ok("false"), created()]);
        let comp = ComponentConfig {
            name: "comp".into(),
            target: String::new(),
            scripts: vec![sb_spec(Some("run"), "", Some("img"), Some(tag))],
            ..Default::default()
        };
        assert_eq!(
            f.deployer()
                .deploy_component_scripts("w1", &comp, Path::new("/nonexistent")),
            0
        );
        assert_eq!(f.bodies("/scripts/create").len(), 1);

        std::env::remove_var(tag);
    }

    // ── the pattern/path grammar on the plain script side ──

    #[test]
    fn a_directory_named_by_a_pattern_is_not_a_file() {
        let root = temp_root("pat-dir");
        std::fs::create_dir_all(root.join("d")).unwrap();
        std::fs::write(root.join("run.py"), "print(1)\n").unwrap();

        // the finder itself: a pattern naming a directory matches nothing
        assert!(find_files_by_pattern(&root, "d").is_empty());

        // and through the component door: zero failures, zero traffic
        let f = FakeWindmill::start(vec![]);
        let comp = ComponentConfig {
            name: "comp".into(),
            target: String::new(),
            scripts: vec![sb_spec(None, "d", None, None)],
            ..Default::default()
        };
        assert_eq!(f.deployer().deploy_component_scripts("w1", &comp, &root), 0);
        assert_eq!(f.requests(), 0);
    }

    #[test]
    fn an_empty_path_falls_back_to_the_base_dir_search() {
        let root = temp_root("pat-emptypath");
        std::fs::write(root.join("run.py"), "print(1)\n").unwrap();

        // pattern + empty path: the base-dir search still finds the file
        let f = FakeWindmill::start(vec![ok("false"), created()]);
        let comp = ComponentConfig {
            name: "comp".into(),
            target: String::new(),
            scripts: vec![ScriptSpec {
                pattern: "*.py".into(),
                path: Some(String::new()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(f.deployer().deploy_component_scripts("w1", &comp, &root), 0);
        assert_eq!(f.bodies("/scripts/create").len(), 1);

        // a path of only the empty string plants nothing and fails nothing
        let f = FakeWindmill::start(vec![]);
        let comp = ComponentConfig {
            name: "comp".into(),
            target: String::new(),
            scripts: vec![ScriptSpec {
                path: Some(String::new()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(f.deployer().deploy_component_scripts("w1", &comp, &root), 0);
        assert_eq!(f.requests(), 0);
    }

    // ── the flows side of the same grammar ──

    #[test]
    fn flows_pin_their_pattern_and_path_edges() {
        let root = temp_root("flows-edges");
        std::fs::create_dir_all(root.join("d")).unwrap();
        std::fs::write(root.join("flow.yaml"), "summary: s\nsteps: []\n").unwrap();

        // happy: a declared path deploys with the folder spoken for
        let f = FakeWindmill::start(vec![created(), ok("false"), created()]);
        let comp = ComponentConfig {
            name: "comp".into(),
            target: "t1".into(),
            flows: Some(vec![FlowSpec {
                path: Some("flow.yaml".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(f.deployer().deploy_component_flows("w1", &comp, &root), 0);
        assert_eq!(f.bodies("/flows/create").len(), 1);
        assert_eq!(f.recorded()[0].1, "/api/w/w1/folders/create");

        // no target: the folder door stays closed, the flow still plants
        let f = FakeWindmill::start(vec![ok("false"), created()]);
        let comp = ComponentConfig {
            name: "comp".into(),
            target: String::new(),
            flows: Some(vec![FlowSpec {
                path: Some("flow.yaml".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(f.deployer().deploy_component_flows("w1", &comp, &root), 0);
        assert!(f
            .recorded()
            .iter()
            .all(|(_, p)| !p.contains("folders/create")));

        // empty pattern + declared path: the path branch carries it
        let f = FakeWindmill::start(vec![ok("false"), created()]);
        let comp = ComponentConfig {
            name: "comp".into(),
            target: String::new(),
            flows: Some(vec![FlowSpec {
                pattern: String::new(),
                path: Some("flow.yaml".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(f.deployer().deploy_component_flows("w1", &comp, &root), 0);
        assert_eq!(f.bodies("/flows/create").len(), 1);

        // a pattern naming a directory plants nothing, fails nothing
        let f = FakeWindmill::start(vec![]);
        let comp = ComponentConfig {
            name: "comp".into(),
            target: String::new(),
            flows: Some(vec![FlowSpec {
                pattern: "d".into(),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(f.deployer().deploy_component_flows("w1", &comp, &root), 0);
        assert_eq!(f.requests(), 0);

        // pattern + empty path: no fallthrough on the flows side —
        // an empty path with a pattern plants NOTHING here
        let f = FakeWindmill::start(vec![]);
        let comp = ComponentConfig {
            name: "comp".into(),
            target: String::new(),
            flows: Some(vec![FlowSpec {
                pattern: "*.yaml".into(),
                path: Some(String::new()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(f.deployer().deploy_component_flows("w1", &comp, &root), 0);
        assert_eq!(f.requests(), 0);

        // a path of only the empty string: nothing to read, no failure
        let f = FakeWindmill::start(vec![]);
        let comp = ComponentConfig {
            name: "comp".into(),
            target: String::new(),
            flows: Some(vec![FlowSpec {
                path: Some(String::new()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(f.deployer().deploy_component_flows("w1", &comp, &root), 0);
        assert_eq!(f.requests(), 0);
    }

    #[test]
    fn flow_failures_count_exactly_one_each() {
        let root = temp_root("flows-fail");
        std::fs::write(root.join("flow.yaml"), "summary: s\nsteps: []\n").unwrap();

        // the create door refusing: one failure for the flow
        let f = FakeWindmill::start(vec![ok("false"), (500, String::new())]);
        let comp = ComponentConfig {
            name: "comp".into(),
            target: String::new(),
            flows: Some(vec![FlowSpec {
                path: Some("flow.yaml".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(f.deployer().deploy_component_flows("w1", &comp, &root), 1);

        // a path naming a directory: the read fails, one failure
        std::fs::create_dir_all(root.join("adir")).unwrap();
        let f = FakeWindmill::start(vec![]);
        let comp = ComponentConfig {
            name: "comp".into(),
            target: String::new(),
            flows: Some(vec![FlowSpec {
                path: Some("adir".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(f.deployer().deploy_component_flows("w1", &comp, &root), 1);
    }

    // ── the retry doors' attempt arithmetic ──

    #[test]
    fn retry_doors_spend_attempts_on_the_wire_then_stop() {
        // flow: absent, refused once, then planted — two create attempts
        let f = FakeWindmill::start(vec![ok("false"), (500, String::new()), created()]);
        let d = WindmillDeployer::with_base_url(&f.url(), 1, 0);
        d.deploy_flow_with_retry("w1", "f/t1/flow", "summary: s\n", &FlowSettings::default())
            .unwrap();
        assert_eq!(
            f.recorded()
                .iter()
                .filter(|(_, p)| p.contains("flows/create"))
                .count(),
            2
        );

        // a flow already present: one check, zero creates
        let f = FakeWindmill::start(vec![ok("true")]);
        let d = WindmillDeployer::with_base_url(&f.url(), 1, 0);
        d.deploy_flow_with_retry("w1", "f/t1/flow", "summary: s\n", &FlowSettings::default())
            .unwrap();
        assert_eq!(f.requests(), 1);

        // schedule: the same grammar on its own door
        let f = FakeWindmill::start(vec![ok("false"), (500, String::new()), created()]);
        let d = WindmillDeployer::with_base_url(&f.url(), 1, 0);
        d.deploy_schedule_with_retry("w1", "f/t1/run_schedule", "0 * * * * *", "f/t1/run", "s")
            .unwrap();
        assert_eq!(
            f.recorded()
                .iter()
                .filter(|(_, p)| p.contains("schedules/create"))
                .count(),
            2
        );
    }

    #[test]
    fn schedule_counters_spend_exactly_one_on_each_error_branch() {
        let comp = ComponentConfig {
            name: "comp".into(),
            target: "t1".into(),
            schedules: vec![ScheduleSpec {
                script: "duty".into(),
                every_seconds: 60,
            }],
            ..Default::default()
        };

        // the existence check itself failing (connection reset): one failure
        let f = FakeWindmill::start(vec![reset()]);
        assert_eq!(f.deployer().deploy_component_schedules("w1", &comp), 1);

        // the schedule create door refusing: one failure
        let f = FakeWindmill::start(vec![ok("true"), ok("false"), (500, String::new())]);
        assert_eq!(f.deployer().deploy_component_schedules("w1", &comp), 1);
    }

    // ── seed_workspace's override and counting paths ──

    fn write_component(root: &std::path::Path, yaml: &str, files: &[(&str, &str)]) {
        let comp = root.join("comp");
        std::fs::create_dir_all(&comp).unwrap();
        std::fs::write(comp.join("component.yaml"), yaml).unwrap();
        for (name, body) in files {
            std::fs::write(comp.join(name), body).unwrap();
        }
    }

    #[test]
    fn seed_workspace_counts_its_verdicts_and_honors_the_default() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        env::set_var("WM_TOKEN", "seed-token2");
        capture_logs();

        // no workspace declared: the DEFAULT workspace is the one created
        let root = temp_root("seed-default");
        write_component(
            &root,
            "name: comp\ntarget: t1\nscripts:\n  - pattern: \"*.py\"\n",
            &[("run.py", "print(1)\n")],
        );
        let f = FakeWindmill::start(vec![
            ok("false"), // default_ws does not exist
            created(),   // create default_ws
            created(),   // create folder t1
            ok("false"), // script absent
            created(),   // create script
        ]);
        env::set_var("WINDMILL_BASE_URL", f.url());
        let failures = seed_workspace(
            &[root.join("comp").to_string_lossy().to_string()],
            "default_ws",
            0,
            0,
        )
        .unwrap();
        assert_eq!(failures, 0);
        assert_eq!(
            f.bodies("/api/workspaces/create")[0]["id"],
            json!("default_ws")
        );
        let lines = drain_logs();
        assert!(
            lines.iter().any(|l| l.contains("completed successfully")),
            "the clean verdict speaks its line"
        );
        assert!(
            !lines.iter().any(|l| l.contains("completed with")),
            "no failure verdict on a clean run"
        );

        // a script create refusing: exactly one failure, said aloud
        let root = temp_root("seed-scriptfail");
        write_component(
            &root,
            "name: comp\ntarget: t1\nscripts:\n  - pattern: \"*.py\"\n",
            &[("run.py", "print(1)\n")],
        );
        let f = FakeWindmill::start(vec![
            ok("false"),
            created(),
            created(),
            ok("false"),
            (500, String::new()),
        ]);
        env::set_var("WINDMILL_BASE_URL", f.url());
        let failures = seed_workspace(
            &[root.join("comp").to_string_lossy().to_string()],
            "default_ws",
            0,
            0,
        )
        .unwrap();
        assert_eq!(failures, 1);
        let lines = drain_logs();
        assert!(lines
            .iter()
            .any(|l| l.contains("completed with 1 failures")));

        // empty flows: no flow walk, no folder traffic beyond the script's
        let root = temp_root("seed-emptyflows");
        write_component(&root, "name: comp\ntarget: t1\nflows: []\n", &[]);
        let f = FakeWindmill::start(vec![ok("false"), created()]);
        env::set_var("WINDMILL_BASE_URL", f.url());
        let failures = seed_workspace(
            &[root.join("comp").to_string_lossy().to_string()],
            "default_ws",
            0,
            0,
        )
        .unwrap();
        assert_eq!(failures, 0);
        assert!(f
            .recorded()
            .iter()
            .all(|(_, p)| !p.contains("folders/create")));

        // a flow create refusing: exactly one failure
        let root = temp_root("seed-flowfail");
        write_component(
            &root,
            "name: comp\ntarget: t1\nflows:\n  - path: flow.yaml\n",
            &[("flow.yaml", "summary: s\nsteps: []\n")],
        );
        let f = FakeWindmill::start(vec![
            ok("false"), // workspace
            created(),
            created(),   // folder
            ok("false"), // flow absent
            (500, String::new()),
        ]);
        env::set_var("WINDMILL_BASE_URL", f.url());
        let failures = seed_workspace(
            &[root.join("comp").to_string_lossy().to_string()],
            "default_ws",
            0,
            0,
        )
        .unwrap();
        assert_eq!(failures, 1);

        env::remove_var("WINDMILL_BASE_URL");
        env::remove_var("WM_TOKEN");
    }

    // ── create_folder's status arms speak only in log lines ──

    #[test]
    fn folder_statuses_speak_their_own_lines() {
        capture_logs();

        let f = FakeWindmill::start(vec![created()]);
        drain_logs();
        f.deployer().create_folder("w1", "f/t1/sub").unwrap();
        let lines = drain_logs();
        assert!(lines
            .iter()
            .any(|l| l.contains("Successfully created folder")));

        let f = FakeWindmill::start(vec![(409, String::new())]);
        drain_logs();
        f.deployer().create_folder("w1", "f/t1/sub").unwrap();
        let lines = drain_logs();
        assert!(lines.iter().any(|l| l.contains("Folder already exists")));

        // any other status: neither line — the door stays Ok-silent
        let f = FakeWindmill::start(vec![(500, String::new())]);
        drain_logs();
        f.deployer().create_folder("w1", "f/t1/sub").unwrap();
        let lines = drain_logs();
        assert!(
            !lines.iter().any(|l| l.contains("Folder already exists")
                || l.contains("Successfully created folder")),
            "a server error speaks neither of those lines: {lines:?}"
        );
    }
}
