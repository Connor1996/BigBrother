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

#[derive(Debug, Clone, Default)]
struct WorkspaceSyncReport {
    merged_base_branch: bool,
    merge_conflicts_present: bool,
}

impl WorkspaceSyncReport {
    fn prompt_note(&self) -> Option<&'static str> {
        if self.merge_conflicts_present {
            Some(
                "- Workspace preparation already merged the latest base branch into the local PR branch and Git reported merge conflicts.\n- Start by resolving the existing conflicts in the working tree, then run validation, commit the resolution, and push if safe.",
            )
        } else if self.merged_base_branch {
            Some(
                "- Workspace preparation already merged the latest base branch into the local PR branch before this run.\n- Validate the merged result, make any required follow-up fixes, then commit and push if safe.",
            )
        } else {
            None
        }
    }
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
    let sync_report = sync_workspace(
        &workspace_path,
        request.workspace.git_transport,
        &request.pull_request,
    )
    .await?;

    let prompt_instructions = combine_operator_instructions(
        request.agent.additional_instructions.as_deref(),
        sync_report.prompt_note(),
    );

    let prompt = build_prompt(
        &request.pull_request,
        request.trigger,
        prompt_instructions.as_deref(),
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
    command.env("SYMPHONY_PR_BASE_SHA", &request.pull_request.base_sha);
    command.env("SYMPHONY_PR_HEAD_SHA", &request.pull_request.head_sha);
    command.env(
        "SYMPHONY_PR_HAS_CONFLICT",
        if request.pull_request.has_conflicts {
            "1"
        } else {
            "0"
        },
    );
    if let Some(mergeable_state) = request.pull_request.mergeable_state.as_deref() {
        command.env("SYMPHONY_PR_MERGEABLE_STATE", mergeable_state);
    }
    command.env("SYMPHONY_TRIGGER", request.trigger.label());
    command.env(
        "SYMPHONY_WORKSPACE",
        workspace_path.to_string_lossy().to_string(),
    );
    command.env(
        "SYMPHONY_BASE_BRANCH_MERGED",
        if sync_report.merged_base_branch {
            "1"
        } else {
            "0"
        },
    );
    command.env(
        "SYMPHONY_WORKSPACE_CONFLICTS_PRESENT",
        if sync_report.merge_conflicts_present {
            "1"
        } else {
            "0"
        },
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

async fn sync_workspace(
    workspace: &Path,
    transport: GitTransport,
    pr: &PullRequest,
) -> Result<WorkspaceSyncReport> {
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
        &["fetch", remote_name, &pr.base_ref],
        Some(workspace),
    )
    .await
    .context("failed to fetch base branch")?;
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

    merge_base_branch(workspace, remote_name, pr).await
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

async fn merge_base_branch(
    workspace: &Path,
    remote_name: &str,
    pr: &PullRequest,
) -> Result<WorkspaceSyncReport> {
    if pr.base_ref == pr.head_ref {
        return Ok(WorkspaceSyncReport::default());
    }

    let merge_target = format!("{remote_name}/{}", pr.base_ref);
    let output = run_command_output(
        "git",
        &["merge", "--no-edit", "--no-ff", &merge_target],
        Some(workspace),
    )
    .await
    .context("failed to run git merge for base branch")?;
    let summary = summarize_command_result(&output);

    if output.status.success() {
        return Ok(WorkspaceSyncReport {
            merged_base_branch: !is_merge_already_up_to_date(&summary),
            merge_conflicts_present: false,
        });
    }

    if has_unmerged_paths(workspace).await? {
        return Ok(WorkspaceSyncReport {
            merged_base_branch: true,
            merge_conflicts_present: true,
        });
    }

    Err(anyhow!(
        "failed to merge base branch {merge_target}: {summary}"
    ))
}

async fn has_unmerged_paths(workspace: &Path) -> Result<bool> {
    let output = run_command_capture(
        "git",
        &["diff", "--name-only", "--diff-filter=U"],
        Some(workspace),
    )
    .await
    .context("failed to detect unmerged paths")?;

    Ok(!output.trim().is_empty())
}

fn summarize_command_result(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    summarize_output(&stdout, &stderr)
}

fn is_merge_already_up_to_date(summary: &str) -> bool {
    let normalized = summary.to_ascii_lowercase();
    normalized.contains("already up to date")
}

fn combine_operator_instructions(base: Option<&str>, extra: Option<&str>) -> Option<String> {
    match (
        base.map(str::trim).filter(|value| !value.is_empty()),
        extra.map(str::trim).filter(|value| !value.is_empty()),
    ) {
        (Some(base), Some(extra)) => Some(format!("{base}\n{extra}")),
        (Some(base), None) => Some(base.to_owned()),
        (None, Some(extra)) => Some(extra.to_owned()),
        (None, None) => None,
    }
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
        fs,
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
            base_sha: "def456".to_owned(),
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
            has_conflicts: false,
            mergeable_state: Some("clean".to_owned()),
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
        run_command("git", &["branch", "-M", "main"], Some(path))
            .await
            .expect("default branch should be main");
    }

    async fn commit_all(path: &Path, message: &str) {
        run_command("git", &["add", "."], Some(path))
            .await
            .expect("git add should succeed");
        run_command("git", &["commit", "-m", message], Some(path))
            .await
            .expect("git commit should succeed");
    }

    async fn init_bare_repo(path: &Path) {
        let bare = path.to_str().expect("path should be utf-8");
        run_command("git", &["init", "--bare", bare], None)
            .await
            .expect("bare repo should initialize");
    }

    async fn clone_repo(remote: &Path, path: &Path) {
        let remote = remote.to_str().expect("remote path should be utf-8");
        let path = path.to_str().expect("clone path should be utf-8");
        run_command("git", &["clone", remote, path], None)
            .await
            .expect("repo should clone");
    }

    async fn prepare_workspace_scenario(
        with_conflict: bool,
    ) -> (ResolvedWorkspaceConfig, PullRequest) {
        let remote = unique_temp_path("remote.git");
        let seed = unique_temp_path("seed");
        let root = unique_temp_path("root");
        let workspace_repo = root.join("symphony");

        init_bare_repo(&remote).await;
        init_git_repo(&seed).await;
        fs::write(seed.join("shared.txt"), "base\n").expect("seed file should write");
        commit_all(&seed, "initial").await;

        let remote_str = remote.to_str().expect("remote path should be utf-8");
        run_command("git", &["remote", "add", "origin", remote_str], Some(&seed))
            .await
            .expect("origin remote should add");
        run_command("git", &["push", "-u", "origin", "main"], Some(&seed))
            .await
            .expect("main should push");
        run_command(
            "git",
            &["symbolic-ref", "HEAD", "refs/heads/main"],
            Some(&remote),
        )
        .await
        .expect("bare HEAD should point at main");

        run_command("git", &["checkout", "-b", "feature/test"], Some(&seed))
            .await
            .expect("feature branch should create");
        if with_conflict {
            fs::write(seed.join("shared.txt"), "feature change\n")
                .expect("feature edit should write");
        } else {
            fs::write(seed.join("feature.txt"), "feature change\n")
                .expect("feature file should write");
        }
        commit_all(&seed, "feature change").await;
        run_command(
            "git",
            &["push", "-u", "origin", "feature/test"],
            Some(&seed),
        )
        .await
        .expect("feature branch should push");

        run_command("git", &["checkout", "main"], Some(&seed))
            .await
            .expect("main should check out");
        if with_conflict {
            fs::write(seed.join("shared.txt"), "main change\n").expect("main edit should write");
        } else {
            fs::write(seed.join("base.txt"), "main change\n").expect("base file should write");
        }
        commit_all(&seed, "main change").await;
        run_command("git", &["push", "origin", "main"], Some(&seed))
            .await
            .expect("main should push again");

        std::fs::create_dir_all(&root).expect("workspace root should exist");
        clone_repo(&remote, &workspace_repo).await;
        run_command(
            "git",
            &["config", "user.email", "symphony-rs@example.com"],
            Some(&workspace_repo),
        )
        .await
        .expect("workspace email should configure");
        run_command(
            "git",
            &["config", "user.name", "Symphony RS"],
            Some(&workspace_repo),
        )
        .await
        .expect("workspace user should configure");

        let base_sha = run_command_capture("git", &["rev-parse", "main"], Some(&seed))
            .await
            .expect("base sha should resolve");
        let head_sha = run_command_capture("git", &["rev-parse", "feature/test"], Some(&seed))
            .await
            .expect("head sha should resolve");

        let mut pr = sample_pr("openai/symphony");
        pr.clone_url = remote_str.to_owned();
        pr.ssh_url = remote_str.to_owned();
        pr.head_ref = "feature/test".to_owned();
        pr.base_ref = "main".to_owned();
        pr.head_sha = head_sha;
        pr.base_sha = base_sha;
        pr.has_conflicts = with_conflict;
        pr.mergeable_state = Some(if with_conflict {
            "dirty".to_owned()
        } else {
            "clean".to_owned()
        });

        (sample_workspace(root), pr)
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

    #[tokio::test]
    async fn sync_workspace_merges_base_branch_when_merge_is_clean() {
        let (workspace, pr) = prepare_workspace_scenario(false).await;
        let repo = workspace.root.join("symphony");

        let report = sync_workspace(&repo, workspace.git_transport, &pr)
            .await
            .expect("clean merge should succeed");

        assert!(report.merged_base_branch, "base branch should be merged");
        assert!(
            !report.merge_conflicts_present,
            "clean merge should not leave conflicts behind"
        );
        assert!(
            repo.join("base.txt").exists(),
            "workspace should contain the file introduced on main after the merge"
        );
        assert!(
            !has_unmerged_paths(&repo)
                .await
                .expect("unmerged path detection should work"),
            "clean merge should leave no unmerged paths",
        );
    }

    #[tokio::test]
    async fn sync_workspace_keeps_conflict_markers_for_agent_resolution() {
        let (workspace, pr) = prepare_workspace_scenario(true).await;
        let repo = workspace.root.join("symphony");

        let report = sync_workspace(&repo, workspace.git_transport, &pr)
            .await
            .expect("conflicted merge should still return a sync report");

        assert!(
            report.merged_base_branch,
            "merge attempt should have happened"
        );
        assert!(
            report.merge_conflicts_present,
            "conflicted merge should be reported back to the runner"
        );
        assert!(
            has_unmerged_paths(&repo)
                .await
                .expect("unmerged path detection should work"),
            "workspace should keep the unmerged files for the agent",
        );

        let conflict_contents =
            fs::read_to_string(repo.join("shared.txt")).expect("conflicted file should exist");
        assert!(
            conflict_contents.contains("<<<<<<<"),
            "conflicted file should include Git conflict markers",
        );
    }
}
