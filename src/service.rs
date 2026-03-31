use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use tokio::sync::{mpsc, Mutex as AsyncMutex};

use crate::{
    config::ResolvedConfig,
    model::{
        AttentionReason, DashboardState, EventLevel, PersistentPrState, PullRequest, RunnerState,
        TrackedPr, TrackingStatus,
    },
    runner::{RunOutcome, RunRequest},
    state_store::{PersistentStateFile, StateStore},
};

pub trait GitHubProvider: Send + Sync {
    fn fetch_pull_requests(&self) -> BoxFuture<'_, Result<Vec<PullRequest>>>;
}

pub trait AgentRunner: Send + Sync {
    fn run(&self, request: RunRequest) -> BoxFuture<'static, RunOutcome>;
}

const MAX_AUTOMATIC_RETRIES: u32 = 5;
const MAX_LIVE_OUTPUT_CHARS: usize = 16_000;

#[derive(Debug, Clone)]
struct ActiveRun {
    started_at: DateTime<Utc>,
    trigger: AttentionReason,
}

#[derive(Debug)]
struct SupervisorInner {
    persisted_state: PersistentStateFile,
    active_runs: HashMap<String, ActiveRun>,
}

pub struct Supervisor {
    config: ResolvedConfig,
    provider: Arc<dyn GitHubProvider>,
    runner: Arc<dyn AgentRunner>,
    store: StateStore,
    shared_state: Arc<Mutex<DashboardState>>,
    inner: AsyncMutex<SupervisorInner>,
}

impl Supervisor {
    pub fn new(
        config: ResolvedConfig,
        provider: Arc<dyn GitHubProvider>,
        runner: Arc<dyn AgentRunner>,
    ) -> Result<Self> {
        let store = StateStore::new(&config.state_path);
        let persisted_state = store.load()?;

        Ok(Self {
            config,
            provider,
            runner,
            store,
            shared_state: Arc::new(Mutex::new(DashboardState::default())),
            inner: AsyncMutex::new(SupervisorInner {
                persisted_state,
                active_runs: HashMap::new(),
            }),
        })
    }

    pub fn poll_interval_secs(&self) -> u64 {
        self.config.daemon.poll_interval_secs.max(5)
    }

    pub fn shared_state(&self) -> Arc<Mutex<DashboardState>> {
        self.shared_state.clone()
    }

    pub fn snapshot(&self) -> DashboardState {
        self.shared_state
            .lock()
            .expect("dashboard state mutex poisoned")
            .clone()
    }

    pub fn push_event(
        &self,
        level: EventLevel,
        pr_key: Option<String>,
        message: impl Into<String>,
    ) {
        let mut state = self
            .shared_state
            .lock()
            .expect("dashboard state mutex poisoned");
        state.push_event(level, pr_key, message);
    }

    pub async fn set_pr_paused(&self, pr_key: &str, paused: bool) -> Result<Option<TrackedPr>> {
        let (changed, updated) = {
            let mut inner = self.inner.lock().await;
            let mut state = self
                .shared_state
                .lock()
                .expect("dashboard state mutex poisoned");

            let Some(tracked) = state.tracked_prs.get_mut(pr_key) else {
                return Ok(None);
            };

            let previous = inner
                .persisted_state
                .prs
                .entry(pr_key.to_owned())
                .or_default()
                .clone();
            let changed = previous.paused != paused;

            if changed {
                let persisted = inner
                    .persisted_state
                    .prs
                    .entry(pr_key.to_owned())
                    .or_default();
                persisted.paused = paused;
                if !paused {
                    persisted.clear_retry_state();
                }

                if let Err(error) = self.store.save(&inner.persisted_state) {
                    *inner
                        .persisted_state
                        .prs
                        .entry(pr_key.to_owned())
                        .or_default() = previous;
                    return Err(error);
                }
            }

            tracked.persisted = inner
                .persisted_state
                .prs
                .get(pr_key)
                .cloned()
                .unwrap_or_default();
            tracked.attention_reason =
                determine_attention_reason(&tracked.pull_request, &tracked.persisted);
            tracked.status = derive_status(
                &tracked.pull_request,
                &tracked.persisted,
                tracked.attention_reason,
                tracked.runner.is_some(),
            );

            (changed, tracked.clone())
        };

        if changed {
            self.push_event(
                EventLevel::Info,
                Some(pr_key.to_owned()),
                if paused {
                    format!("paused review tracking for {pr_key}")
                } else {
                    format!("resumed review tracking for {pr_key}")
                },
            );
        }

        Ok(Some(updated))
    }

    pub async fn poll_once(&self) -> Result<()> {
        {
            let mut state = self
                .shared_state
                .lock()
                .expect("dashboard state mutex poisoned");
            state.last_poll_started_at = Some(Utc::now());
            state.last_poll_error = None;
        }

        let result = self.poll_once_inner().await;
        let finished_at = Utc::now();
        let next_poll_due_at = finished_at
            + chrono::Duration::seconds(
                self.config.daemon.poll_interval_secs.min(i64::MAX as u64) as i64
            );

        let mut state = self
            .shared_state
            .lock()
            .expect("dashboard state mutex poisoned");
        state.last_poll_finished_at = Some(finished_at);
        state.next_poll_due_at = Some(next_poll_due_at);

        match &result {
            Ok(()) => {
                state.last_poll_error = None;
            }
            Err(error) => {
                state.last_poll_error = Some(error.to_string());
                state.push_event(EventLevel::Error, None, format!("poll failed: {error:#}"));
            }
        }

        result
    }

    async fn poll_once_inner(&self) -> Result<()> {
        let prs = self.provider.fetch_pull_requests().await?;

        let selected_request = {
            let mut inner = self.inner.lock().await;
            refresh_dashboard(
                &self.shared_state,
                &prs,
                &inner.persisted_state,
                &inner.active_runs,
                self.config.daemon.poll_interval_secs,
            );

            let selected = select_run_request(
                &self.config,
                &prs,
                &inner.persisted_state,
                &inner.active_runs,
            );

            if let Some((pr, trigger)) = selected {
                let started_at = Utc::now();
                inner.active_runs.insert(
                    pr.key.clone(),
                    ActiveRun {
                        started_at,
                        trigger,
                    },
                );
                refresh_dashboard(
                    &self.shared_state,
                    &prs,
                    &inner.persisted_state,
                    &inner.active_runs,
                    self.config.daemon.poll_interval_secs,
                );
                self.push_event(
                    EventLevel::Info,
                    Some(pr.key.clone()),
                    format!(
                        "starting agent run for {} because {}",
                        pr.key,
                        trigger.label()
                    ),
                );

                Some(RunRequest {
                    pull_request: pr.clone(),
                    trigger,
                    workspace: self.config.workspace.clone(),
                    agent: self.config.agent.clone(),
                    output_updates: None,
                })
            } else {
                None
            }
        };

        if let Some(request) = selected_request {
            let pr_key = request.pull_request.key.clone();
            let (output_tx, mut output_rx) = mpsc::unbounded_channel::<String>();
            let mut runner_request = request.clone();
            runner_request.output_updates = Some(output_tx);
            let output_pr_key = pr_key.clone();
            let shared_state = self.shared_state.clone();
            let output_forwarder = tokio::spawn(async move {
                let mut buffered_output = String::new();
                while let Some(chunk) = output_rx.recv().await {
                    append_live_output(&mut buffered_output, &chunk);
                    let mut state = shared_state.lock().expect("dashboard state mutex poisoned");
                    if let Some(tracked) = state.tracked_prs.get_mut(&output_pr_key) {
                        if let Some(runner) = tracked.runner.as_mut() {
                            runner.live_output = Some(buffered_output.clone());
                        }
                    }
                }
            });

            let outcome = self.runner.run(runner_request).await;
            output_forwarder.await.ok();

            let mut inner = self.inner.lock().await;
            let active_run = inner.active_runs.remove(&pr_key);
            let mut auto_paused = false;
            {
                let persisted = inner.persisted_state.prs.entry(pr_key.clone()).or_default();
                persisted.last_run_started_at = Some(outcome.started_at);
                persisted.last_run_finished_at = Some(outcome.finished_at);
                persisted.last_run_status =
                    Some(if outcome.success { "success" } else { "error" }.to_owned());
                persisted.last_run_summary = Some(outcome.summary.clone());
                persisted.last_run_trigger = active_run.as_ref().map(|run| run.trigger);
                if outcome.success {
                    record_successful_run(persisted, &request, &outcome);
                } else {
                    auto_paused = record_failed_run(persisted, &request);
                }
            }

            self.store.save(&inner.persisted_state)?;

            refresh_dashboard(
                &self.shared_state,
                &prs,
                &inner.persisted_state,
                &inner.active_runs,
                self.config.daemon.poll_interval_secs,
            );

            self.push_event(
                if outcome.success {
                    EventLevel::Info
                } else {
                    EventLevel::Error
                },
                Some(pr_key.clone()),
                if outcome.success {
                    format!("agent run completed for {pr_key}")
                } else {
                    format!("agent run failed for {pr_key}: {}", outcome.summary)
                },
            );

            if auto_paused {
                self.push_event(
                    EventLevel::Error,
                    Some(pr_key.clone()),
                    format!("auto-paused {pr_key} after {MAX_AUTOMATIC_RETRIES} retry attempts"),
                );
            }
        }

        Ok(())
    }
}

fn select_run_request<'a>(
    config: &ResolvedConfig,
    prs: &'a [PullRequest],
    persisted_state: &PersistentStateFile,
    active_runs: &HashMap<String, ActiveRun>,
) -> Option<(&'a PullRequest, AttentionReason)> {
    if active_runs.len() >= config.daemon.max_concurrent_runs {
        return None;
    }

    prs.iter().find_map(|pr| {
        if active_runs.contains_key(&pr.key) {
            return None;
        }

        let persisted = persisted_state
            .prs
            .get(&pr.key)
            .cloned()
            .unwrap_or_default();
        if persisted.paused {
            return None;
        }
        determine_attention_reason(pr, &persisted).map(|reason| (pr, reason))
    })
}

fn refresh_dashboard(
    shared_state: &Arc<Mutex<DashboardState>>,
    prs: &[PullRequest],
    persisted_state: &PersistentStateFile,
    active_runs: &HashMap<String, ActiveRun>,
    poll_interval_secs: u64,
) {
    let tracked = build_tracked_prs(prs, persisted_state, active_runs);

    let mut state = shared_state.lock().expect("dashboard state mutex poisoned");
    state.tracked_prs = tracked;
    state.next_poll_due_at = Some(
        Utc::now() + chrono::Duration::seconds(poll_interval_secs.min(i64::MAX as u64) as i64),
    );
}

fn build_tracked_prs(
    prs: &[PullRequest],
    persisted_state: &PersistentStateFile,
    active_runs: &HashMap<String, ActiveRun>,
) -> BTreeMap<String, TrackedPr> {
    let mut tracked = BTreeMap::new();

    for pr in prs {
        let persisted = persisted_state
            .prs
            .get(&pr.key)
            .cloned()
            .unwrap_or_default();
        let attention_reason = determine_attention_reason(pr, &persisted);
        let active_run = active_runs.get(&pr.key);

        tracked.insert(
            pr.key.clone(),
            TrackedPr {
                pull_request: pr.clone(),
                status: derive_status(pr, &persisted, attention_reason, active_run.is_some()),
                attention_reason,
                persisted,
                runner: active_run.map(|active| RunnerState {
                    status: TrackingStatus::Running,
                    started_at: active.started_at,
                    finished_at: None,
                    attempt: 1,
                    trigger: active.trigger,
                    summary: "waiting for Codex CLI output...".to_owned(),
                    live_output: None,
                    exit_code: None,
                }),
            },
        );
    }

    tracked
}

pub fn determine_attention_reason(
    pr: &PullRequest,
    persisted: &PersistentPrState,
) -> Option<AttentionReason> {
    if pr.is_draft || pr.is_closed || pr.is_merged {
        return None;
    }

    let has_new_conflict = pr.has_conflicts
        && match (
            persisted.processed_conflict_head_sha(),
            persisted.processed_conflict_base_sha(),
        ) {
            (Some(last_head_sha), Some(last_base_sha)) => {
                last_head_sha != pr.head_sha.as_str() || last_base_sha != pr.base_sha.as_str()
            }
            _ => true,
        };

    if has_new_conflict {
        return Some(AttentionReason::MergeConflict);
    }

    let has_new_feedback = pr
        .latest_reviewer_activity_at
        .zip(persisted.processed_review_comment_at())
        .map(|(latest, processed)| latest > processed)
        .unwrap_or(pr.latest_reviewer_activity_at.is_some());

    if has_new_feedback
        && matches!(
            pr.review_decision,
            crate::model::ReviewDecision::Commented
                | crate::model::ReviewDecision::ChangesRequested
        )
    {
        return Some(AttentionReason::ReviewFeedback);
    }

    let ci_changed = match (
        persisted.processed_ci_head_sha(),
        pr.ci_updated_at,
        persisted.processed_ci_signal_at(),
    ) {
        (Some(last_sha), Some(latest_ci_at), Some(processed_ci_at)) => {
            last_sha != pr.head_sha.as_str() || latest_ci_at > processed_ci_at
        }
        _ => pr.ci_updated_at.is_some(),
    };

    if ci_changed && matches!(pr.ci_status, crate::model::CiStatus::Failure) {
        return Some(AttentionReason::CiFailed);
    }

    None
}

fn derive_status(
    pr: &PullRequest,
    persisted: &PersistentPrState,
    attention_reason: Option<AttentionReason>,
    is_running: bool,
) -> TrackingStatus {
    if is_running {
        TrackingStatus::Running
    } else if pr.is_merged {
        TrackingStatus::Merged
    } else if pr.is_closed {
        TrackingStatus::Closed
    } else if persisted.paused {
        TrackingStatus::Paused
    } else if pr.is_draft {
        TrackingStatus::Draft
    } else if let Some(reason) = attention_reason {
        if is_retry_scheduled(pr, persisted, reason) {
            TrackingStatus::RetryScheduled
        } else {
            match reason {
                AttentionReason::MergeConflict => TrackingStatus::Conflict,
                _ => TrackingStatus::NeedsAttention,
            }
        }
    } else if pr.has_conflicts {
        TrackingStatus::Conflict
    } else if is_waiting_merge(pr) {
        TrackingStatus::WaitingMerge
    } else {
        TrackingStatus::WaitingReview
    }
}

fn is_waiting_merge(pr: &PullRequest) -> bool {
    !pr.is_draft
        && !pr.is_closed
        && !pr.is_merged
        && !pr.has_conflicts
        && pr.approval_count > 0
        && matches!(pr.review_decision, crate::model::ReviewDecision::Approved)
        && matches!(pr.ci_status, crate::model::CiStatus::Success)
}

fn is_retry_scheduled(
    pr: &PullRequest,
    persisted: &PersistentPrState,
    reason: AttentionReason,
) -> bool {
    persisted.last_run_status.as_deref() == Some("error")
        && persisted.consecutive_failures > 0
        && persisted.consecutive_failures <= MAX_AUTOMATIC_RETRIES
        && retry_signal_matches(pr, persisted, reason)
}

fn retry_signal_matches(
    pr: &PullRequest,
    persisted: &PersistentPrState,
    reason: AttentionReason,
) -> bool {
    if persisted.retry_trigger != Some(reason) {
        return false;
    }

    if persisted.retry_head_sha.as_deref() != Some(pr.head_sha.as_str()) {
        return false;
    }

    match reason {
        AttentionReason::MergeConflict => {
            persisted.retry_base_sha.as_deref() == Some(pr.base_sha.as_str())
        }
        AttentionReason::ReviewFeedback => {
            persisted.retry_comment_at == pr.latest_reviewer_activity_at
        }
        AttentionReason::CiFailed => persisted.retry_ci_at == pr.ci_updated_at,
    }
}

fn record_failed_run(persisted: &mut PersistentPrState, request: &RunRequest) -> bool {
    persisted.consecutive_failures =
        if retry_signal_matches(&request.pull_request, persisted, request.trigger) {
            persisted.consecutive_failures.saturating_add(1).max(1)
        } else {
            1
        };
    persisted.retry_trigger = Some(request.trigger);
    persisted.retry_head_sha = Some(request.pull_request.head_sha.clone());
    persisted.retry_base_sha = Some(request.pull_request.base_sha.clone());
    persisted.retry_comment_at = request.pull_request.latest_reviewer_activity_at;
    persisted.retry_ci_at = request.pull_request.ci_updated_at;

    let retries_used = persisted.consecutive_failures.saturating_sub(1);
    let auto_paused = retries_used >= MAX_AUTOMATIC_RETRIES;
    if auto_paused {
        persisted.paused = true;
    }

    auto_paused
}

fn record_successful_run(
    persisted: &mut PersistentPrState,
    request: &RunRequest,
    outcome: &RunOutcome,
) {
    match request.trigger {
        AttentionReason::ReviewFeedback => {
            persisted.last_processed_review_comment_at = outcome.processed_comment_at;
            persisted.last_processed_comment_at = outcome.processed_comment_at;
        }
        AttentionReason::CiFailed => {
            persisted.last_processed_ci_signal_at = outcome.processed_ci_at;
            persisted.last_processed_ci_head_sha = Some(outcome.processed_head_sha.clone());
            persisted.last_processed_ci_at = outcome.processed_ci_at;
            persisted.last_processed_head_sha = Some(outcome.processed_head_sha.clone());
        }
        AttentionReason::MergeConflict => {
            persisted.last_processed_conflict_head_sha = Some(outcome.processed_head_sha.clone());
            persisted.last_processed_conflict_base_sha =
                Some(request.pull_request.base_sha.clone());
            persisted.last_processed_head_sha = Some(outcome.processed_head_sha.clone());
            persisted.last_processed_base_sha = Some(request.pull_request.base_sha.clone());
        }
    }

    persisted.clear_retry_state();
}

fn append_live_output(buffer: &mut String, chunk: &str) {
    buffer.push_str(chunk);

    let total_chars = buffer.chars().count();
    if total_chars <= MAX_LIVE_OUTPUT_CHARS {
        return;
    }

    let trim_chars = total_chars - MAX_LIVE_OUTPUT_CHARS;
    if let Some((trim_at, _)) = buffer.char_indices().nth(trim_chars) {
        buffer.drain(..trim_at);
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::{
        config::{
            AgentConfig, DaemonConfig, GitTransport, ResolvedConfig, ResolvedGitHubConfig,
            ResolvedWorkspaceConfig, UiConfig,
        },
        model::{CiStatus, PullRequest, ReviewDecision},
        state_store::PersistentStateFile,
    };

    fn sample_pr() -> PullRequest {
        PullRequest {
            key: "openai/symphony#42".to_owned(),
            repo_full_name: "openai/symphony".to_owned(),
            number: 42,
            title: "Fix poller".to_owned(),
            body: None,
            url: "https://github.com/openai/symphony/pull/42".to_owned(),
            author_login: "connor".to_owned(),
            labels: vec![],
            created_at: Utc.with_ymd_and_hms(2026, 3, 30, 18, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 3, 30, 18, 0, 0).unwrap(),
            head_sha: "abc123".to_owned(),
            head_ref: "feature".to_owned(),
            base_sha: "def456".to_owned(),
            base_ref: "main".to_owned(),
            clone_url: "https://github.com/openai/symphony.git".to_owned(),
            ssh_url: "git@github.com:openai/symphony.git".to_owned(),
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

    fn sample_config() -> ResolvedConfig {
        ResolvedConfig {
            github: ResolvedGitHubConfig {
                api_token: "token".to_owned(),
                api_base_url: "https://api.github.test".to_owned(),
                author: Some("connor".to_owned()),
                query: None,
                max_prs: 5,
            },
            daemon: DaemonConfig {
                poll_interval_secs: 30,
                max_concurrent_runs: 1,
            },
            workspace: ResolvedWorkspaceConfig {
                root: PathBuf::from("/tmp/workspaces"),
                repo_map: std::collections::BTreeMap::new(),
                git_transport: GitTransport::Https,
            },
            agent: AgentConfig {
                command: "codex".to_owned(),
                args: vec![],
                additional_instructions: None,
            },
            ui: UiConfig::default(),
            state_path: PathBuf::from("/tmp/state.json"),
        }
    }

    #[test]
    fn review_feedback_after_last_processed_timestamp_triggers_attention() {
        let mut pr = sample_pr();
        pr.review_decision = ReviewDecision::ChangesRequested;
        pr.latest_reviewer_activity_at = Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 5, 0).unwrap());

        let persisted = PersistentPrState {
            last_processed_comment_at: Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 0, 0).unwrap()),
            ..PersistentPrState::default()
        };

        assert_eq!(
            determine_attention_reason(&pr, &persisted),
            Some(AttentionReason::ReviewFeedback)
        );
    }

    #[test]
    fn unchanged_failing_ci_does_not_requeue_same_sha_forever() {
        let mut pr = sample_pr();
        pr.ci_status = CiStatus::Failure;
        pr.ci_updated_at = Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 5, 0).unwrap());

        let persisted = PersistentPrState {
            last_processed_ci_signal_at: pr.ci_updated_at,
            last_processed_ci_head_sha: Some(pr.head_sha.clone()),
            ..PersistentPrState::default()
        };

        assert_eq!(determine_attention_reason(&pr, &persisted), None);
    }

    #[test]
    fn successful_review_run_does_not_consume_failed_ci_signal() {
        let mut pr = sample_pr();
        pr.ci_status = CiStatus::Failure;
        pr.ci_updated_at = Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 6, 0).unwrap());
        pr.review_decision = ReviewDecision::ChangesRequested;
        pr.latest_reviewer_activity_at = Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 5, 0).unwrap());

        let config = sample_config();
        let request = RunRequest {
            pull_request: pr.clone(),
            trigger: AttentionReason::ReviewFeedback,
            workspace: config.workspace,
            agent: config.agent,
            output_updates: None,
        };
        let outcome = RunOutcome {
            started_at: Utc::now(),
            finished_at: Utc::now(),
            success: true,
            exit_code: Some(0),
            summary: "review addressed".to_owned(),
            processed_comment_at: pr.latest_reviewer_activity_at,
            processed_ci_at: pr.ci_updated_at,
            processed_head_sha: pr.head_sha.clone(),
        };
        let mut persisted = PersistentPrState::default();

        record_successful_run(&mut persisted, &request, &outcome);

        assert_eq!(
            persisted.last_processed_review_comment_at,
            pr.latest_reviewer_activity_at
        );
        assert_eq!(persisted.last_processed_ci_signal_at, None);
        assert_eq!(
            determine_attention_reason(&pr, &persisted),
            Some(AttentionReason::CiFailed)
        );
    }

    #[test]
    fn legacy_ci_marker_from_non_ci_run_is_ignored() {
        let mut pr = sample_pr();
        pr.ci_status = CiStatus::Failure;
        pr.ci_updated_at = Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 5, 0).unwrap());

        let persisted = PersistentPrState {
            last_processed_ci_at: pr.ci_updated_at,
            last_processed_head_sha: Some(pr.head_sha.clone()),
            last_run_status: Some("error".to_owned()),
            last_run_trigger: Some(AttentionReason::ReviewFeedback),
            ..PersistentPrState::default()
        };

        assert_eq!(
            determine_attention_reason(&pr, &persisted),
            Some(AttentionReason::CiFailed)
        );
    }

    #[test]
    fn paused_prs_are_not_selected_for_runs() {
        let mut pr = sample_pr();
        pr.ci_status = CiStatus::Failure;
        pr.ci_updated_at = Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 5, 0).unwrap());

        let persisted_state = PersistentStateFile {
            prs: [(
                pr.key.clone(),
                PersistentPrState {
                    paused: true,
                    ..PersistentPrState::default()
                },
            )]
            .into_iter()
            .collect(),
        };

        assert!(
            select_run_request(&sample_config(), &[pr], &persisted_state, &HashMap::new())
                .is_none(),
            "paused PR should never be scheduled automatically",
        );
    }

    #[test]
    fn paused_prs_report_paused_status() {
        let pr = sample_pr();
        let persisted = PersistentPrState {
            paused: true,
            ..PersistentPrState::default()
        };

        assert_eq!(
            derive_status(&pr, &persisted, None, false),
            TrackingStatus::Paused
        );
        assert_eq!(
            derive_status(&pr, &persisted, Some(AttentionReason::CiFailed), true),
            TrackingStatus::Running
        );
    }

    #[test]
    fn failed_actionable_prs_report_retry_scheduled_until_retry_budget_is_exhausted() {
        let mut pr = sample_pr();
        pr.ci_status = CiStatus::Failure;
        pr.ci_updated_at = Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 5, 0).unwrap());

        let persisted = PersistentPrState {
            last_run_status: Some("error".to_owned()),
            consecutive_failures: 2,
            retry_trigger: Some(AttentionReason::CiFailed),
            retry_head_sha: Some(pr.head_sha.clone()),
            retry_ci_at: pr.ci_updated_at,
            ..PersistentPrState::default()
        };

        assert_eq!(
            derive_status(&pr, &persisted, Some(AttentionReason::CiFailed), false),
            TrackingStatus::RetryScheduled
        );
    }

    #[test]
    fn stale_execution_error_without_current_attention_returns_waiting_review() {
        let pr = sample_pr();
        let persisted = PersistentPrState {
            last_run_status: Some("error".to_owned()),
            consecutive_failures: 3,
            retry_trigger: Some(AttentionReason::CiFailed),
            retry_head_sha: Some(pr.head_sha.clone()),
            retry_ci_at: Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 5, 0).unwrap()),
            ..PersistentPrState::default()
        };

        assert_eq!(
            derive_status(&pr, &persisted, None, false),
            TrackingStatus::WaitingReview
        );
    }

    #[test]
    fn approved_green_pr_reports_waiting_merge() {
        let mut pr = sample_pr();
        pr.review_decision = ReviewDecision::Approved;
        pr.approval_count = 1;

        assert_eq!(
            derive_status(&pr, &PersistentPrState::default(), None, false),
            TrackingStatus::WaitingMerge
        );
    }

    #[test]
    fn approved_pr_without_green_ci_stays_waiting_review() {
        let mut pr = sample_pr();
        pr.review_decision = ReviewDecision::Approved;
        pr.approval_count = 1;
        pr.ci_status = CiStatus::Pending;

        assert_eq!(
            derive_status(&pr, &PersistentPrState::default(), None, false),
            TrackingStatus::WaitingReview
        );
    }

    #[test]
    fn merge_conflict_triggers_conflict_status() {
        let mut pr = sample_pr();
        pr.has_conflicts = true;
        pr.mergeable_state = Some("dirty".to_owned());

        assert_eq!(
            determine_attention_reason(&pr, &PersistentPrState::default()),
            Some(AttentionReason::MergeConflict)
        );
        assert_eq!(
            derive_status(
                &pr,
                &PersistentPrState::default(),
                Some(AttentionReason::MergeConflict),
                false,
            ),
            TrackingStatus::Conflict
        );
    }

    #[test]
    fn processed_conflict_for_same_head_and_base_does_not_retrigger() {
        let mut pr = sample_pr();
        pr.has_conflicts = true;

        let persisted = PersistentPrState {
            last_processed_conflict_head_sha: Some(pr.head_sha.clone()),
            last_processed_conflict_base_sha: Some(pr.base_sha.clone()),
            ..PersistentPrState::default()
        };

        assert_eq!(determine_attention_reason(&pr, &persisted), None);
        assert_eq!(
            derive_status(&pr, &persisted, None, false),
            TrackingStatus::Conflict
        );
    }
}
