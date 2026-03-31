use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use tokio::{io::AsyncWriteExt, process::Command};

use crate::{
    config::{AgentConfig, GitTransport, ResolvedWorkspaceConfig},
    model::{AttentionReason, PullRequest},
    prompt::build_prompt,
    service::AgentRunner,
};

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub pull_request: PullRequest,
    pub trigger: AttentionReason,
    pub workspace: ResolvedWorkspaceConfig,
    pub agent: AgentConfig,
}

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub summary: String,
    pub processed_comment_at: Option<DateTime<Utc>>,
    pub processed_ci_at: Option<DateTime<Utc>>,
    pub processed_head_sha: String,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ShellAgentRunner;

impl AgentRunner for ShellAgentRunner {
    fn run(&self, request: RunRequest) -> BoxFuture<'static, RunOutcome> {
        Box::pin(async move { run(request).await })
    }
}

pub async fn run(request: RunRequest) -> RunOutcome {
    let started_at = Utc::now();

    let result = run_inner(&request).await;
    let finished_at = Utc::now();

    match result {
        Ok((exit_code, summary)) => RunOutcome {
            started_at,
            finished_at,
            success: exit_code == Some(0),
            exit_code,
            summary,
            processed_comment_at: request.pull_request.latest_reviewer_activity_at,
            processed_ci_at: request.pull_request.ci_updated_at,
            processed_head_sha: request.pull_request.head_sha.clone(),
        },
        Err(error) => RunOutcome {
            started_at,
            finished_at,
            success: false,
            exit_code: None,
            summary: error.to_string(),
            processed_comment_at: request.pull_request.latest_reviewer_activity_at,
            processed_ci_at: request.pull_request.ci_updated_at,
            processed_head_sha: request.pull_request.head_sha.clone(),
        },
    }
}

async fn run_inner(request: &RunRequest) -> Result<(Option<i32>, String)> {
    let workspace_path = resolve_checkout(&request.workspace, &request.pull_request).await?;
    sync_workspace(
        &workspace_path,
        request.workspace.git_transport,
        &request.pull_request,
    )
    .await?;

    let prompt = build_prompt(
        &request.pull_request,
        request.trigger,
        request.agent.additional_instructions.as_deref(),
    );

    let mut command = Command::new(&request.agent.command);
    command.args(&request.agent.args);
    command.current_dir(&workspace_path);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.env("SYMPHONY_PR_REPO", &request.pull_request.repo_full_name);
    command.env(
        "SYMPHONY_PR_NUMBER",
        request.pull_request.number.to_string(),
    );
    command.env("SYMPHONY_PR_URL", &request.pull_request.url);
    command.env("SYMPHONY_PR_HEAD_REF", &request.pull_request.head_ref);
    command.env("SYMPHONY_PR_BASE_REF", &request.pull_request.base_ref);
    command.env("SYMPHONY_PR_HEAD_SHA", &request.pull_request.head_sha);
    command.env("SYMPHONY_TRIGGER", request.trigger.label());
    command.env(
        "SYMPHONY_WORKSPACE",
        workspace_path.to_string_lossy().to_string(),
    );

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn agent command {}", request.agent.command))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("agent stdin was not available"))?;
    stdin
        .write_all(prompt.as_bytes())
        .await
        .context("failed writing prompt to agent stdin")?;
    stdin.shutdown().await.ok();

    let output = child
        .wait_with_output()
        .await
        .context("failed waiting for agent process")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let summary = summarize_output(&stdout, &stderr);

    Ok((output.status.code(), summary))
}

async fn resolve_checkout(
    workspace: &ResolvedWorkspaceConfig,
    pr: &PullRequest,
) -> Result<PathBuf> {
    let candidate = workspace
        .repo_map
        .get(&pr.repo_full_name)
        .cloned()
        .unwrap_or_else(|| workspace.root.join(repo_dir_name(&pr.repo_full_name)));

    if !candidate.exists() {
        return Err(anyhow!(
            "no local checkout found for {}: looked for {} and found no configured repo_map entry",
            pr.repo_full_name,
            candidate.display()
        ));
    }

    let repo_root = run_command_capture("git", &["rev-parse", "--show-toplevel"], Some(&candidate))
        .await
        .with_context(|| {
            format!(
                "configured checkout for {} is not a usable git repository: {}",
                pr.repo_full_name,
                candidate.display()
            )
        })?;
    let path = PathBuf::from(repo_root.trim());

    let tracked_changes = run_command_capture(
        "git",
        &["status", "--porcelain", "--untracked-files=no"],
        Some(&path),
    )
    .await
    .context("failed to inspect local checkout state")?;
    if !tracked_changes.trim().is_empty() {
        return Err(anyhow!(
            "local checkout {} has tracked changes; refusing to reuse a dirty repository",
            path.display()
        ));
    }

    Ok(path)
}

async fn sync_workspace(workspace: &Path, transport: GitTransport, pr: &PullRequest) -> Result<()> {
    let remote_url = match transport {
        GitTransport::Ssh => pr.ssh_url.as_str(),
        GitTransport::Https => pr.clone_url.as_str(),
    };
    let remote_name = "symphony-pr";

    if run_command_capture("git", &["remote", "get-url", remote_name], Some(workspace))
        .await
        .is_ok()
    {
        run_command(
            "git",
            &["remote", "set-url", remote_name, remote_url],
            Some(workspace),
        )
        .await
        .context("failed to update PR remote URL")?;
    } else {
        run_command(
            "git",
            &["remote", "add", remote_name, remote_url],
            Some(workspace),
        )
        .await
        .context("failed to add PR remote")?;
    }

    run_command(
        "git",
        &["fetch", remote_name, &pr.head_ref],
        Some(workspace),
    )
    .await
    .context("failed to fetch head branch")?;
    run_command(
        "git",
        &[
            "checkout",
            "-B",
            &pr.head_ref,
            &format!("{remote_name}/{}", pr.head_ref),
        ],
        Some(workspace),
    )
    .await
    .context("failed to check out PR branch")?;
    run_command(
        "git",
        &[
            "branch",
            "--set-upstream-to",
            &format!("{remote_name}/{}", pr.head_ref),
            &pr.head_ref,
        ],
        Some(workspace),
    )
    .await
    .context("failed to set PR branch upstream")?;

    Ok(())
}

async fn run_command(program: &str, args: &[&str], cwd: Option<&Path>) -> Result<()> {
    let output = run_command_output(program, args, cwd).await?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(anyhow!(
            "{program} exited with {:?}: {} {}",
            output.status.code(),
            stdout.trim(),
            stderr.trim()
        ))
    }
}

async fn run_command_capture(program: &str, args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let output = run_command_output(program, args, cwd).await?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(anyhow!(
            "{program} exited with {:?}: {} {}",
            output.status.code(),
            stdout.trim(),
            stderr.trim()
        ))
    }
}

async fn run_command_output(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
) -> Result<std::process::Output> {
    let mut command = Command::new(program);
    command.args(args);

    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }

    command
        .output()
        .await
        .with_context(|| format!("failed running {program}"))
}

fn repo_dir_name(repo_full_name: &str) -> &str {
    repo_full_name
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(repo_full_name)
}

fn summarize_output(stdout: &str, stderr: &str) -> String {
    let combined = [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    if combined.is_empty() {
        return "agent completed without output".to_owned();
    }

    let lines = combined.lines().collect::<Vec<_>>();
    let tail = lines.iter().rev().take(12).copied().collect::<Vec<_>>();
    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::{
        config::ResolvedWorkspaceConfig,
        model::{CiStatus, ReviewDecision},
    };

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn sample_pr(repo_full_name: &str) -> PullRequest {
        PullRequest {
            key: format!("{repo_full_name}#1"),
            repo_full_name: repo_full_name.to_owned(),
            number: 1,
            title: "Test".to_owned(),
            body: None,
            url: format!("https://github.com/{repo_full_name}/pull/1"),
            author_login: "connor".to_owned(),
            labels: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            head_sha: "abc123".to_owned(),
            head_ref: "feature/test".to_owned(),
            base_ref: "main".to_owned(),
            clone_url: format!("https://github.com/{repo_full_name}.git"),
            ssh_url: format!("git@github.com:{repo_full_name}.git"),
            ci_status: CiStatus::Success,
            ci_updated_at: None,
            review_decision: ReviewDecision::Clean,
            approval_count: 0,
            review_comment_count: 0,
            issue_comment_count: 0,
            latest_reviewer_activity_at: None,
            is_draft: false,
            is_closed: false,
            is_merged: false,
        }
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("symphony-rs-runner-{nonce}-{counter}-{name}"))
    }

    fn sample_workspace(root: PathBuf) -> ResolvedWorkspaceConfig {
        ResolvedWorkspaceConfig {
            root,
            repo_map: BTreeMap::new(),
            git_transport: GitTransport::Https,
        }
    }

    async fn init_git_repo(path: &Path) {
        std::fs::create_dir_all(path).expect("repo dir should create");
        run_command("git", &["init"], Some(path))
            .await
            .expect("git repo should initialize");
        run_command(
            "git",
            &["config", "user.email", "symphony-rs@example.com"],
            Some(path),
        )
        .await
        .expect("git email should configure");
        run_command("git", &["config", "user.name", "Symphony RS"], Some(path))
            .await
            .expect("git user should configure");
    }

    #[tokio::test]
    async fn resolve_checkout_uses_explicit_repo_map_first() {
        let root = unique_temp_path("root");
        let mapped = unique_temp_path("mapped-repo");
        let auto = root.join("symphony");
        init_git_repo(&mapped).await;
        init_git_repo(&auto).await;

        let mut workspace = sample_workspace(root);
        workspace
            .repo_map
            .insert("openai/symphony".to_owned(), mapped.clone());

        let resolved = resolve_checkout(&workspace, &sample_pr("openai/symphony"))
            .await
            .expect("mapped repo should resolve");
        assert_eq!(
            std::fs::canonicalize(resolved).expect("resolved repo should canonicalize"),
            std::fs::canonicalize(mapped).expect("mapped repo should canonicalize")
        );
    }

    #[tokio::test]
    async fn resolve_checkout_falls_back_to_same_name_under_root() {
        let root = unique_temp_path("root");
        let repo = root.join("symphony");
        init_git_repo(&repo).await;

        let resolved = resolve_checkout(&sample_workspace(root), &sample_pr("openai/symphony"))
            .await
            .expect("same-name repo should resolve");
        assert_eq!(
            std::fs::canonicalize(resolved).expect("resolved repo should canonicalize"),
            std::fs::canonicalize(repo).expect("repo should canonicalize")
        );
    }

    #[tokio::test]
    async fn resolve_checkout_errors_when_no_existing_repo_is_found() {
        let root = unique_temp_path("root");
        std::fs::create_dir_all(&root).expect("root dir should create");

        let error = resolve_checkout(
            &sample_workspace(root.clone()),
            &sample_pr("openai/symphony"),
        )
        .await
        .expect_err("missing repo should fail");
        assert!(
            error
                .to_string()
                .contains(&root.join("symphony").display().to_string()),
            "error should mention the missing auto-discovery path",
        );
    }

    #[tokio::test]
    async fn resolve_checkout_rejects_dirty_repositories() {
        let root = unique_temp_path("root");
        let repo = root.join("symphony");
        init_git_repo(&repo).await;
        std::fs::write(repo.join("tracked.txt"), "original").expect("tracked file should write");
        run_command("git", &["add", "tracked.txt"], Some(&repo))
            .await
            .expect("git add should succeed");
        run_command("git", &["commit", "-m", "init"], Some(&repo))
            .await
            .expect("git commit should succeed");
        std::fs::write(repo.join("tracked.txt"), "modified").expect("tracked file should rewrite");

        let error = resolve_checkout(&sample_workspace(root), &sample_pr("openai/symphony"))
            .await
            .expect_err("dirty repo should fail");
        assert!(
            error.to_string().contains("tracked changes"),
            "error should explain that dirty repositories are rejected",
        );
    }
}
