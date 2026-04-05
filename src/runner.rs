use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::{process::Command, sync::mpsc::UnboundedSender};

use crate::{
    config::{AgentConfig, AgentPromptTemplates, GitTransport, ResolvedWorkspaceConfig},
    model::{AttentionReason, PullRequest, NEEDS_DECISION_SUMMARY},
    prompt::{build_prompt, render_template},
    service::AgentRunner,
};

pub(crate) const PROMPT_TRANSCRIPT_HEADER: &str = "=== Prompt Sent To Codex CLI ===\n";
pub(crate) const OUTPUT_TRANSCRIPT_HEADER: &str = "=== Codex CLI Output ===\n";
pub(crate) const NEEDS_DECISION_PREFIX: &str = "BIGBROTHER_NEEDS_DECISION:";
const DEFAULT_PTY_ROWS: u16 = 36;
const DEFAULT_PTY_COLS: u16 = 120;
const DEEP_REVIEW_TARGET_DIR: &str = "target/bigbrother-deep-review";

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub pull_request: PullRequest,
    pub trigger: AttentionReason,
    pub workspace: ResolvedWorkspaceConfig,
    pub agent: AgentConfig,
    pub output_updates: Option<UnboundedSender<RunUpdate>>,
}

#[derive(Debug, Clone)]
pub enum RunUpdate {
    TranscriptChunk(String),
    TerminalSnapshot {
        screen: String,
        last_output_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub summary: String,
    pub needs_decision_reason: Option<String>,
    pub captured_output: Option<String>,
    pub captured_terminal: Option<String>,
    pub last_terminal_output_at: Option<DateTime<Utc>>,
    pub processed_comment_at: Option<DateTime<Utc>>,
    pub processed_ci_at: Option<DateTime<Utc>>,
    pub processed_head_sha: String,
}

#[derive(Debug, Clone)]
struct NeedsDecisionSignal {
    reason: String,
    display_output: String,
}

#[derive(Debug)]
struct RunFailure {
    error: anyhow::Error,
    captured_output: Option<String>,
}

#[derive(Debug)]
struct PromptFile {
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct DeepReviewArtifact {
    path: PathBuf,
    target_dir: PathBuf,
    file_name: String,
}

impl PromptFile {
    fn create(prompt: &str) -> Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before UNIX_EPOCH")?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "symphony-rs-prompt-{}-{nonce}.txt",
            std::process::id()
        ));
        fs::write(&path, prompt)
            .with_context(|| format!("failed to write prompt file at {}", path.display()))?;
        Ok(Self { path })
    }
}

impl Drop for PromptFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl RunFailure {
    fn without_output(error: anyhow::Error) -> Self {
        Self {
            error,
            captured_output: None,
        }
    }

    fn with_transcript(error: anyhow::Error, transcript_preamble: &str) -> Self {
        Self {
            captured_output: Some(cli_transcript(transcript_preamble, &format!("{error}\n"))),
            error,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct WorkspaceSyncReport {
    fetched_pr_branch: bool,
    fetched_base_branch: bool,
    resumed_conflict_workspace: bool,
}

impl WorkspaceSyncReport {
    fn prompt_note(&self, templates: &AgentPromptTemplates) -> Option<String> {
        if self.resumed_conflict_workspace {
            Some(templates.resumed_conflict.clone())
        } else if self.fetched_pr_branch || self.fetched_base_branch {
            Some(templates.workspace_ready.clone())
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
        Ok((
            exit_code,
            summary,
            needs_decision_reason,
            captured_output,
            captured_terminal,
            last_terminal_output_at,
        )) => RunOutcome {
            started_at,
            finished_at,
            success: exit_code == Some(0),
            exit_code,
            summary,
            needs_decision_reason,
            captured_output,
            captured_terminal,
            last_terminal_output_at,
            processed_comment_at: request.pull_request.latest_reviewer_activity_at,
            processed_ci_at: request.pull_request.ci_updated_at,
            processed_head_sha: request.pull_request.head_sha.clone(),
        },
        Err(failure) => RunOutcome {
            started_at,
            finished_at,
            success: false,
            exit_code: None,
            summary: request.trigger.failure_summary().to_owned(),
            needs_decision_reason: None,
            captured_output: failure.captured_output.or(Some(failure.error.to_string())),
            captured_terminal: None,
            last_terminal_output_at: None,
            processed_comment_at: request.pull_request.latest_reviewer_activity_at,
            processed_ci_at: request.pull_request.ci_updated_at,
            processed_head_sha: request.pull_request.head_sha.clone(),
        },
    }
}

async fn run_inner(
    request: &RunRequest,
) -> Result<
    (
        Option<i32>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<DateTime<Utc>>,
    ),
    RunFailure,
> {
    let checkout = resolve_checkout(&request.workspace, &request.pull_request)
        .await
        .map_err(RunFailure::without_output)?;
    let workspace_path = checkout.path;
    let sync_report = if checkout.resumed_conflict_workspace {
        WorkspaceSyncReport {
            fetched_pr_branch: false,
            fetched_base_branch: false,
            resumed_conflict_workspace: true,
        }
    } else {
        sync_workspace(
            &workspace_path,
            request.workspace.git_transport,
            &request.pull_request,
        )
        .await
        .map_err(RunFailure::without_output)?
    };

    let deep_review_artifact = if request.trigger == AttentionReason::DeepReview {
        Some(
            prepare_deep_review_artifact(&workspace_path, &request.pull_request)
                .map_err(RunFailure::without_output)?,
        )
    } else {
        None
    };

    let prompt_instructions = build_prompt_instructions(
        request.trigger,
        request.agent.additional_instructions.as_deref(),
        &sync_report,
        deep_review_artifact.as_ref(),
        &request.agent.prompts,
    );

    let prompt = build_prompt(
        &request.pull_request,
        request.trigger,
        &request.agent.prompts,
        prompt_instructions.as_deref(),
    );
    let transcript_preamble = cli_transcript_preamble(&prompt);

    if let Some(output_updates) = request.output_updates.as_ref() {
        let _ = output_updates.send(RunUpdate::TranscriptChunk(transcript_preamble.clone()));
    }

    let prompt_file = PromptFile::create(&prompt)
        .map_err(|error| RunFailure::with_transcript(error, &transcript_preamble))?;
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: DEFAULT_PTY_ROWS,
            cols: DEFAULT_PTY_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("failed to create PTY for agent command")
        .map_err(|error| RunFailure::with_transcript(error, &transcript_preamble))?;
    let command = build_pty_command(request, &workspace_path, &sync_report, &prompt_file)
        .map_err(|error| RunFailure::with_transcript(error, &transcript_preamble))?;
    let child = pair
        .slave
        .spawn_command(command)
        .with_context(|| format!("failed to spawn agent command {}", request.agent.command))
        .map_err(|error| RunFailure::with_transcript(error, &transcript_preamble))?;
    drop(pair.slave);
    let reader = pair
        .master
        .try_clone_reader()
        .context("failed to clone PTY reader for agent command")
        .map_err(|error| RunFailure::with_transcript(error, &transcript_preamble))?;
    drop(pair.master);

    let (status, combined_output, terminal_screen, last_terminal_output_at) =
        collect_process_output(child, reader, request.output_updates.clone())
            .await
            .map_err(|error| RunFailure::with_transcript(error, &transcript_preamble))?;
    let exit_code = i32::try_from(status.exit_code()).ok();
    let needs_decision_signal = if request.trigger == AttentionReason::DeepReview {
        None
    } else {
        extract_needs_decision_signal(&combined_output)
    };
    let summary = if needs_decision_signal.is_some() {
        NEEDS_DECISION_SUMMARY.to_owned()
    } else if exit_code == Some(0) {
        request.trigger.success_summary().to_owned()
    } else {
        request.trigger.failure_summary().to_owned()
    };
    let transcript_output = needs_decision_signal
        .as_ref()
        .map(|signal| signal.display_output.as_str())
        .unwrap_or(combined_output.as_str());
    let base_captured_output =
        normalize_output(&cli_transcript(&transcript_preamble, transcript_output));
    let captured_output = if request.trigger == AttentionReason::DeepReview && exit_code == Some(0)
    {
        let artifact = deep_review_artifact.as_ref().ok_or_else(|| {
            RunFailure::with_transcript(
                anyhow!("missing deep review artifact"),
                &transcript_preamble,
            )
        })?;
        normalize_output(
            &load_deep_review_report(artifact).map_err(|error| RunFailure {
                error,
                captured_output: base_captured_output.clone(),
            })?,
        )
    } else {
        base_captured_output
    };
    let captured_terminal = normalize_output(&terminal_screen);

    Ok((
        exit_code,
        summary,
        needs_decision_signal.map(|signal| signal.reason),
        captured_output,
        captured_terminal,
        last_terminal_output_at,
    ))
}

#[derive(Debug)]
struct CheckoutResolution {
    path: PathBuf,
    resumed_conflict_workspace: bool,
}

async fn resolve_checkout(
    workspace: &ResolvedWorkspaceConfig,
    pr: &PullRequest,
) -> Result<CheckoutResolution> {
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
        if can_resume_existing_conflict_workspace(&path, pr).await? {
            return Ok(CheckoutResolution {
                path,
                resumed_conflict_workspace: true,
            });
        }

        return Err(anyhow!(
            "local checkout {} has tracked changes; refusing to reuse a dirty repository",
            path.display()
        ));
    }

    Ok(CheckoutResolution {
        path,
        resumed_conflict_workspace: false,
    })
}

async fn can_resume_existing_conflict_workspace(
    workspace: &Path,
    pr: &PullRequest,
) -> Result<bool> {
    let current_branch = run_command_capture(
        "git",
        &["rev-parse", "--abbrev-ref", "HEAD"],
        Some(workspace),
    )
    .await
    .context("failed to inspect current branch for conflict resume")?;
    if current_branch.trim() != pr.head_ref {
        return Ok(false);
    }

    let current_head_sha = run_command_capture("git", &["rev-parse", "HEAD"], Some(workspace))
        .await
        .context("failed to inspect current HEAD for conflict resume")?;
    if current_head_sha.trim() != pr.head_sha {
        return Ok(false);
    }

    let merge_head_sha = match run_command_capture(
        "git",
        &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        Some(workspace),
    )
    .await
    {
        Ok(output) => output,
        Err(_) => return Ok(false),
    };
    if merge_head_sha.trim() != pr.base_sha {
        return Ok(false);
    }

    has_unmerged_paths(workspace).await
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

    Ok(WorkspaceSyncReport {
        fetched_pr_branch: true,
        fetched_base_branch: true,
        resumed_conflict_workspace: false,
    })
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

fn build_prompt_instructions(
    trigger: AttentionReason,
    base: Option<&str>,
    sync_report: &WorkspaceSyncReport,
    deep_review_artifact: Option<&DeepReviewArtifact>,
    templates: &AgentPromptTemplates,
) -> Option<String> {
    let extra = match trigger {
        AttentionReason::DeepReview => {
            deep_review_artifact.map(|artifact| deep_review_prompt_note(artifact, templates))
        }
        _ => sync_report.prompt_note(templates),
    };

    combine_operator_instructions(base, extra.as_deref())
}

fn prepare_deep_review_artifact(
    workspace_path: &Path,
    pr: &PullRequest,
) -> Result<DeepReviewArtifact> {
    let target_dir = workspace_path.join(DEEP_REVIEW_TARGET_DIR);
    fs::create_dir_all(&target_dir).with_context(|| {
        format!(
            "failed to create deep review target directory at {}",
            target_dir.display()
        )
    })?;
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let file_name = format!("review-report-pr{}-{timestamp}.md", pr.number);
    let path = target_dir.join(&file_name);

    Ok(DeepReviewArtifact {
        path,
        target_dir,
        file_name,
    })
}

fn deep_review_prompt_note(
    artifact: &DeepReviewArtifact,
    templates: &AgentPromptTemplates,
) -> String {
    render_template(
        &templates.deep_review_artifact,
        &[
            ("target_dir", artifact.target_dir.display().to_string()),
            ("file_name", artifact.file_name.clone()),
            ("artifact_path", artifact.path.display().to_string()),
        ],
    )
}

fn load_deep_review_report(artifact: &DeepReviewArtifact) -> Result<String> {
    let report = fs::read_to_string(&artifact.path).with_context(|| {
        format!(
            "deep review succeeded but no review report was written to {}",
            artifact.path.display()
        )
    })?;

    let report = report.trim();
    if report.is_empty() {
        Err(anyhow!(
            "deep review report at {} was empty",
            artifact.path.display()
        ))
    } else {
        Ok(report.to_owned())
    }
}

fn repo_dir_name(repo_full_name: &str) -> &str {
    repo_full_name
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(repo_full_name)
}

pub(crate) fn cli_transcript_preamble(prompt: &str) -> String {
    format!(
        "{PROMPT_TRANSCRIPT_HEADER}{}\n\n{OUTPUT_TRANSCRIPT_HEADER}",
        prompt.trim_end_matches('\n')
    )
}

pub(crate) fn cli_transcript(transcript_preamble: &str, output: &str) -> String {
    let mut transcript = String::with_capacity(transcript_preamble.len() + output.len());
    transcript.push_str(transcript_preamble);
    transcript.push_str(output);
    transcript
}

fn build_pty_command(
    request: &RunRequest,
    workspace_path: &Path,
    sync_report: &WorkspaceSyncReport,
    prompt_file: &PromptFile,
) -> Result<CommandBuilder> {
    let prompt_path = prompt_file
        .path
        .to_str()
        .ok_or_else(|| anyhow!("prompt file path is not valid UTF-8"))?;
    let mut command = CommandBuilder::new("sh");
    command.arg("-c");
    command.arg("exec \"$@\" <\"$SYMPHONY_PROMPT_PATH\"");
    command.arg("symphony-agent");
    for arg in build_agent_command_argv(&request.agent) {
        command.arg(arg);
    }

    command.cwd(workspace_path);
    command.env("SYMPHONY_PROMPT_PATH", prompt_path);
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
    command.env("SYMPHONY_BASE_BRANCH_MERGED", "0");
    command.env(
        "SYMPHONY_BASE_BRANCH_FETCHED",
        if sync_report.fetched_base_branch {
            "1"
        } else {
            "0"
        },
    );
    command.env(
        "SYMPHONY_WORKSPACE_CONFLICTS_PRESENT",
        if sync_report.resumed_conflict_workspace {
            "1"
        } else {
            "0"
        },
    );

    Ok(command)
}

fn build_agent_command_argv(agent: &AgentConfig) -> Vec<String> {
    let mut argv = vec![agent.command.clone()];
    if agent.dangerously_bypass_approvals_and_sandbox {
        argv.push("--dangerously-bypass-approvals-and-sandbox".to_owned());
    }
    if should_inject_codex_reasoning_effort(agent) {
        argv.push("-c".to_owned());
        argv.push(format!(
            "model_reasoning_effort={:?}",
            agent.model_reasoning_effort
        ));
    }
    argv.extend(agent.args.iter().cloned());
    argv
}

fn should_inject_codex_reasoning_effort(agent: &AgentConfig) -> bool {
    Path::new(&agent.command)
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value == "codex")
}

fn normalize_output(output: &str) -> Option<String> {
    if output.trim().is_empty() {
        None
    } else {
        Some(output.to_owned())
    }
}

fn extract_needs_decision_signal(output: &str) -> Option<NeedsDecisionSignal> {
    let mut reason = None;
    let mut explanation_lines = Vec::new();
    let mut saw_marker = false;

    for line in output.lines() {
        if !saw_marker {
            if let Some(value) = line.trim().strip_prefix(NEEDS_DECISION_PREFIX) {
                let parsed = value.trim();
                reason = Some(if parsed.is_empty() {
                    NEEDS_DECISION_SUMMARY.to_owned()
                } else {
                    parsed.to_owned()
                });
                saw_marker = true;
            }
            continue;
        }

        explanation_lines.push(line);
    }

    let reason = reason?;
    let explanation = explanation_lines.join("\n").trim().to_owned();
    let display_output = if explanation.is_empty() {
        format!("Operator decision required: {reason}")
    } else {
        format!("Operator decision required: {reason}\n\n{explanation}")
    };

    Some(NeedsDecisionSignal {
        reason,
        display_output,
    })
}

async fn collect_process_output(
    mut child: Box<dyn portable_pty::Child + Send>,
    reader: Box<dyn Read + Send>,
    output_updates: Option<UnboundedSender<RunUpdate>>,
) -> Result<(
    portable_pty::ExitStatus,
    String,
    String,
    Option<DateTime<Utc>>,
)> {
    let wait_task = tokio::task::spawn_blocking(move || {
        child.wait().context("failed waiting for agent PTY process")
    });
    let reader_task =
        tokio::task::spawn_blocking(move || read_output_stream(reader, output_updates));

    let status = wait_task.await.context("agent wait task panicked")??;
    let (combined_text, terminal_screen, last_terminal_output_at) = reader_task
        .await
        .context("agent PTY reader task panicked")??;

    Ok((
        status,
        combined_text,
        terminal_screen,
        last_terminal_output_at,
    ))
}

fn read_output_stream(
    mut reader: Box<dyn Read + Send>,
    output_updates: Option<UnboundedSender<RunUpdate>>,
) -> Result<(String, String, Option<DateTime<Utc>>)> {
    let mut parser = vt100::Parser::new(DEFAULT_PTY_ROWS, DEFAULT_PTY_COLS, 0);
    let mut buffer = [0_u8; 4096];
    let mut combined_text = String::new();
    let mut last_terminal_output_at = None;

    loop {
        let bytes_read = reader
            .read(&mut buffer)
            .context("failed reading agent PTY output stream")?;
        if bytes_read == 0 {
            break;
        }

        let chunk = &buffer[..bytes_read];
        parser.process(chunk);
        let output_at = Utc::now();
        last_terminal_output_at = Some(output_at);

        let transcript_chunk = normalize_terminal_transcript_chunk(chunk);
        if !transcript_chunk.is_empty() {
            combined_text.push_str(&transcript_chunk);
            if let Some(output_updates) = output_updates.as_ref() {
                let _ = output_updates.send(RunUpdate::TranscriptChunk(transcript_chunk));
            }
        }

        let screen = render_terminal_screen(&parser);
        if let Some(output_updates) = output_updates.as_ref() {
            let _ = output_updates.send(RunUpdate::TerminalSnapshot {
                screen,
                last_output_at: output_at,
            });
        }
    }

    Ok((
        combined_text,
        render_terminal_screen(&parser),
        last_terminal_output_at,
    ))
}

fn normalize_terminal_transcript_chunk(chunk: &[u8]) -> String {
    let stripped = strip_ansi_escapes::strip(chunk);
    normalize_carriage_returns(&String::from_utf8_lossy(&stripped))
}

fn normalize_carriage_returns(text: &str) -> String {
    let mut normalized = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\r' {
            normalized.push('\n');
            if matches!(chars.peek(), Some('\n')) {
                chars.next();
            }
        } else {
            normalized.push(ch);
        }
    }

    normalized
}

fn render_terminal_screen(parser: &vt100::Parser) -> String {
    trim_trailing_blank_lines(&parser.screen().contents())
}

fn trim_trailing_blank_lines(screen: &str) -> String {
    let mut lines = screen.lines().collect::<Vec<_>>();
    while matches!(lines.last(), Some(line) if line.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n")
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
        config::{AgentPromptTemplates, ResolvedWorkspaceConfig},
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
            &["config", "user.email", "bigbrother@example.com"],
            Some(path),
        )
        .await
        .expect("git email should configure");
        run_command("git", &["config", "user.name", "BigBrother"], Some(path))
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
            &["config", "user.email", "bigbrother@example.com"],
            Some(&workspace_repo),
        )
        .await
        .expect("workspace email should configure");
        run_command(
            "git",
            &["config", "user.name", "BigBrother"],
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

    #[test]
    fn cli_transcript_separates_prompt_and_output() {
        let prompt = "Inspect the workspace\nRun focused validation\n";
        let output = "codex: inspected workspace\ncargo test -q\n";
        let transcript = cli_transcript(&cli_transcript_preamble(prompt), output);

        assert!(transcript.starts_with(PROMPT_TRANSCRIPT_HEADER));
        assert!(transcript.contains("Inspect the workspace\nRun focused validation"));
        assert!(transcript.contains("\n\n=== Codex CLI Output ===\n"));
        assert!(transcript.ends_with(output));
    }

    #[test]
    fn deep_review_prompt_instructions_use_skill_artifact_without_merge_guidance() {
        let artifact = DeepReviewArtifact {
            path: PathBuf::from("/tmp/review-report.md"),
            target_dir: PathBuf::from("/tmp"),
            file_name: "review-report.md".to_owned(),
        };
        let sync_report = WorkspaceSyncReport {
            fetched_pr_branch: true,
            fetched_base_branch: true,
            resumed_conflict_workspace: false,
        };

        let instructions = build_prompt_instructions(
            AttentionReason::DeepReview,
            Some("- Operator note."),
            &sync_report,
            Some(&artifact),
            &AgentPromptTemplates::default(),
        )
        .expect("deep review instructions should exist");

        assert!(instructions.contains("$deep-review"));
        assert!(instructions.contains("review-report.md"));
        assert!(!instructions.contains("merge the latest base branch"));
    }

    #[test]
    fn actionable_prompt_instructions_keep_sync_guidance() {
        let sync_report = WorkspaceSyncReport {
            fetched_pr_branch: true,
            fetched_base_branch: true,
            resumed_conflict_workspace: false,
        };

        let instructions = build_prompt_instructions(
            AttentionReason::CiFailed,
            None,
            &sync_report,
            None,
            &AgentPromptTemplates::default(),
        )
        .expect("actionable instructions should exist");

        assert!(instructions.contains("merge the latest base branch"));
    }

    #[test]
    fn extract_needs_decision_signal_rewrites_operator_output() {
        let signal = extract_needs_decision_signal(
            "thinking out loud\nBIGBROTHER_NEEDS_DECISION: requires API behavior change\nNeed approval to alter the public response shape.\nOption A keeps compatibility.\nOption B simplifies the handler.",
        )
        .expect("decision marker should be detected");

        assert_eq!(signal.reason, "requires API behavior change");
        assert_eq!(
            signal.display_output,
            "Operator decision required: requires API behavior change\n\nNeed approval to alter the public response shape.\nOption A keeps compatibility.\nOption B simplifies the handler."
        );
    }

    #[test]
    fn extract_needs_decision_signal_requires_explicit_marker() {
        assert!(
            extract_needs_decision_signal("Need approval before changing API shape.").is_none()
        );
    }

    #[test]
    fn build_agent_command_argv_injects_xhigh_reasoning_for_codex_exec() {
        let agent = AgentConfig {
            command: "codex".to_owned(),
            args: vec![
                "exec".to_owned(),
                "--model".to_owned(),
                "gpt-5.4".to_owned(),
            ],
            model_reasoning_effort: "xhigh".to_owned(),
            dangerously_bypass_approvals_and_sandbox: true,
            additional_instructions: None,
            prompts: AgentPromptTemplates::default(),
        };

        assert_eq!(
            build_agent_command_argv(&agent),
            vec![
                "codex".to_owned(),
                "--dangerously-bypass-approvals-and-sandbox".to_owned(),
                "-c".to_owned(),
                "model_reasoning_effort=\"xhigh\"".to_owned(),
                "exec".to_owned(),
                "--model".to_owned(),
                "gpt-5.4".to_owned(),
            ]
        );
    }

    #[test]
    fn build_agent_command_argv_skips_codex_only_reasoning_override_for_other_agents() {
        let agent = AgentConfig {
            command: "other-agent".to_owned(),
            args: vec!["run".to_owned()],
            model_reasoning_effort: "xhigh".to_owned(),
            dangerously_bypass_approvals_and_sandbox: false,
            additional_instructions: None,
            prompts: AgentPromptTemplates::default(),
        };

        assert_eq!(
            build_agent_command_argv(&agent),
            vec!["other-agent".to_owned(), "run".to_owned()]
        );
    }

    async fn simulate_agent_merge_conflict(workspace_repo: &Path, pr: &PullRequest) {
        run_command(
            "git",
            &[
                "checkout",
                "-B",
                &pr.head_ref,
                &format!("origin/{}", pr.head_ref),
            ],
            Some(workspace_repo),
        )
        .await
        .expect("feature branch should check out in workspace");

        let merge_result = run_command_output(
            "git",
            &[
                "merge",
                "--no-edit",
                "--no-ff",
                &format!("origin/{}", pr.base_ref),
            ],
            Some(workspace_repo),
        )
        .await
        .expect("git merge should execute");

        assert!(
            !merge_result.status.success(),
            "test setup expects a conflicting merge"
        );
        assert!(
            has_unmerged_paths(workspace_repo)
                .await
                .expect("unmerged path detection should work"),
            "test setup should leave merge conflicts behind",
        );
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
            std::fs::canonicalize(resolved.path).expect("resolved repo should canonicalize"),
            std::fs::canonicalize(mapped).expect("mapped repo should canonicalize")
        );
        assert!(
            !resolved.resumed_conflict_workspace,
            "clean mapped repo should not be treated as a conflict resume",
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
            std::fs::canonicalize(resolved.path).expect("resolved repo should canonicalize"),
            std::fs::canonicalize(repo).expect("repo should canonicalize")
        );
        assert!(
            !resolved.resumed_conflict_workspace,
            "clean auto-discovered repo should not be treated as a conflict resume",
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
    async fn resolve_checkout_allows_resuming_matching_conflict_workspace() {
        let (workspace, pr) = prepare_workspace_scenario(true).await;
        let repo = workspace.root.join("symphony");

        simulate_agent_merge_conflict(&repo, &pr).await;

        let resolved = resolve_checkout(&workspace, &pr)
            .await
            .expect("matching unresolved conflict workspace should resume");
        assert!(
            resolved.resumed_conflict_workspace,
            "runner should recognize the previous conflict workspace for this PR",
        );
        assert_eq!(
            std::fs::canonicalize(resolved.path).expect("resolved repo should canonicalize"),
            std::fs::canonicalize(repo).expect("repo should canonicalize")
        );
    }

    #[tokio::test]
    async fn sync_workspace_fetches_branches_and_checks_out_pr_branch_without_merging_base() {
        let (workspace, pr) = prepare_workspace_scenario(false).await;
        let repo = workspace.root.join("symphony");

        let report = sync_workspace(&repo, workspace.git_transport, &pr)
            .await
            .expect("workspace sync should succeed");

        assert!(
            report.fetched_pr_branch,
            "workspace sync should fetch and reset the PR branch"
        );
        assert!(
            report.fetched_base_branch,
            "workspace sync should fetch the latest base branch ref"
        );
        assert!(
            !report.resumed_conflict_workspace,
            "a clean sync should not look like a resumed conflict workspace",
        );
        assert!(
            !repo.join("base.txt").exists(),
            "runner should not merge the base branch into the workspace anymore",
        );
        assert!(
            !has_unmerged_paths(&repo)
                .await
                .expect("unmerged path detection should work"),
            "plain workspace sync should not leave merge conflicts behind",
        );

        let current_branch =
            run_command_capture("git", &["rev-parse", "--abbrev-ref", "HEAD"], Some(&repo))
                .await
                .expect("current branch should resolve");
        assert_eq!(current_branch, pr.head_ref);
    }
}
