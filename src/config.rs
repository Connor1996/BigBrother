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
    pub agent: RawAgentConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub notifications: NotificationsConfig,
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
pub struct RawAgentConfig {
    #[serde(default = "default_agent_command")]
    pub command: String,
    #[serde(default = "default_agent_args")]
    pub args: Vec<String>,
    #[serde(default = "default_agent_model_reasoning_effort")]
    pub model_reasoning_effort: String,
    #[serde(default)]
    pub dangerously_bypass_approvals_and_sandbox: bool,
    #[serde(default)]
    pub additional_instructions: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub command: String,
    pub args: Vec<String>,
    pub model_reasoning_effort: String,
    pub dangerously_bypass_approvals_and_sandbox: bool,
    pub additional_instructions: Option<String>,
    pub prompts: AgentPromptTemplates,
}

#[derive(Debug, Clone)]
pub struct AgentPromptTemplates {
    pub actionable: String,
    pub deep_review: String,
    pub ci_failure_rules: String,
    pub workspace_ready: String,
    pub resumed_conflict: String,
    pub deep_review_artifact: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_refresh_hz")]
    pub refresh_hz: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NotificationsConfig {
    #[serde(default)]
    pub feishu: Option<FeishuNotificationsConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeishuNotificationsConfig {
    pub app_id: String,
    pub app_secret: String,
    pub receive_id: String,
    #[serde(default)]
    pub receive_id_type: FeishuReceiveIdType,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default = "default_notification_timeout_secs")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeishuReceiveIdType {
    #[default]
    Email,
    OpenId,
    UserId,
    UnionId,
    ChatId,
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub github: ResolvedGitHubConfig,
    pub daemon: DaemonConfig,
    pub workspace: ResolvedWorkspaceConfig,
    pub agent: AgentConfig,
    pub ui: UiConfig,
    pub notifications: ResolvedNotificationsConfig,
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

#[derive(Debug, Clone, Default)]
pub struct ResolvedNotificationsConfig {
    pub feishu: Option<ResolvedFeishuNotificationsConfig>,
}

#[derive(Debug, Clone)]
pub struct ResolvedFeishuNotificationsConfig {
    pub app_id: String,
    pub app_secret: String,
    pub receive_id: String,
    pub receive_id_type: FeishuReceiveIdType,
    pub label: String,
    pub timeout_secs: u64,
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
            .unwrap_or_else(|| config_dir.join("bigbrother-state.json"));
        let notifications = resolve_notifications(self.notifications, &config_dir)?;

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
            agent: resolve_agent_config(self.agent)?,
            ui: self.ui,
            notifications,
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

impl Default for RawAgentConfig {
    fn default() -> Self {
        Self {
            command: default_agent_command(),
            args: default_agent_args(),
            model_reasoning_effort: default_agent_model_reasoning_effort(),
            dangerously_bypass_approvals_and_sandbox: false,
            additional_instructions: None,
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            command: default_agent_command(),
            args: default_agent_args(),
            model_reasoning_effort: default_agent_model_reasoning_effort(),
            dangerously_bypass_approvals_and_sandbox: false,
            additional_instructions: None,
            prompts: AgentPromptTemplates::default(),
        }
    }
}

impl Default for AgentPromptTemplates {
    fn default() -> Self {
        Self {
            actionable: DEFAULT_ACTIONABLE_PROMPT_TEMPLATE.to_owned(),
            deep_review: DEFAULT_DEEP_REVIEW_PROMPT_TEMPLATE.to_owned(),
            ci_failure_rules: DEFAULT_CI_FAILURE_RULES_TEMPLATE.to_owned(),
            workspace_ready: DEFAULT_WORKSPACE_READY_TEMPLATE.to_owned(),
            resumed_conflict: DEFAULT_RESUMED_CONFLICT_TEMPLATE.to_owned(),
            deep_review_artifact: DEFAULT_DEEP_REVIEW_ARTIFACT_TEMPLATE.to_owned(),
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

impl FeishuReceiveIdType {
    pub fn as_api_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::OpenId => "open_id",
            Self::UserId => "user_id",
            Self::UnionId => "union_id",
            Self::ChatId => "chat_id",
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

fn resolve_required_secret(value: Option<String>, field_name: &str) -> Result<String> {
    let value = value.ok_or_else(|| anyhow!("{field_name} is required"))?;
    if value.trim().starts_with('$') {
        resolve_env_reference(&value)
            .ok_or_else(|| anyhow!("failed to resolve secret value from {}", value))
    } else {
        Ok(resolve_literal(value, None))
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

fn default_agent_model_reasoning_effort() -> String {
    "xhigh".to_owned()
}

const DEFAULT_ACTIONABLE_PROMPT_TEMPLATE: &str = include_str!("../prompts/actionable.md");
const DEFAULT_DEEP_REVIEW_PROMPT_TEMPLATE: &str = include_str!("../prompts/deep_review.md");
const DEFAULT_CI_FAILURE_RULES_TEMPLATE: &str = include_str!("../prompts/ci_failure_rules.md");
const DEFAULT_WORKSPACE_READY_TEMPLATE: &str = include_str!("../prompts/workspace_ready.md");
const DEFAULT_RESUMED_CONFLICT_TEMPLATE: &str = include_str!("../prompts/resumed_conflict.md");
const DEFAULT_DEEP_REVIEW_ARTIFACT_TEMPLATE: &str =
    include_str!("../prompts/deep_review_artifact.md");
const PROMPTS_DIR: &str = "prompts";
const ACTIONABLE_PROMPT_FILE: &str = "actionable.md";
const DEEP_REVIEW_PROMPT_FILE: &str = "deep_review.md";
const CI_FAILURE_RULES_FILE: &str = "ci_failure_rules.md";
const WORKSPACE_READY_FILE: &str = "workspace_ready.md";
const RESUMED_CONFLICT_FILE: &str = "resumed_conflict.md";
const DEEP_REVIEW_ARTIFACT_FILE: &str = "deep_review_artifact.md";

fn default_refresh_hz() -> u64 {
    2
}

fn default_notification_timeout_secs() -> u64 {
    10
}

fn default_feishu_notification_label() -> String {
    "bigbrother".to_owned()
}

fn resolve_agent_config(raw: RawAgentConfig) -> Result<AgentConfig> {
    Ok(AgentConfig {
        command: resolve_literal(raw.command, None),
        args: raw
            .args
            .into_iter()
            .map(|value| resolve_literal(value, None))
            .collect(),
        model_reasoning_effort: resolve_literal(raw.model_reasoning_effort, None),
        dangerously_bypass_approvals_and_sandbox: raw.dangerously_bypass_approvals_and_sandbox,
        additional_instructions: raw
            .additional_instructions
            .map(|value| resolve_literal(value, None)),
        prompts: resolve_agent_prompt_templates()?,
    })
}

fn resolve_agent_prompt_templates() -> Result<AgentPromptTemplates> {
    load_agent_prompt_templates_from_dir(&repo_prompts_dir())
}

fn load_agent_prompt_templates_from_dir(prompt_dir: &Path) -> Result<AgentPromptTemplates> {
    Ok(AgentPromptTemplates {
        actionable: load_repo_prompt_template(prompt_dir, ACTIONABLE_PROMPT_FILE)?,
        deep_review: load_repo_prompt_template(prompt_dir, DEEP_REVIEW_PROMPT_FILE)?,
        ci_failure_rules: load_repo_prompt_template(prompt_dir, CI_FAILURE_RULES_FILE)?,
        workspace_ready: load_repo_prompt_template(prompt_dir, WORKSPACE_READY_FILE)?,
        resumed_conflict: load_repo_prompt_template(prompt_dir, RESUMED_CONFLICT_FILE)?,
        deep_review_artifact: load_repo_prompt_template(prompt_dir, DEEP_REVIEW_ARTIFACT_FILE)?,
    })
}

fn repo_prompts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(PROMPTS_DIR)
}

fn load_repo_prompt_template(prompt_dir: &Path, file_name: &str) -> Result<String> {
    let path = prompt_dir.join(file_name);
    fs::read_to_string(&path)
        .with_context(|| format!("failed to read prompt template at {}", path.display()))
}

fn resolve_notifications(
    config: NotificationsConfig,
    _config_dir: &Path,
) -> Result<ResolvedNotificationsConfig> {
    let feishu = match config.feishu {
        Some(raw) => {
            let label = raw
                .label
                .map(|value| resolve_literal(value, Some(default_feishu_notification_label())))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(default_feishu_notification_label);
            let timeout_secs = raw.timeout_secs.max(1);
            let app_id = resolve_literal(raw.app_id, None);
            if app_id.is_empty() {
                return Err(anyhow!("notifications.feishu.app_id cannot be empty"));
            }

            let app_secret =
                resolve_required_secret(Some(raw.app_secret), "notifications.feishu.app_secret")?
                    .trim()
                    .to_owned();
            if app_secret.is_empty() {
                return Err(anyhow!("notifications.feishu.app_secret cannot be empty"));
            }

            let receive_id = resolve_literal(raw.receive_id, None);
            if receive_id.is_empty() {
                return Err(anyhow!("notifications.feishu.receive_id cannot be empty"));
            }

            Some(ResolvedFeishuNotificationsConfig {
                app_id,
                app_secret,
                receive_id,
                receive_id_type: raw.receive_id_type,
                label,
                timeout_secs,
            })
        }
        None => None,
    };

    Ok(ResolvedNotificationsConfig { feishu })
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
            query: Some("is:pr is:open author:{author} label:bigbrother".to_owned()),
            max_prs: 25,
        };

        assert_eq!(
            build_search_query(&config, "connor"),
            "is:pr is:open author:connor label:bigbrother"
        );
    }

    #[test]
    fn load_resolves_workspace_repo_map_relative_to_config_dir() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("bigbrother-config-{unique}"));
        std::fs::create_dir_all(dir.join("repos/custom")).expect("temp config dir should create");
        let config_path = dir.join("bigbrother.toml");
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
        let dir = std::env::temp_dir().join(format!("bigbrother-config-{unique}"));
        std::fs::create_dir_all(&dir).expect("temp config dir should create");
        let config_path = dir.join("bigbrother.toml");
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

    #[test]
    fn load_defaults_agent_reasoning_effort_to_xhigh() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("bigbrother-config-{unique}"));
        std::fs::create_dir_all(&dir).expect("temp config dir should create");
        let config_path = dir.join("bigbrother.toml");
        std::fs::write(
            &config_path,
            r#"
[github]
api_token = "token"
"#,
        )
        .expect("config fixture should write");

        let resolved = AppConfig::load(&config_path).expect("config should load");

        assert_eq!(resolved.agent.model_reasoning_effort, "xhigh");
    }

    #[test]
    fn load_preserves_explicit_agent_reasoning_effort() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("bigbrother-config-{unique}"));
        std::fs::create_dir_all(&dir).expect("temp config dir should create");
        let config_path = dir.join("bigbrother.toml");
        std::fs::write(
            &config_path,
            r#"
[github]
api_token = "token"

[agent]
model_reasoning_effort = "high"
"#,
        )
        .expect("config fixture should write");

        let resolved = AppConfig::load(&config_path).expect("config should load");

        assert_eq!(resolved.agent.model_reasoning_effort, "high");
    }

    #[test]
    fn load_agent_prompt_templates_from_dir_reads_repo_style_markdown_files() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("bigbrother-config-{unique}"));
        let prompts_dir = dir.join("prompts");
        std::fs::create_dir_all(&prompts_dir).expect("prompt fixture dir should create");
        std::fs::write(
            prompts_dir.join("actionable.md"),
            "custom actionable for {{repo}}",
        )
        .expect("custom actionable prompt should write");
        std::fs::write(
            prompts_dir.join("deep_review_artifact.md"),
            "artifact => {{artifact_path}}",
        )
        .expect("custom deep review artifact prompt should write");
        std::fs::write(
            prompts_dir.join("deep_review.md"),
            "deep review for {{repo}}",
        )
        .expect("deep review prompt should write");
        std::fs::write(prompts_dir.join("ci_failure_rules.md"), "- retest guidance")
            .expect("ci failure rules should write");
        std::fs::write(
            prompts_dir.join("workspace_ready.md"),
            "- workspace ready guidance",
        )
        .expect("workspace ready prompt should write");
        std::fs::write(
            prompts_dir.join("resumed_conflict.md"),
            "- resumed conflict guidance",
        )
        .expect("resumed conflict prompt should write");

        let resolved = load_agent_prompt_templates_from_dir(&prompts_dir)
            .expect("prompt templates should load");

        assert_eq!(resolved.actionable, "custom actionable for {{repo}}");
        assert_eq!(
            resolved.deep_review_artifact,
            "artifact => {{artifact_path}}"
        );
        assert_eq!(resolved.deep_review, "deep review for {{repo}}");
    }

    #[test]
    fn load_resolves_feishu_notification_settings() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("bigbrother-config-{unique}"));
        std::fs::create_dir_all(&dir).expect("temp config dir should create");
        let config_path = dir.join("bigbrother.toml");
        std::fs::write(
            &config_path,
            r#"
[github]
api_token = "token"

[notifications.feishu]
app_id = "cli_test"
app_secret = "secret"
receive_id = "you@example.com"
receive_id_type = "email"
label = "connor-mbp"
timeout_secs = 9
"#,
        )
        .expect("config fixture should write");

        let resolved = AppConfig::load(&config_path).expect("config should load");
        let feishu = resolved
            .notifications
            .feishu
            .expect("feishu notifications should resolve");

        assert_eq!(feishu.app_id, "cli_test");
        assert_eq!(feishu.app_secret, "secret");
        assert_eq!(feishu.receive_id, "you@example.com");
        assert_eq!(feishu.receive_id_type, FeishuReceiveIdType::Email);
        assert_eq!(feishu.label, "connor-mbp");
        assert_eq!(feishu.timeout_secs, 9);
    }
}
