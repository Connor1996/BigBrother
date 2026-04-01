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
        AttentionReason, DashboardState, EventLevel, PersistentPrState, PullRequest,
        ReviewRequestPr, RunnerState, TrackedPr, TrackingStatus,
    },
    notify::{build_notification_sink, Notification, NotificationSink},
    runner::{
        RunOutcome, RunRequest, RunUpdate, OUTPUT_TRANSCRIPT_HEADER, PROMPT_TRANSCRIPT_HEADER,
    },
    state_store::{PersistentStateFile, StateStore},
};

pub trait GitHubProvider: Send + Sync {
    fn fetch_pull_requests(&self) -> BoxFuture<'_, Result<Vec<PullRequest>>>;

    fn fetch_review_requests(&self) -> BoxFuture<'_, Result<Vec<PullRequest>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

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

    fn fetch_review_requests_with_stats(
        &self,
    ) -> BoxFuture<'_, Result<(Vec<PullRequest>, GitHubRequestStats)>> {
        Box::pin(async move {
            Ok((
                self.fetch_review_requests().await?,
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

    fn post_issue_comment(&self, pr_key: String, body: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let _ = (pr_key, body);
            Err(anyhow::anyhow!(
                "posting issue comments is not supported by this provider"
            ))
        })
    }
}

#[derive(Debug, Clone, Default)]
struct PollResult {
    tracked_prs: Vec<PullRequest>,
    review_requests: Vec<PullRequest>,
    fetch_stats: GitHubRequestStats,
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
    pub total_matching_prs: Option<usize>,
    pub viewer_requests: usize,
    pub search_requests: usize,
    pub pull_detail_requests: usize,
    pub review_requests: usize,
    pub review_comment_requests: usize,
    pub issue_comment_requests: usize,
    pub check_run_requests: usize,
    pub light_prs: usize,
    pub hydrated_prs: usize,
    pub reused_prs: usize,
}

impl GitHubRequestStats {
    pub fn merge(&mut self, other: &GitHubRequestStats) {
        self.viewer_requests += other.viewer_requests;
        self.search_requests += other.search_requests;
        self.pull_detail_requests += other.pull_detail_requests;
        self.review_requests += other.review_requests;
        self.review_comment_requests += other.review_comment_requests;
        self.issue_comment_requests += other.issue_comment_requests;
        self.check_run_requests += other.check_run_requests;
        self.light_prs += other.light_prs;
        self.hydrated_prs += other.hydrated_prs;
        self.reused_prs += other.reused_prs;
        if self.total_matching_prs.is_none() {
            self.total_matching_prs = other.total_matching_prs;
        }
    }

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
    notifier: Box<dyn NotificationSink>,
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
        let notifier = build_notification_sink(&config)?;
        Self::new_with_notifier(config, provider, runner, notifier)
    }

    pub fn new_with_notifier(
        config: ResolvedConfig,
        provider: Arc<dyn GitHubProvider>,
        runner: Arc<dyn AgentRunner>,
        notifier: Box<dyn NotificationSink>,
    ) -> Result<Self> {
        let store = StateStore::new(&config.state_path);
        let persisted_state = store.load()?;

        Ok(Self {
            config,
            provider,
            runner,
            notifier,
            store,
            shared_state: Arc::new(Mutex::new(DashboardState::default())),
            inner: AsyncMutex::new(SupervisorInner {
                persisted_state,
                active_runs: HashMap::new(),
            }),
            poll_lock: AsyncMutex::new(()),
        })
    }

    async fn send_notification(&self, pr_key: Option<&str>, notification: Notification) {
        if let Err(error) = self.notifier.send(notification).await {
            self.push_event(
                EventLevel::Error,
                pr_key.map(str::to_owned),
                format!("failed to deliver notification: {error:#}"),
            );
        }
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

    pub async fn trigger_deep_review(
        self: &Arc<Self>,
        pr_key: &str,
    ) -> Result<Option<ReviewRequestPr>> {
        {
            let state = self
                .shared_state
                .lock()
                .expect("dashboard state mutex poisoned");
            if !state.review_requests.contains_key(pr_key) {
                return Ok(None);
            }
        }

        let (pull_request, _) = self
            .provider
            .fetch_pull_request_with_stats(pr_key.to_owned())
            .await?;
        let Some(pull_request) = pull_request else {
            return Ok(None);
        };

        let started_at = Utc::now();
        let updated = {
            let mut inner = self.inner.lock().await;
            if inner.active_runs.contains_key(pr_key) {
                return Err(anyhow::anyhow!("a run is already active for {pr_key}"));
            }
            if inner.active_runs.len() >= self.config.daemon.max_concurrent_runs {
                return Err(anyhow::anyhow!(
                    "no runner slots are available for deep review ({}/{})",
                    inner.active_runs.len(),
                    self.config.daemon.max_concurrent_runs
                ));
            }

            inner.active_runs.insert(
                pr_key.to_owned(),
                ActiveRun {
                    started_at,
                    trigger: AttentionReason::DeepReview,
                },
            );

            let persisted = inner
                .persisted_state
                .prs
                .get(pr_key)
                .cloned()
                .unwrap_or_default();
            let mut state = self
                .shared_state
                .lock()
                .expect("dashboard state mutex poisoned");
            let review_request = ReviewRequestPr {
                pull_request: pull_request.clone(),
                persisted,
                runner: Some(RunnerState {
                    status: TrackingStatus::Running,
                    started_at,
                    finished_at: None,
                    attempt: 1,
                    trigger: AttentionReason::DeepReview,
                    summary: AttentionReason::DeepReview.active_summary().to_owned(),
                    live_output: None,
                    live_terminal: None,
                    last_terminal_output_at: None,
                    exit_code: None,
                }),
            };
            state
                .review_requests
                .insert(pr_key.to_owned(), review_request.clone());
            review_request
        };

        self.push_event(
            EventLevel::Info,
            Some(pr_key.to_owned()),
            format!("starting manual deep review for {pr_key}"),
        );
        self.send_notification(
            Some(&pr_key),
            manual_deep_review_started_notification(&pull_request),
        )
        .await;

        let supervisor = Arc::clone(self);
        let workspace = self.config.workspace.clone();
        let agent = self.config.agent.clone();
        tokio::spawn(async move {
            supervisor
                .run_manual_request(RunRequest {
                    pull_request,
                    trigger: AttentionReason::DeepReview,
                    workspace,
                    agent,
                    output_updates: None,
                })
                .await;
        });

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
        let mut poll_failure_notification = None;

        {
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
                    poll_failure_notification =
                        Some(poll_failed_notification(preferred_pr_key, error));
                }
            }
        }

        if let Some(notification) = poll_failure_notification {
            self.send_notification(preferred_pr_key, notification).await;
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

    async fn run_manual_request(self: Arc<Self>, request: RunRequest) {
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
                update_runner_output(&mut state, &output_pr_key, &mut buffered_output, update);
            }
        });

        let outcome = self.runner.run(runner_request).await;
        output_forwarder.await.ok();
        let mut outcome = outcome;
        let mut posted_comment = false;
        if outcome.success {
            let comment_body = deep_review_comment_body(outcome.captured_output.as_deref());
            match self
                .provider
                .post_issue_comment(pr_key.clone(), comment_body)
                .await
            {
                Ok(()) => {
                    posted_comment = true;
                }
                Err(error) => {
                    outcome = deep_review_comment_failure(outcome, &error.to_string());
                }
            }
        }

        let persisted = {
            let mut inner = self.inner.lock().await;
            let active_run = inner.active_runs.remove(&pr_key);
            let persisted = inner.persisted_state.prs.entry(pr_key.clone()).or_default();
            persist_run_metadata(
                persisted,
                active_run.as_ref().map(|run| run.trigger),
                &outcome,
            );
            let persisted = persisted.clone();

            if let Err(error) = self.store.save(&inner.persisted_state) {
                self.push_event(
                    EventLevel::Error,
                    Some(pr_key.clone()),
                    format!("failed to persist deep review state for {pr_key}: {error:#}"),
                );
            }

            persisted
        };

        {
            let mut state = self
                .shared_state
                .lock()
                .expect("dashboard state mutex poisoned");
            if let Some(review_request) = state.review_requests.get_mut(&pr_key) {
                review_request.persisted = persisted;
                review_request.runner = None;
            }
        }

        if posted_comment {
            self.push_event(
                EventLevel::Info,
                Some(pr_key.clone()),
                format!("posted deep review comment for {pr_key}"),
            );
        }

        self.push_event(
            if outcome.success {
                EventLevel::Info
            } else {
                EventLevel::Error
            },
            Some(pr_key.clone()),
            if outcome.success {
                format!("manual deep review completed for {pr_key}")
            } else {
                format!(
                    "manual deep review failed for {pr_key}: {}",
                    outcome.summary
                )
            },
        );
        self.send_notification(
            Some(&pr_key),
            manual_deep_review_finished_notification(&request.pull_request, &outcome),
        )
        .await;
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

        let poll_result = self
            .fetch_prs_for_poll(preferred_pr_key, allow_fallback)
            .await?;
        let prs = &poll_result.tracked_prs;
        let review_requests = &poll_result.review_requests;
        let fetch_stats = &poll_result.fetch_stats;
        if fetch_stats.has_metrics() {
            self.push_event(
                EventLevel::Info,
                preferred_pr_key.map(str::to_owned),
                fetch_stats.activity_message(prs.len() + review_requests.len(), preferred_pr_key),
            );
        }

        let selected_request = {
            let mut inner = self.inner.lock().await;
            let active_run_count = inner.active_runs.len();
            refresh_dashboard(
                &self.shared_state,
                &prs,
                &review_requests,
                &inner.persisted_state,
                &inner.active_runs,
                self.config.daemon.poll_interval_secs,
                fetch_stats.total_matching_prs,
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
                    &review_requests,
                    &inner.persisted_state,
                    &inner.active_runs,
                    self.config.daemon.poll_interval_secs,
                    fetch_stats.total_matching_prs,
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

                Some((
                    RunRequest {
                        pull_request: pr.clone(),
                        trigger,
                        workspace: self.config.workspace.clone(),
                        agent: self.config.agent.clone(),
                        output_updates: None,
                    },
                    automatic_run_started_notification(pr, trigger),
                ))
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

        if let Some((request, start_notification)) = selected_request {
            let pr_key = request.pull_request.key.clone();
            self.send_notification(Some(&pr_key), start_notification)
                .await;
            let (output_tx, mut output_rx) = mpsc::unbounded_channel::<RunUpdate>();
            let mut runner_request = request.clone();
            runner_request.output_updates = Some(output_tx);
            let output_pr_key = pr_key.clone();
            let shared_state = self.shared_state.clone();
            let output_forwarder = tokio::spawn(async move {
                let mut buffered_output = String::new();
                while let Some(update) = output_rx.recv().await {
                    let mut state = shared_state.lock().expect("dashboard state mutex poisoned");
                    update_runner_output(&mut state, &output_pr_key, &mut buffered_output, update);
                }
            });

            let outcome = self.runner.run(runner_request).await;
            output_forwarder.await.ok();

            let mut inner = self.inner.lock().await;
            let active_run = inner.active_runs.remove(&pr_key);
            let mut auto_paused = false;
            {
                let persisted = inner.persisted_state.prs.entry(pr_key.clone()).or_default();
                persist_run_metadata(
                    persisted,
                    active_run.as_ref().map(|run| run.trigger),
                    &outcome,
                );
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
                &review_requests,
                &inner.persisted_state,
                &inner.active_runs,
                self.config.daemon.poll_interval_secs,
                fetch_stats.total_matching_prs,
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
            self.send_notification(
                Some(&pr_key),
                automatic_run_finished_notification(&request, &outcome),
            )
            .await;

            if auto_paused {
                self.push_event(
                    EventLevel::Error,
                    Some(pr_key.clone()),
                    format!("auto-paused {pr_key} after {MAX_AUTOMATIC_RETRIES} retry attempts"),
                );
                self.send_notification(
                    Some(&pr_key),
                    automatic_run_auto_paused_notification(&request.pull_request),
                )
                .await;
            }
        }

        Ok(())
    }

    async fn fetch_prs_for_poll(
        &self,
        preferred_pr_key: Option<&str>,
        allow_fallback: bool,
    ) -> Result<PollResult> {
        if let Some(pr_key) = preferred_pr_key {
            if !allow_fallback {
                let mut prs = current_poll_query_state(&self.shared_state).previous_prs;
                let review_requests = current_review_requests(&self.shared_state);
                let (fetched, stats) = self
                    .provider
                    .fetch_pull_request_with_stats(pr_key.to_owned())
                    .await?;
                merge_targeted_pr_into_list(&mut prs, pr_key, fetched);
                return Ok(PollResult {
                    tracked_prs: prs,
                    review_requests,
                    fetch_stats: stats,
                });
            }
        }

        let tracked = self
            .provider
            .fetch_pull_requests_with_state_and_stats(current_poll_query_state(&self.shared_state))
            .await?;
        let review_requests = self.provider.fetch_review_requests_with_stats().await?;
        let mut combined_stats = tracked.1.clone();
        combined_stats.merge(&review_requests.1);

        Ok(PollResult {
            tracked_prs: tracked.0,
            review_requests: review_requests.0,
            fetch_stats: combined_stats,
        })
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
    review_requests: &[PullRequest],
    persisted_state: &PersistentStateFile,
    active_runs: &HashMap<String, ActiveRun>,
    poll_interval_secs: u64,
    total_matching_prs: Option<usize>,
) {
    let mut state = shared_state.lock().expect("dashboard state mutex poisoned");
    let tracked = build_tracked_prs(prs, persisted_state, active_runs, &state.tracked_prs);
    let review_requests = build_review_requests(
        review_requests,
        persisted_state,
        active_runs,
        &state.review_requests,
    );
    let tracked_len = tracked.len();
    let previous_total = state.total_matching_prs.unwrap_or(tracked_len);
    state.tracked_prs = tracked;
    state.review_requests = review_requests;
    state.total_matching_prs = Some(
        total_matching_prs
            .unwrap_or(previous_total)
            .max(tracked_len),
    );
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

fn build_review_requests(
    prs: &[PullRequest],
    persisted_state: &PersistentStateFile,
    active_runs: &HashMap<String, ActiveRun>,
    previous_review_requests: &BTreeMap<String, ReviewRequestPr>,
) -> BTreeMap<String, ReviewRequestPr> {
    let mut review_requests = BTreeMap::new();

    for pr in prs {
        review_requests.insert(
            pr.key.clone(),
            build_review_request(pr, persisted_state, active_runs),
        );
    }

    for (pr_key, previous) in previous_review_requests {
        if review_requests.contains_key(pr_key) {
            continue;
        }

        if previous.runner.is_some() {
            review_requests.insert(
                pr_key.clone(),
                build_review_request(&previous.pull_request, persisted_state, active_runs),
            );
        }
    }

    review_requests
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

fn build_review_request(
    pr: &PullRequest,
    persisted_state: &PersistentStateFile,
    active_runs: &HashMap<String, ActiveRun>,
) -> ReviewRequestPr {
    let persisted = persisted_state
        .prs
        .get(&pr.key)
        .cloned()
        .unwrap_or_default();
    let active_run = active_runs.get(&pr.key);

    ReviewRequestPr {
        pull_request: pr.clone(),
        persisted,
        runner: active_run
            .filter(|active| active.trigger == AttentionReason::DeepReview)
            .map(|active| RunnerState {
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

fn current_review_requests(shared_state: &Arc<Mutex<DashboardState>>) -> Vec<PullRequest> {
    let state = shared_state.lock().expect("dashboard state mutex poisoned");
    state
        .review_requests
        .values()
        .map(|review_request| review_request.pull_request.clone())
        .collect()
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
    } else if matches!(pr.ci_status, crate::model::CiStatus::Pending) {
        TrackingStatus::WaitingCi
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
        AttentionReason::DeepReview => false,
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
        AttentionReason::MergeConflict | AttentionReason::DeepReview => {}
    }

    persisted.clear_retry_state();
}

fn update_runner_output(
    state: &mut DashboardState,
    pr_key: &str,
    buffered_output: &mut String,
    update: RunUpdate,
) {
    let Some(runner) = runner_for_pr_mut(state, pr_key) else {
        return;
    };

    match update {
        RunUpdate::TranscriptChunk(chunk) => {
            append_live_output(buffered_output, &chunk);
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

fn runner_for_pr_mut<'a>(
    state: &'a mut DashboardState,
    pr_key: &str,
) -> Option<&'a mut RunnerState> {
    if let Some(tracked) = state.tracked_prs.get_mut(pr_key) {
        return tracked.runner.as_mut();
    }

    if let Some(review_request) = state.review_requests.get_mut(pr_key) {
        return review_request.runner.as_mut();
    }

    None
}

fn persist_run_metadata(
    persisted: &mut PersistentPrState,
    trigger: Option<AttentionReason>,
    outcome: &RunOutcome,
) {
    persisted.last_run_started_at = Some(outcome.started_at);
    persisted.last_run_finished_at = Some(outcome.finished_at);
    persisted.last_run_status = Some(if outcome.success { "success" } else { "error" }.to_owned());
    persisted.last_run_summary = Some(outcome.summary.clone());
    persisted.last_run_output = capped_run_output(outcome.captured_output.as_deref());
    persisted.last_run_terminal = capped_run_output(outcome.captured_terminal.as_deref());
    persisted.last_terminal_output_at = outcome.last_terminal_output_at;
    persisted.last_run_trigger = trigger;
}

fn deep_review_comment_body(output: Option<&str>) -> String {
    let review_body = output
        .and_then(strip_output_transcript_preamble)
        .filter(|body| !body.trim().is_empty())
        .unwrap_or("No findings.".to_owned());
    format!("## Deep Review\n\n{review_body}")
}

fn strip_output_transcript_preamble(output: &str) -> Option<String> {
    let output = output
        .find(OUTPUT_TRANSCRIPT_HEADER)
        .map(|offset| &output[offset + OUTPUT_TRANSCRIPT_HEADER.len()..])
        .unwrap_or(output)
        .trim();

    if output.is_empty() {
        None
    } else {
        Some(output.to_owned())
    }
}

fn deep_review_comment_failure(mut outcome: RunOutcome, error: &str) -> RunOutcome {
    let mut captured_output =
        strip_output_transcript_preamble(outcome.captured_output.as_deref().unwrap_or(""))
            .unwrap_or_default();
    if !captured_output.is_empty() {
        captured_output.push_str("\n\n");
    }
    captured_output.push_str(&format!("failed to post deep review comment: {error}"));
    outcome.success = false;
    outcome.exit_code = None;
    outcome.summary = AttentionReason::DeepReview.failure_summary().to_owned();
    outcome.captured_output = Some(captured_output);
    outcome
}

fn poll_failed_notification(preferred_pr_key: Option<&str>, error: &anyhow::Error) -> Notification {
    let title = preferred_pr_key
        .map(|pr_key| format!("targeted daemon check failed for {pr_key}"))
        .unwrap_or_else(|| "scheduled daemon poll failed".to_owned());

    Notification::new(EventLevel::Error, title, format!("Error: {error:#}"))
}

fn manual_deep_review_started_notification(pr: &PullRequest) -> Notification {
    Notification::new(
        EventLevel::Info,
        format!("starting manual deep review for {}", pr.key),
        pr_notification_body(pr, &[format!("Result URL: {}", pr.url)]),
    )
}

fn manual_deep_review_finished_notification(
    pr: &PullRequest,
    outcome: &RunOutcome,
) -> Notification {
    Notification::new(
        if outcome.success {
            EventLevel::Info
        } else {
            EventLevel::Error
        },
        if outcome.success {
            format!("manual deep review completed for {}", pr.key)
        } else {
            format!("manual deep review failed for {}", pr.key)
        },
        pr_notification_body(
            pr,
            &[
                format!("Summary: {}", outcome.summary),
                format!("Result URL: {}", pr.url),
            ],
        ),
    )
}

fn automatic_run_started_notification(pr: &PullRequest, trigger: AttentionReason) -> Notification {
    Notification::new(
        EventLevel::Info,
        format!("starting agent run for {}", pr.key),
        pr_notification_body(
            pr,
            &[
                format!("Reason: {}", trigger.label()),
                format!("Status: {}", TrackingStatus::Running.label()),
                format!("Result URL: {}", pr.url),
            ],
        ),
    )
}

fn automatic_run_finished_notification(request: &RunRequest, outcome: &RunOutcome) -> Notification {
    Notification::new(
        if outcome.success {
            EventLevel::Info
        } else {
            EventLevel::Error
        },
        if outcome.success {
            format!("agent run completed for {}", request.pull_request.key)
        } else {
            format!("agent run failed for {}", request.pull_request.key)
        },
        pr_notification_body(
            &request.pull_request,
            &[
                format!("Reason: {}", request.trigger.label()),
                format!("Summary: {}", outcome.summary),
                format!("Result URL: {}", request.pull_request.url),
            ],
        ),
    )
}

fn automatic_run_auto_paused_notification(pr: &PullRequest) -> Notification {
    Notification::new(
        EventLevel::Error,
        format!("auto-paused {}", pr.key),
        pr_notification_body(
            pr,
            &[
                format!(
                    "Reason: automatic retries reached the limit of {} attempts",
                    MAX_AUTOMATIC_RETRIES
                ),
                format!("Result URL: {}", pr.url),
            ],
        ),
    )
}

fn pr_notification_body(pr: &PullRequest, extra_lines: &[String]) -> String {
    let mut lines = vec![
        format!("PR: {} ({})", pr.key, pr.title),
        format!("Repo: {}", pr.repo_full_name),
    ];
    lines.extend(extra_lines.iter().cloned());
    lines.join("\n")
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
        sync::{Arc, Mutex},
    };

    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::{
        config::{
            AgentConfig, DaemonConfig, GitTransport, ResolvedConfig, ResolvedGitHubConfig,
            ResolvedNotificationsConfig, ResolvedWorkspaceConfig, UiConfig,
        },
        model::{CiStatus, EventLevel, PullRequest, ReviewDecision},
        notify::{Notification, NotificationSink},
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
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
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
            notifications: ResolvedNotificationsConfig::default(),
            state_path: std::env::temp_dir()
                .join(format!("symphony-rs-service-test-{unique}.json")),
        }
    }

    #[derive(Clone, Default)]
    struct RecordingNotificationSink {
        notifications: Arc<Mutex<Vec<Notification>>>,
    }

    impl RecordingNotificationSink {
        fn snapshot(&self) -> Vec<Notification> {
            self.notifications
                .lock()
                .expect("notifications mutex should not be poisoned")
                .clone()
        }
    }

    impl NotificationSink for RecordingNotificationSink {
        fn send(&self, notification: Notification) -> BoxFuture<'static, Result<()>> {
            let notifications = self.notifications.clone();
            Box::pin(async move {
                notifications
                    .lock()
                    .expect("notifications mutex should not be poisoned")
                    .push(notification);
                Ok(())
            })
        }
    }

    #[derive(Clone)]
    struct StaticGitHubProvider {
        prs: Vec<PullRequest>,
    }

    impl GitHubProvider for StaticGitHubProvider {
        fn fetch_pull_requests(&self) -> BoxFuture<'_, Result<Vec<PullRequest>>> {
            let prs = self.prs.clone();
            Box::pin(async move { Ok(prs) })
        }
    }

    #[derive(Clone)]
    struct StaticRunner {
        outcome: RunOutcome,
    }

    impl AgentRunner for StaticRunner {
        fn run(&self, _request: RunRequest) -> BoxFuture<'static, RunOutcome> {
            let outcome = self.outcome.clone();
            Box::pin(async move { outcome })
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
            TrackingStatus::WaitingCi
        );
    }

    #[test]
    fn clean_pr_with_pending_ci_reports_waiting_ci() {
        let mut pr = sample_pr();
        pr.ci_status = CiStatus::Pending;

        assert_eq!(
            derive_status(&pr, &PersistentPrState::default(), None, false),
            TrackingStatus::WaitingCi
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

    #[tokio::test]
    async fn successful_agent_run_emits_start_and_completion_notifications() {
        let mut pr = sample_pr();
        pr.ci_status = CiStatus::Failure;
        pr.ci_updated_at = Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 5, 0).unwrap());

        let outcome = RunOutcome {
            started_at: Utc::now(),
            finished_at: Utc::now(),
            success: true,
            exit_code: Some(0),
            summary: "fixed CI".to_owned(),
            captured_output: None,
            captured_terminal: None,
            last_terminal_output_at: None,
            processed_comment_at: None,
            processed_ci_at: pr.ci_updated_at,
            processed_head_sha: pr.head_sha.clone(),
        };
        let sink = RecordingNotificationSink::default();
        let supervisor = Supervisor::new_with_notifier(
            sample_config(),
            Arc::new(StaticGitHubProvider { prs: vec![pr] }),
            Arc::new(StaticRunner { outcome }),
            Box::new(sink.clone()),
        )
        .expect("supervisor should construct");

        supervisor.poll_once().await.expect("poll should succeed");

        let notifications = sink.snapshot();
        assert_eq!(notifications.len(), 2);
        assert_eq!(notifications[0].level, EventLevel::Info);
        assert!(notifications[0].title.contains("starting agent run"));
        assert_eq!(notifications[1].level, EventLevel::Info);
        assert!(notifications[1].title.contains("agent run completed"));
    }

    #[tokio::test]
    async fn poll_failures_emit_error_notifications() {
        #[derive(Clone)]
        struct FailingProvider;

        impl GitHubProvider for FailingProvider {
            fn fetch_pull_requests(&self) -> BoxFuture<'_, Result<Vec<PullRequest>>> {
                Box::pin(async move { Err(anyhow::anyhow!("boom")) })
            }
        }

        #[derive(Clone)]
        struct UnusedRunner;

        impl AgentRunner for UnusedRunner {
            fn run(&self, _request: RunRequest) -> BoxFuture<'static, RunOutcome> {
                Box::pin(async move { panic!("runner should not be called") })
            }
        }

        let sink = RecordingNotificationSink::default();
        let supervisor = Supervisor::new_with_notifier(
            sample_config(),
            Arc::new(FailingProvider),
            Arc::new(UnusedRunner),
            Box::new(sink.clone()),
        )
        .expect("supervisor should construct");

        let error = supervisor.poll_once().await.expect_err("poll should fail");
        assert!(error.to_string().contains("boom"));

        let notifications = sink.snapshot();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].level, EventLevel::Error);
        assert!(notifications[0]
            .title
            .contains("scheduled daemon poll failed"));
    }
}
