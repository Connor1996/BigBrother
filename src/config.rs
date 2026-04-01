use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub github: GitHubConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub state_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubConfig {
    #[serde(default)]
    pub api_token: Option<String>,
    #[serde(default = "default_github_api_base_url")]
    pub api_base_url: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default = "default_max_prs")]
    pub max_prs: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    #[serde(default = "default_max_concurrent_runs")]
    pub max_concurrent_runs: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceConfig {
    #[serde(default = "default_workspace_root")]
    pub root: String,
    #[serde(default)]
    pub repo_map: BTreeMap<String, String>,
    #[serde(default)]
    pub git_transport: GitTransport,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GitTransport {
    #[default]
    Ssh,
    Https,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "default_agent_command")]
    pub command: String,
    #[serde(default = "default_agent_args")]
    pub args: Vec<String>,
    #[serde(default)]
    pub dangerously_bypass_approvals_and_sandbox: bool,
    #[serde(default)]
    pub additional_instructions: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_refresh_hz")]
    pub refresh_hz: u64,
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub github: ResolvedGitHubConfig,
    pub daemon: DaemonConfig,
    pub workspace: ResolvedWorkspaceConfig,
    pub agent: AgentConfig,
    pub ui: UiConfig,
    pub state_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ResolvedGitHubConfig {
    pub api_token: String,
    pub api_base_url: String,
    pub author: Option<String>,
    pub query: Option<String>,
    pub max_prs: usize,
}

#[derive(Debug, Clone)]
pub struct ResolvedWorkspaceConfig {
    pub root: PathBuf,
    pub repo_map: BTreeMap<String, PathBuf>,
    pub git_transport: GitTransport,
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<ResolvedConfig> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        let parsed: AppConfig = toml::from_str(&raw)
            .with_context(|| format!("failed to parse TOML config at {}", path.display()))?;
        parsed.resolve(path)
    }

    fn resolve(self, path: &Path) -> Result<ResolvedConfig> {
        let config_dir = path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let api_token = resolve_secret(self.github.api_token, &["GITHUB_TOKEN", "GH_TOKEN"])?
            .trim()
            .to_owned();

        if api_token.is_empty() {
            return Err(anyhow!("GitHub API token cannot be empty"));
        }

        let state_path = self
            .state_path
            .as_deref()
            .map(|value| resolve_path(value, &config_dir))
            .unwrap_or_else(|| config_dir.join("symphony-rs-state.json"));

        Ok(ResolvedConfig {
            github: ResolvedGitHubConfig {
                api_token,
                api_base_url: resolve_literal(self.github.api_base_url, None),
                author: self.github.author.map(|value| resolve_literal(value, None)),
                query: self.github.query.map(|value| resolve_literal(value, None)),
                max_prs: self.github.max_prs.max(1),
            },
            daemon: self.daemon,
            workspace: ResolvedWorkspaceConfig {
                root: resolve_path(&self.workspace.root, &config_dir),
                repo_map: self
                    .workspace
                    .repo_map
                    .into_iter()
                    .map(|(repo, path)| (repo, resolve_path(&path, &config_dir)))
                    .collect(),
                git_transport: self.workspace.git_transport,
            },
            agent: AgentConfig {
                command: resolve_literal(self.agent.command, None),
                args: self
                    .agent
                    .args
                    .into_iter()
                    .map(|value| resolve_literal(value, None))
                    .collect(),
                dangerously_bypass_approvals_and_sandbox: self
                    .agent
                    .dangerously_bypass_approvals_and_sandbox,
                additional_instructions: self
                    .agent
                    .additional_instructions
                    .map(|value| resolve_literal(value, None)),
            },
            ui: self.ui,
            state_path,
        })
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: default_poll_interval_secs(),
            max_concurrent_runs: default_max_concurrent_runs(),
        }
    }
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            root: default_workspace_root(),
            repo_map: BTreeMap::new(),
            git_transport: GitTransport::default(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            command: default_agent_command(),
            args: default_agent_args(),
            dangerously_bypass_approvals_and_sandbox: false,
            additional_instructions: None,
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            refresh_hz: default_refresh_hz(),
        }
    }
}

pub fn build_search_query(config: &ResolvedGitHubConfig, author: &str) -> String {
    match &config.query {
        Some(query) => query.replace("{author}", author),
        None => format!("is:pr is:open archived:false author:{author}"),
    }
}

pub fn build_review_request_query(reviewer: &str) -> String {
    format!("is:pr is:open archived:false review-requested:{reviewer}")
}

fn resolve_secret(value: Option<String>, fallbacks: &[&str]) -> Result<String> {
    if let Some(value) = value {
        if value.trim().starts_with('$') {
            resolve_env_reference(&value)
                .ok_or_else(|| anyhow!("failed to resolve secret value from {}", value))
        } else {
            Ok(resolve_literal(value, None))
        }
    } else {
        fallbacks
            .iter()
            .find_map(|key| std::env::var(key).ok())
            .ok_or_else(|| {
                anyhow!("missing GitHub API token; set github.api_token or export GITHUB_TOKEN")
            })
    }
}

fn resolve_literal(value: String, default: Option<String>) -> String {
    match resolve_env_reference(&value) {
        Some(resolved) => resolved,
        None => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                default.unwrap_or_default()
            } else {
                trimmed.to_owned()
            }
        }
    }
}

fn resolve_path(value: &str, base_dir: &Path) -> PathBuf {
    let resolved = resolve_env_reference(value).unwrap_or_else(|| value.trim().to_owned());
    let path = PathBuf::from(resolved);

    if path.is_absolute() {
        path
    } else {
        base_dir.join(path)
    }
}

fn resolve_env_reference(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let env_name = trimmed.strip_prefix('$')?;

    if env_name.is_empty() {
        return None;
    }

    std::env::var(env_name).ok()
}

fn default_github_api_base_url() -> String {
    "https://api.github.com".to_owned()
}

fn default_max_prs() -> usize {
    25
}

fn default_poll_interval_secs() -> u64 {
    60
}

fn default_max_concurrent_runs() -> usize {
    2
}

fn default_workspace_root() -> String {
    "..".to_owned()
}

fn default_agent_command() -> String {
    "codex".to_owned()
}

fn default_agent_args() -> Vec<String> {
    vec![
        "exec".to_owned(),
        "--model".to_owned(),
        "gpt-5.3-codex".to_owned(),
        "-".to_owned(),
    ]
}

fn default_refresh_hz() -> u64 {
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_search_query_uses_default_when_not_configured() {
        let config = ResolvedGitHubConfig {
            api_token: "token".to_owned(),
            api_base_url: default_github_api_base_url(),
            author: None,
            query: None,
            max_prs: 25,
        };

        assert_eq!(
            build_search_query(&config, "connor"),
            "is:pr is:open archived:false author:connor"
        );
    }

    #[test]
    fn build_search_query_interpolates_author_placeholder() {
        let config = ResolvedGitHubConfig {
            api_token: "token".to_owned(),
            api_base_url: default_github_api_base_url(),
            author: None,
            query: Some("is:pr is:open author:{author} label:symphony".to_owned()),
            max_prs: 25,
        };

        assert_eq!(
            build_search_query(&config, "connor"),
            "is:pr is:open author:connor label:symphony"
        );
    }

    #[test]
    fn load_resolves_workspace_repo_map_relative_to_config_dir() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("symphony-rs-config-{unique}"));
        std::fs::create_dir_all(dir.join("repos/custom")).expect("temp config dir should create");
        let config_path = dir.join("symphony-rs.toml");
        std::fs::write(
            &config_path,
            r#"
[github]
api_token = "token"

[workspace]
root = "../repos"
repo_map = { "tidbcloud/tidb-cse" = "./repos/custom/tidb-cse-local" }
"#,
        )
        .expect("config fixture should write");

        let resolved = AppConfig::load(&config_path).expect("config should load");
        assert_eq!(resolved.workspace.root, dir.join("../repos"));
        assert_eq!(
            resolved
                .workspace
                .repo_map
                .get("tidbcloud/tidb-cse")
                .expect("repo map entry should exist"),
            &dir.join("repos/custom/tidb-cse-local")
        );
    }

    #[test]
    fn load_preserves_explicit_agent_full_access_flag() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("symphony-rs-config-{unique}"));
        std::fs::create_dir_all(&dir).expect("temp config dir should create");
        let config_path = dir.join("symphony-rs.toml");
        std::fs::write(
            &config_path,
            r#"
[github]
api_token = "token"

[agent]
dangerously_bypass_approvals_and_sandbox = true
"#,
        )
        .expect("config fixture should write");

        let resolved = AppConfig::load(&config_path).expect("config should load");

        assert!(resolved.agent.dangerously_bypass_approvals_and_sandbox);
    }
}
