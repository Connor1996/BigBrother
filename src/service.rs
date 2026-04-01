use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
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
    runner::{
        RunOutcome, RunRequest, RunUpdate, OUTPUT_TRANSCRIPT_HEADER, PROMPT_TRANSCRIPT_HEADER,
    },
    state_store::{PersistentStateFile, StateStore},
};

pub trait GitHubProvider: Send + Sync {
    fn fetch_pull_requests(&self) -> BoxFuture<'_, Result<Vec<PullRequest>>>;

    fn fetch_pull_requests_with_state(
        &self,
        poll_state: PollQueryState,
    ) -> BoxFuture<'_, Result<Vec<PullRequest>>> {
        let _ = poll_state;
        Box::pin(async move { self.fetch_pull_requests().await })
    }

    fn fetch_pull_requests_with_state_and_stats(
        &self,
        poll_state: PollQueryState,
    ) -> BoxFuture<'_, Result<(Vec<PullRequest>, GitHubRequestStats)>> {
        Box::pin(async move {
            Ok((
                self.fetch_pull_requests_with_state(poll_state).await?,
                GitHubRequestStats::default(),
            ))
        })
    }

    fn fetch_pull_request(&self, pr_key: String) -> BoxFuture<'_, Result<Option<PullRequest>>> {
        Box::pin(async move {
            Ok(self
                .fetch_pull_requests()
                .await?
                .into_iter()
                .find(|pr| pr.key == pr_key))
        })
    }

    fn fetch_pull_request_with_stats(
        &self,
        pr_key: String,
    ) -> BoxFuture<'_, Result<(Option<PullRequest>, GitHubRequestStats)>> {
        Box::pin(async move {
            Ok((
                self.fetch_pull_request(pr_key).await?,
                GitHubRequestStats::default(),
            ))
        })
    }
}

pub trait AgentRunner: Send + Sync {
    fn run(&self, request: RunRequest) -> BoxFuture<'static, RunOutcome>;
}

#[derive(Debug, Clone, Default)]
pub struct PollQueryState {
    pub previous_prs: Vec<PullRequest>,
    pub frozen_pr_keys: BTreeSet<String>,
}

const MAX_AUTOMATIC_RETRIES: u32 = 5;
const MAX_LIVE_OUTPUT_CHARS: usize = 16_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitHubRequestStats {
    pub viewer_requests: usize,
    pub search_requests: usize,
    pub pull_detail_requests: usize,
    pub review_requests: usize,
    pub review_comment_requests: usize,
    pub issue_comment_requests: usize,
    pub check_run_requests: usize,
    pub combined_status_requests: usize,
    pub light_prs: usize,
    pub hydrated_prs: usize,
    pub reused_prs: usize,
}

impl GitHubRequestStats {
    pub fn has_metrics(&self) -> bool {
        self.total_requests() > 0
            || self.light_prs > 0
            || self.hydrated_prs > 0
            || self.reused_prs > 0
    }

    pub fn total_requests(&self) -> usize {
        self.viewer_requests
            + self.search_requests
            + self.pull_detail_requests
            + self.review_requests
            + self.review_comment_requests
            + self.issue_comment_requests
            + self.check_run_requests
            + self.combined_status_requests
    }

    pub fn activity_message(
        &self,
        fallback_pr_count: usize,
        preferred_pr_key: Option<&str>,
    ) -> String {
        let scope = preferred_pr_key
            .map(|pr_key| format!("targeted check for {pr_key}"))
            .unwrap_or_else(|| "scheduled poll".to_owned());
        let fetched_prs = if self.light_prs > 0 {
            self.light_prs
        } else {
            fallback_pr_count
        };

        if !self.has_metrics() {
            return format!(
                "{scope} fetched {fetched_prs} PRs without provider-side GitHub request metrics"
            );
        }

        let mut request_parts = Vec::new();
        push_metric_part(&mut request_parts, "viewer", self.viewer_requests);
        push_metric_part(&mut request_parts, "search", self.search_requests);
        push_metric_part(&mut request_parts, "pull detail", self.pull_detail_requests);
        push_metric_part(&mut request_parts, "reviews", self.review_requests);
        push_metric_part(
            &mut request_parts,
            "review comments",
            self.review_comment_requests,
        );
        push_metric_part(
            &mut request_parts,
            "issue comments",
            self.issue_comment_requests,
        );
        push_metric_part(&mut request_parts, "check runs", self.check_run_requests);
        push_metric_part(
            &mut request_parts,
            "combined status",
            self.combined_status_requests,
        );

        let mut outcome_parts = Vec::new();
        push_metric_part(&mut outcome_parts, "hydrated PR", self.hydrated_prs);
        push_metric_part(&mut outcome_parts, "reused PR", self.reused_prs);

        let total_requests = self.total_requests();
        let request_label = if total_requests == 1 {
            "request"
        } else {
            "requests"
        };

        if outcome_parts.is_empty() {
            format!(
                "{scope} fetched {fetched_prs} PRs using {total_requests} GitHub {request_label} ({})",
                request_parts.join(", ")
            )
        } else {
            format!(
                "{scope} fetched {fetched_prs} PRs using {total_requests} GitHub {request_label} ({}; {})",
                request_parts.join(", "),
                outcome_parts.join(", ")
            )
        }
    }
}

fn push_metric_part(parts: &mut Vec<String>, label: &str, count: usize) {
    if count == 0 {
        return;
    }

    parts.push(format!("{label} {count}"));
}

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
    poll_lock: AsyncMutex<()>,
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
            poll_lock: AsyncMutex::new(()),
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

    pub async fn set_pr_paused(
        self: &Arc<Self>,
        pr_key: &str,
        paused: bool,
    ) -> Result<Option<TrackedPr>> {
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

        if changed && !paused {
            self.schedule_immediate_check(pr_key.to_owned());
        }

        Ok(Some(updated))
    }

    pub async fn poll_once(&self) -> Result<()> {
        self.poll_once_with_selection(None, true).await
    }

    async fn poll_once_for_pr(&self, pr_key: &str) -> Result<()> {
        self.poll_once_with_selection(Some(pr_key), false).await
    }

    async fn poll_once_with_selection(
        &self,
        preferred_pr_key: Option<&str>,
        allow_fallback: bool,
    ) -> Result<()> {
        let _poll_guard = self.poll_lock.lock().await;
        {
            let mut state = self
                .shared_state
                .lock()
                .expect("dashboard state mutex poisoned");
            state.last_poll_started_at = Some(Utc::now());
            state.last_poll_error = None;
        }
        let result = self.poll_once_inner(preferred_pr_key, allow_fallback).await;
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

    fn schedule_immediate_check(self: &Arc<Self>, pr_key: String) {
        let supervisor = Arc::clone(self);
        tokio::spawn(async move {
            supervisor.push_event(
                EventLevel::Info,
                Some(pr_key.clone()),
                format!("triggering immediate check for {pr_key} after resume"),
            );

            if let Err(error) = supervisor.poll_once_for_pr(&pr_key).await {
                supervisor.push_event(
                    EventLevel::Error,
                    Some(pr_key.clone()),
                    format!("immediate check failed for {pr_key}: {error:#}"),
                );
            }
        });
    }

    async fn poll_once_inner(
        &self,
        preferred_pr_key: Option<&str>,
        allow_fallback: bool,
    ) -> Result<()> {
        self.push_event(
            EventLevel::Info,
            preferred_pr_key.map(str::to_owned),
            match preferred_pr_key {
                Some(pr_key) => format!("starting targeted daemon check for {pr_key}"),
                None => "starting scheduled daemon poll".to_owned(),
            },
        );

        let (prs, fetch_stats) = self
            .fetch_prs_for_poll(preferred_pr_key, allow_fallback)
            .await?;
        if fetch_stats.has_metrics() {
            self.push_event(
                EventLevel::Info,
                preferred_pr_key.map(str::to_owned),
                fetch_stats.activity_message(prs.len(), preferred_pr_key),
            );
        }

        let selected_request = {
            let mut inner = self.inner.lock().await;
            let active_run_count = inner.active_runs.len();
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
                preferred_pr_key,
                allow_fallback,
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
                if let Some(pr_key) = preferred_pr_key {
                    self.push_event(
                        EventLevel::Info,
                        Some(pr_key.to_owned()),
                        if active_run_count >= self.config.daemon.max_concurrent_runs {
                            format!(
                                "immediate re-check for {pr_key} is waiting for a free runner slot"
                            )
                        } else {
                            format!("immediate re-check found no actionable work for {pr_key}")
                        },
                    );
                } else if active_run_count >= self.config.daemon.max_concurrent_runs {
                    self.push_event(
                        EventLevel::Info,
                        None,
                        format!(
                            "daemon poll is waiting for a free runner slot ({active_run_count}/{} active)",
                            self.config.daemon.max_concurrent_runs
                        ),
                    );
                } else {
                    self.push_event(
                        EventLevel::Info,
                        None,
                        format!(
                            "daemon poll found no actionable PRs among {} tracked PRs",
                            prs.len()
                        ),
                    );
                }
                None
            }
        };

        if let Some(request) = selected_request {
            let pr_key = request.pull_request.key.clone();
            let (output_tx, mut output_rx) = mpsc::unbounded_channel::<RunUpdate>();
            let mut runner_request = request.clone();
            runner_request.output_updates = Some(output_tx);
            let output_pr_key = pr_key.clone();
            let shared_state = self.shared_state.clone();
            let output_forwarder = tokio::spawn(async move {
                let mut buffered_output = String::new();
                while let Some(update) = output_rx.recv().await {
                    let mut state = shared_state.lock().expect("dashboard state mutex poisoned");
                    if let Some(tracked) = state.tracked_prs.get_mut(&output_pr_key) {
                        if let Some(runner) = tracked.runner.as_mut() {
                            match update {
                                RunUpdate::TranscriptChunk(chunk) => {
                                    append_live_output(&mut buffered_output, &chunk);
                                    runner.live_output = Some(buffered_output.clone());
                                }
                                RunUpdate::TerminalSnapshot {
                                    screen,
                                    last_output_at,
                                } => {
                                    runner.live_terminal = Some(screen);
                                    runner.last_terminal_output_at = Some(last_output_at);
                                }
                            }
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
                persisted.last_run_output = capped_run_output(outcome.captured_output.as_deref());
                persisted.last_run_terminal =
                    capped_run_output(outcome.captured_terminal.as_deref());
                persisted.last_terminal_output_at = outcome.last_terminal_output_at;
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

    async fn fetch_prs_for_poll(
        &self,
        preferred_pr_key: Option<&str>,
        allow_fallback: bool,
    ) -> Result<(Vec<PullRequest>, GitHubRequestStats)> {
        if let Some(pr_key) = preferred_pr_key {
            if !allow_fallback {
                let mut prs = current_poll_query_state(&self.shared_state).previous_prs;
                let (fetched, stats) = self
                    .provider
                    .fetch_pull_request_with_stats(pr_key.to_owned())
                    .await?;
                merge_targeted_pr_into_list(&mut prs, pr_key, fetched);
                return Ok((prs, stats));
            }
        }

        self.provider
            .fetch_pull_requests_with_state_and_stats(current_poll_query_state(&self.shared_state))
            .await
    }
}

fn select_run_request<'a>(
    config: &ResolvedConfig,
    prs: &'a [PullRequest],
    persisted_state: &PersistentStateFile,
    active_runs: &HashMap<String, ActiveRun>,
    preferred_pr_key: Option<&str>,
    allow_fallback: bool,
) -> Option<(&'a PullRequest, AttentionReason)> {
    if active_runs.len() >= config.daemon.max_concurrent_runs {
        return None;
    }

    if let Some(pr_key) = preferred_pr_key {
        if let Some(selected) = prs
            .iter()
            .find(|pr| pr.key == pr_key)
            .and_then(|pr| selectable_run_request(pr, persisted_state, active_runs))
        {
            return Some(selected);
        }

        if !allow_fallback {
            return None;
        }
    }

    prs.iter()
        .find_map(|pr| selectable_run_request(pr, persisted_state, active_runs))
}

fn selectable_run_request<'a>(
    pr: &'a PullRequest,
    persisted_state: &PersistentStateFile,
    active_runs: &HashMap<String, ActiveRun>,
) -> Option<(&'a PullRequest, AttentionReason)> {
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
}

fn refresh_dashboard(
    shared_state: &Arc<Mutex<DashboardState>>,
    prs: &[PullRequest],
    persisted_state: &PersistentStateFile,
    active_runs: &HashMap<String, ActiveRun>,
    poll_interval_secs: u64,
) {
    let mut state = shared_state.lock().expect("dashboard state mutex poisoned");
    let tracked = build_tracked_prs(prs, persisted_state, active_runs, &state.tracked_prs);
    state.tracked_prs = tracked;
    state.next_poll_due_at = Some(
        Utc::now() + chrono::Duration::seconds(poll_interval_secs.min(i64::MAX as u64) as i64),
    );
}

fn build_tracked_prs(
    prs: &[PullRequest],
    persisted_state: &PersistentStateFile,
    active_runs: &HashMap<String, ActiveRun>,
    previous_tracked: &BTreeMap<String, TrackedPr>,
) -> BTreeMap<String, TrackedPr> {
    let mut tracked = BTreeMap::new();

    for pr in prs {
        tracked.insert(
            pr.key.clone(),
            build_tracked_pr(pr, persisted_state, active_runs),
        );
    }

    for (pr_key, previous) in previous_tracked {
        if tracked.contains_key(pr_key) {
            continue;
        }

        let persisted = persisted_state.prs.get(pr_key).cloned().unwrap_or_default();
        if !persisted.paused {
            continue;
        }

        tracked.insert(
            pr_key.clone(),
            build_tracked_pr(&previous.pull_request, persisted_state, active_runs),
        );
    }

    tracked
}

fn build_tracked_pr(
    pr: &PullRequest,
    persisted_state: &PersistentStateFile,
    active_runs: &HashMap<String, ActiveRun>,
) -> TrackedPr {
    let persisted = persisted_state
        .prs
        .get(&pr.key)
        .cloned()
        .unwrap_or_default();
    let attention_reason = determine_attention_reason(pr, &persisted);
    let active_run = active_runs.get(&pr.key);

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
            summary: active.trigger.active_summary().to_owned(),
            live_output: None,
            live_terminal: None,
            last_terminal_output_at: None,
            exit_code: None,
        }),
    }
}

fn current_poll_query_state(shared_state: &Arc<Mutex<DashboardState>>) -> PollQueryState {
    let state = shared_state.lock().expect("dashboard state mutex poisoned");

    PollQueryState {
        previous_prs: state
            .tracked_prs
            .values()
            .map(|tracked| tracked.pull_request.clone())
            .collect(),
        frozen_pr_keys: state
            .tracked_prs
            .iter()
            .filter(|(_, tracked)| tracked.persisted.paused)
            .map(|(pr_key, _)| pr_key.clone())
            .collect(),
    }
}

fn merge_targeted_pr_into_list(
    prs: &mut Vec<PullRequest>,
    pr_key: &str,
    fetched: Option<PullRequest>,
) {
    match fetched {
        Some(pull_request) => {
            if let Some(existing) = prs.iter_mut().find(|pr| pr.key == pr_key) {
                *existing = pull_request;
            } else {
                prs.push(pull_request);
            }
        }
        None => prs.retain(|pr| pr.key != pr_key),
    }
}

pub fn determine_attention_reason(
    pr: &PullRequest,
    persisted: &PersistentPrState,
) -> Option<AttentionReason> {
    if pr.is_draft || pr.is_closed || pr.is_merged {
        return None;
    }

    if pr.has_conflicts {
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
        AttentionReason::MergeConflict => {}
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
    let trim_start_chars = pinned_transcript_prefix_chars(buffer).min(total_chars);
    let trim_end_chars = (trim_start_chars + trim_chars).min(total_chars);
    let trim_start = char_boundary(buffer, trim_start_chars);
    let trim_end = char_boundary(buffer, trim_end_chars);

    if trim_end > trim_start {
        buffer.drain(trim_start..trim_end);
    }
}

fn capped_run_output(output: Option<&str>) -> Option<String> {
    let output = output?;
    if output.trim().is_empty() {
        return None;
    }

    let mut capped = String::new();
    append_live_output(&mut capped, output);
    Some(capped)
}

fn pinned_transcript_prefix_chars(buffer: &str) -> usize {
    if !buffer.starts_with(PROMPT_TRANSCRIPT_HEADER) {
        return 0;
    }

    buffer
        .find(OUTPUT_TRANSCRIPT_HEADER)
        .map(|offset| {
            buffer[..offset + OUTPUT_TRANSCRIPT_HEADER.len()]
                .chars()
                .count()
        })
        .unwrap_or(0)
}

fn char_boundary(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(byte_index, _)| byte_index)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashMap},
        path::PathBuf,
    };

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
                dangerously_bypass_approvals_and_sandbox: false,
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
            captured_output: Some("codex: fixed review feedback\ncargo test -q".to_owned()),
            captured_terminal: Some("$ codex exec\nreview addressed".to_owned()),
            last_terminal_output_at: Some(Utc::now()),
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
    fn successful_conflict_run_does_not_write_processed_conflict_marker() {
        let mut pr = sample_pr();
        pr.has_conflicts = true;
        pr.mergeable_state = Some("dirty".to_owned());

        let config = sample_config();
        let request = RunRequest {
            pull_request: pr.clone(),
            trigger: AttentionReason::MergeConflict,
            workspace: config.workspace,
            agent: config.agent,
            output_updates: None,
        };
        let outcome = RunOutcome {
            started_at: Utc::now(),
            finished_at: Utc::now(),
            success: true,
            exit_code: Some(0),
            summary: "conflict analysis completed".to_owned(),
            captured_output: Some("codex: analyzed merge conflict".to_owned()),
            captured_terminal: Some("$ codex exec\nanalyzed merge conflict".to_owned()),
            last_terminal_output_at: Some(Utc::now()),
            processed_comment_at: pr.latest_reviewer_activity_at,
            processed_ci_at: pr.ci_updated_at,
            processed_head_sha: pr.head_sha.clone(),
        };
        let mut persisted = PersistentPrState::default();

        record_successful_run(&mut persisted, &request, &outcome);

        assert_eq!(persisted.last_processed_conflict_head_sha, None);
        assert_eq!(persisted.last_processed_conflict_base_sha, None);
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
            select_run_request(
                &sample_config(),
                &[pr],
                &persisted_state,
                &HashMap::new(),
                None,
                true,
            )
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
    fn build_tracked_prs_preserves_paused_snapshot_when_poll_omits_it() {
        let pr = sample_pr();
        let persisted = PersistentPrState {
            paused: true,
            ..PersistentPrState::default()
        };
        let persisted_state = PersistentStateFile {
            prs: [(pr.key.clone(), persisted.clone())].into_iter().collect(),
        };
        let previous_tracked = BTreeMap::from([(
            pr.key.clone(),
            TrackedPr {
                pull_request: pr.clone(),
                status: TrackingStatus::Paused,
                attention_reason: None,
                persisted,
                runner: None,
            },
        )]);

        let tracked = build_tracked_prs(&[], &persisted_state, &HashMap::new(), &previous_tracked);
        let frozen = tracked
            .get(&pr.key)
            .expect("paused PR should be preserved from the previous snapshot");

        assert_eq!(frozen.pull_request.title, pr.title);
        assert_eq!(frozen.status, TrackingStatus::Paused);
    }

    #[test]
    fn preferred_pr_is_selected_first_for_targeted_rechecks() {
        let mut first = sample_pr();
        first.key = "openai/symphony#1".to_owned();
        first.number = 1;
        first.ci_status = CiStatus::Failure;
        first.ci_updated_at = Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 5, 0).unwrap());

        let mut second = sample_pr();
        second.key = "openai/symphony#2".to_owned();
        second.number = 2;
        second.ci_status = CiStatus::Failure;
        second.ci_updated_at = Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 6, 0).unwrap());

        let prs = [first.clone(), second.clone()];
        let selected = select_run_request(
            &sample_config(),
            &prs,
            &PersistentStateFile::default(),
            &HashMap::new(),
            Some(&second.key),
            false,
        )
        .expect("the preferred actionable PR should be selected");

        assert_eq!(selected.0.key, second.key);
        assert_eq!(selected.1, AttentionReason::CiFailed);
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
    fn live_output_trimming_preserves_prompt_section() {
        let mut buffer = crate::runner::cli_transcript_preamble("Inspect workspace\nRun tests");
        let prompt_section = buffer.clone();

        append_live_output(&mut buffer, &"x".repeat(MAX_LIVE_OUTPUT_CHARS));

        assert!(buffer.starts_with(PROMPT_TRANSCRIPT_HEADER));
        assert!(
            buffer.contains(OUTPUT_TRANSCRIPT_HEADER),
            "trimmed transcript should still keep the Codex output divider",
        );
        assert!(
            buffer.contains(&prompt_section),
            "trimmed transcript should preserve the full prompt section",
        );
        assert!(
            buffer.chars().count() <= MAX_LIVE_OUTPUT_CHARS,
            "trimmed transcript should still respect the live-output cap",
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
    fn current_conflict_state_remains_actionable_even_with_legacy_processed_marker() {
        let mut pr = sample_pr();
        pr.has_conflicts = true;

        let persisted = PersistentPrState {
            last_processed_conflict_head_sha: Some(pr.head_sha.clone()),
            last_processed_conflict_base_sha: Some(pr.base_sha.clone()),
            ..PersistentPrState::default()
        };

        assert_eq!(
            determine_attention_reason(&pr, &persisted),
            Some(AttentionReason::MergeConflict)
        );
        assert_eq!(
            derive_status(&pr, &persisted, Some(AttentionReason::MergeConflict), false,),
            TrackingStatus::Conflict
        );
    }
}
