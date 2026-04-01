use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use chrono::{TimeZone, Utc};
use futures::future::BoxFuture;
use serde_json::{json, Value};
use symphony_rs::{
    config::{
        AgentConfig, DaemonConfig, ResolvedConfig, ResolvedGitHubConfig, ResolvedWorkspaceConfig,
        UiConfig,
    },
    model::{
        AttentionReason, CiStatus, PersistentPrState, PullRequest, ReviewDecision, RunnerState,
        TrackedPr, TrackingStatus,
    },
    prompt::build_prompt,
    runner::{RunOutcome, RunRequest, RunUpdate},
    service::{AgentRunner, GitHubProvider, GitHubRequestStats, PollQueryState, Supervisor},
    web,
};
use tokio::sync::Semaphore;
use tokio::time::{timeout, Duration};
use tower::util::ServiceExt;

static TEMP_PATH_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone)]
struct FakeGitHubProvider {
    prs: Vec<PullRequest>,
}

impl GitHubProvider for FakeGitHubProvider {
    fn fetch_pull_requests(&self) -> BoxFuture<'_, Result<Vec<PullRequest>>> {
        let prs = self.prs.clone();
        Box::pin(async move { Ok(prs) })
    }

    fn fetch_pull_request(&self, pr_key: String) -> BoxFuture<'_, Result<Option<PullRequest>>> {
        let pr = self.prs.iter().find(|pr| pr.key == pr_key).cloned();
        Box::pin(async move { Ok(pr) })
    }
}

#[derive(Clone)]
struct CountingTargetedGitHubProvider {
    prs: Vec<PullRequest>,
    total_matching_prs: Option<usize>,
    full_fetches: Arc<AtomicUsize>,
    targeted_fetches: Arc<AtomicUsize>,
}

impl GitHubProvider for CountingTargetedGitHubProvider {
    fn fetch_pull_requests(&self) -> BoxFuture<'_, Result<Vec<PullRequest>>> {
        let prs = self.prs.clone();
        let full_fetches = self.full_fetches.clone();
        Box::pin(async move {
            full_fetches.fetch_add(1, Ordering::SeqCst);
            Ok(prs)
        })
    }

    fn fetch_pull_requests_with_state_and_stats(
        &self,
        _poll_state: PollQueryState,
    ) -> BoxFuture<'_, Result<(Vec<PullRequest>, GitHubRequestStats)>> {
        let prs = self.prs.clone();
        let total_matching_prs = self.total_matching_prs;
        let full_fetches = self.full_fetches.clone();
        Box::pin(async move {
            full_fetches.fetch_add(1, Ordering::SeqCst);
            Ok((
                prs,
                GitHubRequestStats {
                    total_matching_prs,
                    ..GitHubRequestStats::default()
                },
            ))
        })
    }

    fn fetch_pull_request(&self, pr_key: String) -> BoxFuture<'_, Result<Option<PullRequest>>> {
        let pr = self.prs.iter().find(|pr| pr.key == pr_key).cloned();
        let targeted_fetches = self.targeted_fetches.clone();
        Box::pin(async move {
            targeted_fetches.fetch_add(1, Ordering::SeqCst);
            Ok(pr)
        })
    }
}

#[derive(Clone)]
struct CountingStatefulGitHubProvider {
    prs: Vec<PullRequest>,
    full_fetches: Arc<AtomicUsize>,
    stateful_fetches: Arc<AtomicUsize>,
    last_previous_len: Arc<AtomicUsize>,
    last_frozen_len: Arc<AtomicUsize>,
}

impl GitHubProvider for CountingStatefulGitHubProvider {
    fn fetch_pull_requests(&self) -> BoxFuture<'_, Result<Vec<PullRequest>>> {
        let prs = self.prs.clone();
        let full_fetches = self.full_fetches.clone();
        Box::pin(async move {
            full_fetches.fetch_add(1, Ordering::SeqCst);
            Ok(prs)
        })
    }

    fn fetch_pull_requests_with_state(
        &self,
        poll_state: PollQueryState,
    ) -> BoxFuture<'_, Result<Vec<PullRequest>>> {
        let prs = self.prs.clone();
        let stateful_fetches = self.stateful_fetches.clone();
        let last_previous_len = self.last_previous_len.clone();
        let last_frozen_len = self.last_frozen_len.clone();
        Box::pin(async move {
            stateful_fetches.fetch_add(1, Ordering::SeqCst);
            last_previous_len.store(poll_state.previous_prs.len(), Ordering::SeqCst);
            last_frozen_len.store(poll_state.frozen_pr_keys.len(), Ordering::SeqCst);
            Ok(prs)
        })
    }
}

#[derive(Clone)]
struct FreezingAwareMutableGitHubProvider {
    prs: Arc<Mutex<Vec<PullRequest>>>,
    last_frozen_len: Arc<AtomicUsize>,
}

impl GitHubProvider for FreezingAwareMutableGitHubProvider {
    fn fetch_pull_requests(&self) -> BoxFuture<'_, Result<Vec<PullRequest>>> {
        let prs = self
            .prs
            .lock()
            .expect("provider prs mutex should not be poisoned")
            .clone();
        Box::pin(async move { Ok(prs) })
    }

    fn fetch_pull_requests_with_state(
        &self,
        poll_state: PollQueryState,
    ) -> BoxFuture<'_, Result<Vec<PullRequest>>> {
        let prs = self
            .prs
            .lock()
            .expect("provider prs mutex should not be poisoned")
            .clone();
        let last_frozen_len = self.last_frozen_len.clone();
        Box::pin(async move {
            last_frozen_len.store(poll_state.frozen_pr_keys.len(), Ordering::SeqCst);
            let previous_prs = poll_state
                .previous_prs
                .into_iter()
                .map(|pr| (pr.key.clone(), pr))
                .collect::<BTreeMap<_, _>>();

            Ok(prs
                .into_iter()
                .map(|pr| {
                    if poll_state.frozen_pr_keys.contains(&pr.key) {
                        previous_prs.get(&pr.key).cloned().unwrap_or(pr)
                    } else {
                        pr
                    }
                })
                .collect())
        })
    }

    fn fetch_pull_request(&self, pr_key: String) -> BoxFuture<'_, Result<Option<PullRequest>>> {
        let pr = self
            .prs
            .lock()
            .expect("provider prs mutex should not be poisoned")
            .iter()
            .find(|pr| pr.key == pr_key)
            .cloned();
        Box::pin(async move { Ok(pr) })
    }
}

#[derive(Clone)]
struct StatsGitHubProvider {
    prs: Vec<PullRequest>,
    stats: GitHubRequestStats,
}

impl GitHubProvider for StatsGitHubProvider {
    fn fetch_pull_requests(&self) -> BoxFuture<'_, Result<Vec<PullRequest>>> {
        let prs = self.prs.clone();
        Box::pin(async move { Ok(prs) })
    }

    fn fetch_pull_requests_with_state_and_stats(
        &self,
        _poll_state: PollQueryState,
    ) -> BoxFuture<'_, Result<(Vec<PullRequest>, GitHubRequestStats)>> {
        let prs = self.prs.clone();
        let stats = self.stats.clone();
        Box::pin(async move { Ok((prs, stats)) })
    }
}

#[derive(Clone)]
struct FakeAgentRunner {
    invocations: Arc<AtomicUsize>,
    started: Arc<Semaphore>,
    allow_finish: Arc<Semaphore>,
}

impl AgentRunner for FakeAgentRunner {
    fn run(&self, request: RunRequest) -> BoxFuture<'static, RunOutcome> {
        let invocations = self.invocations.clone();
        let started = self.started.clone();
        let allow_finish = self.allow_finish.clone();

        Box::pin(async move {
            invocations.fetch_add(1, Ordering::SeqCst);
            started.add_permits(1);
            let transcript =
                fake_cli_transcript(&request, "codex: inspecting workspace\ncargo test -q\n");
            if let Some(output_updates) = request.output_updates.as_ref() {
                let _ = output_updates.send(RunUpdate::TranscriptChunk(transcript.clone()));
                let _ = output_updates.send(RunUpdate::TerminalSnapshot {
                    screen: "$ codex exec\nThinking...\ncargo test -q".to_owned(),
                    last_output_at: Utc::now(),
                });
            }
            let _permit = allow_finish
                .acquire()
                .await
                .expect("finish semaphore should remain open");

            RunOutcome {
                started_at: Utc::now(),
                finished_at: Utc::now(),
                success: true,
                exit_code: Some(0),
                summary: format!("fixed {}", request.pull_request.key),
                captured_output: Some(transcript),
                captured_terminal: Some("$ codex exec\nThinking...\ncargo test -q".to_owned()),
                last_terminal_output_at: Some(Utc::now()),
                processed_comment_at: request.pull_request.latest_reviewer_activity_at,
                processed_ci_at: request.pull_request.ci_updated_at,
                processed_head_sha: request.pull_request.head_sha,
            }
        })
    }
}

#[derive(Clone)]
struct AlwaysFailingAgentRunner {
    invocations: Arc<AtomicUsize>,
    started: Arc<Semaphore>,
}

impl AgentRunner for AlwaysFailingAgentRunner {
    fn run(&self, request: RunRequest) -> BoxFuture<'static, RunOutcome> {
        let invocations = self.invocations.clone();
        let started = self.started.clone();

        Box::pin(async move {
            invocations.fetch_add(1, Ordering::SeqCst);
            started.add_permits(1);
            let transcript = fake_cli_transcript(&request, "codex: run failed before fix\n");
            if let Some(output_updates) = request.output_updates.as_ref() {
                let _ = output_updates.send(RunUpdate::TranscriptChunk(transcript.clone()));
                let _ = output_updates.send(RunUpdate::TerminalSnapshot {
                    screen: "$ codex exec\nrun failed before fix".to_owned(),
                    last_output_at: Utc::now(),
                });
            }

            RunOutcome {
                started_at: Utc::now(),
                finished_at: Utc::now(),
                success: false,
                exit_code: Some(1),
                summary: format!("failed {}", request.pull_request.key),
                captured_output: Some(transcript),
                captured_terminal: Some("$ codex exec\nrun failed before fix".to_owned()),
                last_terminal_output_at: Some(Utc::now()),
                processed_comment_at: request.pull_request.latest_reviewer_activity_at,
                processed_ci_at: request.pull_request.ci_updated_at,
                processed_head_sha: request.pull_request.head_sha,
            }
        })
    }
}

#[tokio::test]
async fn mvp_flow_tracks_prs_runs_actionable_one_and_does_not_duplicate() {
    let runner = FakeAgentRunner {
        invocations: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(Semaphore::new(0)),
        allow_finish: Arc::new(Semaphore::new(0)),
    };
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider {
                prs: vec![idle_pr(), actionable_pr()],
            }),
            Arc::new(runner.clone()),
        )
        .expect("supervisor should initialize"),
    );

    let poll_task = {
        let supervisor = supervisor.clone();
        tokio::spawn(async move { supervisor.poll_once().await })
    };

    let _started = runner
        .started
        .acquire()
        .await
        .expect("started semaphore should remain open");

    let running_payload = get_json(supervisor.clone(), "/api/prs").await;
    let prs = running_payload["prs"].as_array().expect("prs array");
    assert_eq!(prs.len(), 2, "both PRs should be visible");
    assert_eq!(
        status_for(prs, "openai/symphony#7"),
        Some("running"),
        "actionable PR should become running while runner is active",
    );
    assert_eq!(
        runner.invocations.load(Ordering::SeqCst),
        1,
        "runner should be invoked once",
    );

    runner.allow_finish.add_permits(1);
    poll_task
        .await
        .expect("poll task should join")
        .expect("poll should succeed");

    let post_run_payload = get_json(supervisor.clone(), "/api/prs").await;
    let prs = post_run_payload["prs"].as_array().expect("prs array");
    assert_eq!(
        status_for(prs, "openai/symphony#7"),
        Some("waiting review"),
        "completed actionable PR should settle into waiting review after the signal is processed",
    );
    assert_eq!(
        summary_for(prs, "openai/symphony#7"),
        Some("CI failure handling completed"),
        "latest summary should stay concise and trigger-aware",
    );

    supervisor
        .poll_once()
        .await
        .expect("second poll should still succeed");

    assert_eq!(
        runner.invocations.load(Ordering::SeqCst),
        1,
        "unchanged CI signal should not trigger a second run",
    );

    let health = get_json(supervisor, "/api/health").await;
    assert_eq!(health["tracked_prs"], 2);
    assert_eq!(health["all_prs"], 2);
    assert_eq!(health["running_prs"], 0);
    assert!(health["ok"].as_bool().unwrap_or(false));
}

#[tokio::test]
async fn pause_api_toggles_review_wait_state_for_a_tracked_pr() {
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider {
                prs: vec![idle_pr()],
            }),
            Arc::new(FakeAgentRunner {
                invocations: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(Semaphore::new(0)),
                allow_finish: Arc::new(Semaphore::new(1)),
            }),
        )
        .expect("supervisor should initialize"),
    );

    supervisor
        .poll_once()
        .await
        .expect("initial poll should populate dashboard");

    let paused = post_json(
        supervisor.clone(),
        "/api/prs/pause",
        json!({
            "key": "openai/symphony#1",
            "paused": true,
        }),
    )
    .await;
    assert_eq!(paused["ok"], json!(true));
    assert_eq!(paused["pr"]["status"], json!("paused"));
    assert_eq!(paused["pr"]["is_paused"], json!(true));

    let prs_payload = get_json(supervisor.clone(), "/api/prs").await;
    let prs = prs_payload["prs"].as_array().expect("prs array");
    assert_eq!(status_for(prs, "openai/symphony#1"), Some("paused"));
    assert_eq!(is_paused_for(prs, "openai/symphony#1"), Some(true));

    let resumed = post_json(
        supervisor.clone(),
        "/api/prs/pause",
        json!({
            "key": "openai/symphony#1",
            "paused": false,
        }),
    )
    .await;
    assert_eq!(resumed["pr"]["status"], json!("waiting review"));
    assert_eq!(resumed["pr"]["is_paused"], json!(false));

    let prs_payload = get_json(supervisor, "/api/prs").await;
    let prs = prs_payload["prs"].as_array().expect("prs array");
    assert_eq!(status_for(prs, "openai/symphony#1"), Some("waiting review"));
    assert_eq!(is_paused_for(prs, "openai/symphony#1"), Some(false));
}

#[tokio::test]
async fn activity_api_exposes_recent_daemon_events() {
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider {
                prs: vec![idle_pr()],
            }),
            Arc::new(FakeAgentRunner {
                invocations: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(Semaphore::new(0)),
                allow_finish: Arc::new(Semaphore::new(1)),
            }),
        )
        .expect("supervisor should initialize"),
    );

    supervisor
        .poll_once()
        .await
        .expect("poll should record daemon activity");

    let payload = get_json(supervisor, "/api/activity").await;
    let events = payload["events"].as_array().expect("activity events array");
    assert!(
        events.iter().any(|event| {
            event["message"] == json!("starting scheduled daemon poll")
                && event["level"] == json!("info")
        }),
        "activity should include the poll-start event: {events:?}",
    );
    assert!(
        events.iter().any(|event| {
            event["message"] == json!("daemon poll found no actionable PRs among 1 tracked PRs")
                && event["level"] == json!("info")
        }),
        "activity should include the idle poll summary: {events:?}",
    );
}

#[tokio::test]
async fn activity_api_includes_github_request_metrics_for_scheduled_polls() {
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(StatsGitHubProvider {
                prs: vec![idle_pr(), actionable_pr()],
                stats: GitHubRequestStats {
                    search_requests: 1,
                    pull_detail_requests: 2,
                    review_requests: 1,
                    review_comment_requests: 1,
                    issue_comment_requests: 1,
                    check_run_requests: 1,
                    light_prs: 2,
                    hydrated_prs: 1,
                    reused_prs: 1,
                    ..GitHubRequestStats::default()
                },
            }),
            Arc::new(FakeAgentRunner {
                invocations: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(Semaphore::new(0)),
                allow_finish: Arc::new(Semaphore::new(1)),
            }),
        )
        .expect("supervisor should initialize"),
    );

    supervisor
        .poll_once()
        .await
        .expect("poll should record GitHub request metrics");

    let payload = get_json(supervisor, "/api/activity").await;
    let events = payload["events"].as_array().expect("activity events array");
    let metrics_event = events
        .iter()
        .find(|event| {
            event["message"]
                .as_str()
                .map(|message| {
                    message.contains("scheduled poll fetched 2 PRs using 7 GitHub requests")
                })
                .unwrap_or(false)
        })
        .expect("activity should include a scheduled poll metrics event");

    let message = metrics_event["message"]
        .as_str()
        .expect("metrics event message should be a string");
    assert!(message.contains("search 1"));
    assert!(message.contains("pull detail 2"));
    assert!(message.contains("check runs 1"));
    assert!(!message.contains("combined status"));
    assert!(message.contains("hydrated PR 1"));
    assert!(message.contains("reused PR 1"));
}

#[tokio::test]
async fn scheduled_poll_uses_stateful_fetch_with_dashboard_snapshot() {
    let provider = CountingStatefulGitHubProvider {
        prs: vec![idle_pr()],
        full_fetches: Arc::new(AtomicUsize::new(0)),
        stateful_fetches: Arc::new(AtomicUsize::new(0)),
        last_previous_len: Arc::new(AtomicUsize::new(usize::MAX)),
        last_frozen_len: Arc::new(AtomicUsize::new(usize::MAX)),
    };
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(provider.clone()),
            Arc::new(FakeAgentRunner {
                invocations: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(Semaphore::new(0)),
                allow_finish: Arc::new(Semaphore::new(1)),
            }),
        )
        .expect("supervisor should initialize"),
    );

    supervisor
        .poll_once()
        .await
        .expect("first poll should succeed");
    assert_eq!(
        provider.stateful_fetches.load(Ordering::SeqCst),
        1,
        "scheduled poll should use the stateful fetch path",
    );
    assert_eq!(
        provider.full_fetches.load(Ordering::SeqCst),
        0,
        "scheduled poll should not fall back to the legacy full fetch path",
    );
    assert_eq!(
        provider.last_previous_len.load(Ordering::SeqCst),
        0,
        "first poll should start with an empty dashboard snapshot",
    );

    supervisor
        .poll_once()
        .await
        .expect("second poll should also succeed");
    assert_eq!(
        provider.stateful_fetches.load(Ordering::SeqCst),
        2,
        "subsequent polls should keep using the stateful fetch path",
    );
    assert_eq!(
        provider.last_previous_len.load(Ordering::SeqCst),
        1,
        "second poll should receive the tracked PR from the previous snapshot",
    );
    assert_eq!(
        provider.last_frozen_len.load(Ordering::SeqCst),
        0,
        "non-paused dashboard snapshots should not mark any PRs as frozen",
    );
}

#[tokio::test]
async fn scheduled_poll_keeps_paused_pr_snapshot_frozen() {
    let provider = FreezingAwareMutableGitHubProvider {
        prs: Arc::new(Mutex::new(vec![idle_pr()])),
        last_frozen_len: Arc::new(AtomicUsize::new(usize::MAX)),
    };
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(provider.clone()),
            Arc::new(FakeAgentRunner {
                invocations: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(Semaphore::new(0)),
                allow_finish: Arc::new(Semaphore::new(1)),
            }),
        )
        .expect("supervisor should initialize"),
    );

    supervisor
        .poll_once()
        .await
        .expect("initial poll should succeed");
    supervisor
        .set_pr_paused("openai/symphony#1", true)
        .await
        .expect("pause operation should succeed")
        .expect("tracked PR should exist");

    let mut changed = idle_pr();
    changed.title = "Merged while paused".to_owned();
    changed.is_merged = true;
    changed.updated_at = changed.updated_at + chrono::Duration::minutes(30);
    *provider
        .prs
        .lock()
        .expect("provider prs mutex should not be poisoned") = vec![changed];

    supervisor
        .poll_once()
        .await
        .expect("paused poll should still succeed");

    let shared_state = supervisor.shared_state();
    let state = shared_state
        .lock()
        .expect("dashboard state mutex should not be poisoned");
    let tracked = state
        .tracked_prs
        .get("openai/symphony#1")
        .expect("paused PR should remain tracked");
    assert_eq!(tracked.pull_request.title, "Keep polling healthy");
    assert_eq!(tracked.status, TrackingStatus::Paused);
    assert_eq!(
        provider.last_frozen_len.load(Ordering::SeqCst),
        1,
        "scheduled polls should tell the provider which paused PRs are frozen",
    );
}

#[tokio::test]
async fn dashboard_html_exposes_top_right_pr_and_activity_tabs() {
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider { prs: vec![] }),
            Arc::new(FakeAgentRunner {
                invocations: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(Semaphore::new(0)),
                allow_finish: Arc::new(Semaphore::new(0)),
            }),
        )
        .expect("supervisor should initialize"),
    );

    let html = request_text(
        supervisor,
        Request::builder()
            .uri("/")
            .body(Body::empty())
            .expect("request should build"),
    )
    .await;

    assert!(
        html.contains(r#"id="tab-prs""#) && html.contains(r#"data-view="prs""#),
        "dashboard should expose the PRs tab, got: {html}",
    );
    assert!(
        html.contains(r#"id="tab-activity""#) && html.contains(r#"data-view="activity""#),
        "dashboard should expose the Activity tab, got: {html}",
    );
    assert!(
        html.contains(r#"id="view-activity""#),
        "dashboard should render the activity view container behind the tab switch, got: {html}",
    );
}

#[tokio::test]
async fn approved_green_pr_is_exposed_as_waiting_merge() {
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider {
                prs: vec![approved_green_pr()],
            }),
            Arc::new(FakeAgentRunner {
                invocations: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(Semaphore::new(0)),
                allow_finish: Arc::new(Semaphore::new(1)),
            }),
        )
        .expect("supervisor should initialize"),
    );

    supervisor
        .poll_once()
        .await
        .expect("poll should populate dashboard");

    let prs_payload = get_json(supervisor, "/api/prs").await;
    let prs = prs_payload["prs"].as_array().expect("prs array");
    assert_eq!(status_for(prs, "openai/symphony#9"), Some("waiting merge"));
}

#[tokio::test]
async fn approved_pr_with_pending_ci_reports_waiting_for_ci() {
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider {
                prs: vec![approved_pending_pr()],
            }),
            Arc::new(FakeAgentRunner {
                invocations: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(Semaphore::new(0)),
                allow_finish: Arc::new(Semaphore::new(1)),
            }),
        )
        .expect("supervisor should initialize"),
    );

    supervisor
        .poll_once()
        .await
        .expect("poll should populate dashboard");

    let prs_payload = get_json(supervisor, "/api/prs").await;
    let prs = prs_payload["prs"].as_array().expect("prs array");
    assert_eq!(
        status_for(prs, "openai/symphony#10"),
        Some("waiting for CI")
    );
}

#[tokio::test]
async fn conflicting_pr_is_exposed_as_conflict() {
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider {
                prs: vec![conflicting_pr()],
            }),
            Arc::new(FakeAgentRunner {
                invocations: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(Semaphore::new(0)),
                allow_finish: Arc::new(Semaphore::new(1)),
            }),
        )
        .expect("supervisor should initialize"),
    );

    supervisor
        .poll_once()
        .await
        .expect("poll should populate dashboard");

    let prs_payload = get_json(supervisor, "/api/prs").await;
    let prs = prs_payload["prs"].as_array().expect("prs array");
    assert_eq!(status_for(prs, "openai/symphony#11"), Some("conflict"));
}

#[tokio::test]
async fn unresolved_conflict_run_remains_actionable_after_resume() {
    let state_path = unique_temp_path("state.json");
    let workspace_root = unique_temp_path("workspaces");
    let provider = FreezingAwareMutableGitHubProvider {
        prs: Arc::new(Mutex::new(vec![conflicting_pr()])),
        last_frozen_len: Arc::new(AtomicUsize::new(usize::MAX)),
    };
    let runner = FakeAgentRunner {
        invocations: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(Semaphore::new(0)),
        allow_finish: Arc::new(Semaphore::new(2)),
    };
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(state_path.clone(), workspace_root),
            Arc::new(provider),
            Arc::new(runner.clone()),
        )
        .expect("supervisor should initialize"),
    );

    supervisor
        .poll_once()
        .await
        .expect("initial poll should succeed");

    let prs_payload = get_json(supervisor.clone(), "/api/prs").await;
    let prs = prs_payload["prs"].as_array().expect("prs array");
    assert_eq!(
        status_for(prs, "openai/symphony#11"),
        Some("conflict"),
        "a conflict run that leaves the PR conflicting should stay actionable",
    );

    let persisted: Value = serde_json::from_str(
        &std::fs::read_to_string(&state_path).expect("state file should exist"),
    )
    .expect("state file should be valid json");
    assert_eq!(
        persisted["prs"]["openai/symphony#11"]["last_processed_conflict_head_sha"],
        Value::Null,
        "an unresolved conflict must not be marked as processed",
    );
    assert_eq!(
        persisted["prs"]["openai/symphony#11"]["last_run_status"],
        json!("success"),
        "without an immediate post-run recheck, the run result itself stays successful",
    );

    supervisor
        .set_pr_paused("openai/symphony#11", true)
        .await
        .expect("pause operation should succeed")
        .expect("tracked PR should exist");
    let resumed = supervisor
        .set_pr_paused("openai/symphony#11", false)
        .await
        .expect("resume operation should succeed")
        .expect("tracked PR should exist");
    assert_eq!(resumed.status, TrackingStatus::Conflict);

    wait_for_invocations(&runner.invocations, 2).await;
    assert_eq!(
        runner.invocations.load(Ordering::SeqCst),
        2,
        "resume should trigger another attempt because the conflict is still current",
    );
}

#[tokio::test]
async fn review_run_success_leaves_same_pr_ci_failure_actionable() {
    let runner = FakeAgentRunner {
        invocations: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(Semaphore::new(0)),
        allow_finish: Arc::new(Semaphore::new(0)),
    };
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider {
                prs: vec![review_and_ci_pr()],
            }),
            Arc::new(runner.clone()),
        )
        .expect("supervisor should initialize"),
    );

    let first_poll = {
        let supervisor = supervisor.clone();
        tokio::spawn(async move { supervisor.poll_once().await })
    };
    let _started = runner
        .started
        .acquire()
        .await
        .expect("started semaphore should remain open");
    runner.allow_finish.add_permits(1);
    first_poll
        .await
        .expect("first poll task should join")
        .expect("first poll should succeed");

    let prs_payload = get_json(supervisor.clone(), "/api/prs").await;
    let prs = prs_payload["prs"].as_array().expect("prs array");
    assert_eq!(
        status_for(prs, "openai/symphony#12"),
        Some("needs attention"),
        "the review-triggered success should not consume the unchanged failing CI signal",
    );

    let second_poll = {
        let supervisor = supervisor.clone();
        tokio::spawn(async move { supervisor.poll_once().await })
    };
    let _started = runner
        .started
        .acquire()
        .await
        .expect("started semaphore should remain open");
    runner.allow_finish.add_permits(1);
    second_poll
        .await
        .expect("second poll task should join")
        .expect("second poll should succeed");

    assert_eq!(
        runner.invocations.load(Ordering::SeqCst),
        2,
        "the next poll should pick up the remaining CI failure",
    );
}

#[tokio::test]
async fn running_pr_exposes_live_terminal() {
    let runner = FakeAgentRunner {
        invocations: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(Semaphore::new(0)),
        allow_finish: Arc::new(Semaphore::new(0)),
    };
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider {
                prs: vec![actionable_pr()],
            }),
            Arc::new(runner.clone()),
        )
        .expect("supervisor should initialize"),
    );

    let poll_task = {
        let supervisor = supervisor.clone();
        tokio::spawn(async move { supervisor.poll_once().await })
    };

    let _started = runner
        .started
        .acquire()
        .await
        .expect("started semaphore should remain open");

    let running_payload = get_json(supervisor.clone(), "/api/prs").await;
    let prs = running_payload["prs"].as_array().expect("prs array");
    let running_pr = prs
        .iter()
        .find(|pr| pr["key"] == json!("openai/symphony#7"))
        .expect("running PR should be visible");
    assert_eq!(running_pr["status"], json!("running"));
    assert_eq!(running_pr["details_label"], json!("Started"));
    assert!(running_pr["details_at"].is_string());
    assert_eq!(
        running_pr["latest_summary"],
        json!("investigating CI failure")
    );
    assert_eq!(
        running_pr["detail_output"],
        json!("$ codex exec\nThinking...\ncargo test -q")
    );

    let detail_payload = get_json(supervisor.clone(), "/api/pr?key=openai%2Fsymphony%237").await;
    assert_eq!(detail_payload["key"], json!("openai/symphony#7"));
    assert_eq!(
        detail_payload["latest_summary"],
        json!("investigating CI failure")
    );
    assert_eq!(
        detail_payload["detail_output"],
        json!("$ codex exec\nThinking...\ncargo test -q")
    );
    assert!(detail_payload["last_terminal_output_at"].is_string());
    assert_eq!(detail_payload["live_output"], Value::Null);

    runner.allow_finish.add_permits(1);
    poll_task
        .await
        .expect("poll task should join")
        .expect("poll should succeed");
}

#[tokio::test]
async fn completed_pr_detail_shows_saved_run_output() {
    let runner = FakeAgentRunner {
        invocations: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(Semaphore::new(0)),
        allow_finish: Arc::new(Semaphore::new(1)),
    };
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider {
                prs: vec![actionable_pr()],
            }),
            Arc::new(runner),
        )
        .expect("supervisor should initialize"),
    );

    supervisor
        .poll_once()
        .await
        .expect("poll should finish the fake run");

    let detail_payload = get_json(supervisor.clone(), "/api/pr?key=openai%2Fsymphony%237").await;
    assert_eq!(detail_payload["status"], json!("waiting review"));
    assert_eq!(detail_payload["details_label"], json!("Last run"));
    assert!(detail_payload["details_at"].is_string());
    assert_eq!(
        detail_payload["detail_output"],
        json!("codex: inspecting workspace\ncargo test -q")
    );
    assert!(detail_payload["last_terminal_output_at"].is_string());
    assert_eq!(detail_payload["live_output"], Value::Null);
    assert_eq!(
        detail_payload["latest_summary"],
        json!("CI failure handling completed")
    );
}

#[tokio::test]
async fn running_pr_does_not_fall_back_to_saved_terminal_snapshot() {
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider { prs: vec![] }),
            Arc::new(FakeAgentRunner {
                invocations: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(Semaphore::new(0)),
                allow_finish: Arc::new(Semaphore::new(0)),
            }),
        )
        .expect("supervisor should initialize"),
    );

    {
        let shared_state = supervisor.shared_state();
        let mut state = shared_state
            .lock()
            .expect("dashboard state mutex should not be poisoned");
        let pr = actionable_pr();
        state.tracked_prs.insert(
            pr.key.clone(),
            TrackedPr {
                pull_request: pr,
                status: TrackingStatus::Running,
                attention_reason: Some(AttentionReason::CiFailed),
                persisted: PersistentPrState {
                    last_run_output: Some("stale output".to_owned()),
                    last_run_terminal: Some("stale terminal".to_owned()),
                    ..PersistentPrState::default()
                },
                runner: Some(RunnerState {
                    status: TrackingStatus::Running,
                    started_at: Utc::now(),
                    finished_at: None,
                    attempt: 1,
                    trigger: AttentionReason::CiFailed,
                    summary: AttentionReason::CiFailed.active_summary().to_owned(),
                    live_output: None,
                    live_terminal: None,
                    last_terminal_output_at: None,
                    exit_code: None,
                }),
            },
        );
    }

    let detail_payload = get_json(supervisor, "/api/pr?key=openai%2Fsymphony%237").await;
    assert_eq!(detail_payload["status"], json!("running"));
    assert_eq!(detail_payload["live_output"], Value::Null);
    assert_eq!(detail_payload["detail_output"], Value::Null);
}

#[tokio::test]
async fn saved_run_timestamp_is_exposed_in_pr_list() {
    let finished_at = Utc.with_ymd_and_hms(2026, 3, 31, 21, 12, 0).unwrap();
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider { prs: vec![] }),
            Arc::new(FakeAgentRunner {
                invocations: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(Semaphore::new(0)),
                allow_finish: Arc::new(Semaphore::new(0)),
            }),
        )
        .expect("supervisor should initialize"),
    );

    {
        let shared_state = supervisor.shared_state();
        let mut state = shared_state
            .lock()
            .expect("dashboard state mutex should not be poisoned");
        let pr = actionable_pr();
        state.tracked_prs.insert(
            pr.key.clone(),
            TrackedPr {
                pull_request: pr,
                status: TrackingStatus::WaitingReview,
                attention_reason: None,
                persisted: PersistentPrState {
                    last_run_finished_at: Some(finished_at),
                    last_run_status: Some("error".to_owned()),
                    last_run_trigger: Some(AttentionReason::ReviewFeedback),
                    last_run_summary: Some(
                        "2026-03-31T22:14:30Z ERROR auth expired\nvery long captured output"
                            .to_owned(),
                    ),
                    ..PersistentPrState::default()
                },
                runner: None,
            },
        );
    }

    let prs_payload = get_json(supervisor, "/api/prs").await;
    let prs = prs_payload["prs"].as_array().expect("prs array");
    let pr = prs
        .iter()
        .find(|pr| pr["key"] == json!("openai/symphony#7"))
        .expect("tracked PR should be visible");
    assert_eq!(pr["details_label"], json!("Last run"));
    assert_eq!(
        pr["details_at"],
        json!(finished_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    );
    assert_eq!(
        pr["latest_summary"],
        json!("review feedback handling failed")
    );
}

#[tokio::test]
async fn paused_pr_does_not_auto_run_until_resumed() {
    let runner = FakeAgentRunner {
        invocations: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(Semaphore::new(0)),
        allow_finish: Arc::new(Semaphore::new(1)),
    };
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider {
                prs: vec![actionable_pr()],
            }),
            Arc::new(runner.clone()),
        )
        .expect("supervisor should initialize"),
    );

    {
        let shared_state = supervisor.shared_state();
        let mut state = shared_state
            .lock()
            .expect("dashboard state mutex should not be poisoned");
        let pr = actionable_pr();
        state.tracked_prs.insert(
            pr.key.clone(),
            TrackedPr {
                pull_request: pr.clone(),
                status: TrackingStatus::WaitingReview,
                attention_reason: None,
                persisted: PersistentPrState::default(),
                runner: None,
            },
        );
    }

    let paused = supervisor
        .set_pr_paused("openai/symphony#7", true)
        .await
        .expect("pause operation should succeed")
        .expect("tracked PR should exist");
    assert_eq!(paused.status, TrackingStatus::Paused);
    assert!(paused.persisted.paused);

    supervisor
        .poll_once()
        .await
        .expect("poll should succeed while paused");
    assert_eq!(
        runner.invocations.load(Ordering::SeqCst),
        0,
        "paused PR should not trigger the runner",
    );

    let resumed = supervisor
        .set_pr_paused("openai/symphony#7", false)
        .await
        .expect("resume operation should succeed")
        .expect("tracked PR should exist");
    assert_eq!(resumed.status, TrackingStatus::NeedsAttention);
    assert!(!resumed.persisted.paused);

    wait_for_runner_start(&runner.started).await;
    assert_eq!(
        runner.invocations.load(Ordering::SeqCst),
        1,
        "resumed actionable PR should trigger the runner immediately",
    );
}

#[tokio::test]
async fn resume_targeted_check_fetches_only_the_resumed_pr() {
    let provider = CountingTargetedGitHubProvider {
        prs: vec![actionable_pr()],
        total_matching_prs: None,
        full_fetches: Arc::new(AtomicUsize::new(0)),
        targeted_fetches: Arc::new(AtomicUsize::new(0)),
    };
    let runner = FakeAgentRunner {
        invocations: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(Semaphore::new(0)),
        allow_finish: Arc::new(Semaphore::new(0)),
    };
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(provider.clone()),
            Arc::new(runner.clone()),
        )
        .expect("supervisor should initialize"),
    );

    {
        let shared_state = supervisor.shared_state();
        let mut state = shared_state
            .lock()
            .expect("dashboard state mutex should not be poisoned");
        let pr = actionable_pr();
        state.tracked_prs.insert(
            pr.key.clone(),
            TrackedPr {
                pull_request: pr.clone(),
                status: TrackingStatus::WaitingReview,
                attention_reason: None,
                persisted: PersistentPrState::default(),
                runner: None,
            },
        );
    }

    supervisor
        .set_pr_paused("openai/symphony#7", true)
        .await
        .expect("pause operation should succeed")
        .expect("tracked PR should exist");

    let resumed = supervisor
        .set_pr_paused("openai/symphony#7", false)
        .await
        .expect("resume operation should succeed")
        .expect("tracked PR should exist");
    assert_eq!(resumed.status, TrackingStatus::NeedsAttention);

    wait_for_runner_start(&runner.started).await;
    wait_for_invocations(&provider.targeted_fetches, 1).await;
    assert_eq!(
        provider.targeted_fetches.load(Ordering::SeqCst),
        1,
        "resume should perform exactly one targeted GitHub fetch for the resumed PR",
    );
    assert_eq!(
        provider.full_fetches.load(Ordering::SeqCst),
        0,
        "resume should not fall back to a full authored-PR refresh",
    );
}

#[tokio::test]
async fn health_keeps_total_matching_prs_after_targeted_resume_check() {
    let provider = CountingTargetedGitHubProvider {
        prs: vec![idle_pr()],
        total_matching_prs: Some(7),
        full_fetches: Arc::new(AtomicUsize::new(0)),
        targeted_fetches: Arc::new(AtomicUsize::new(0)),
    };
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(provider.clone()),
            Arc::new(FakeAgentRunner {
                invocations: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(Semaphore::new(0)),
                allow_finish: Arc::new(Semaphore::new(0)),
            }),
        )
        .expect("supervisor should initialize"),
    );

    supervisor
        .poll_once()
        .await
        .expect("initial poll should succeed");
    let initial_health = get_json(supervisor.clone(), "/api/health").await;
    assert_eq!(initial_health["tracked_prs"], 1);
    assert_eq!(initial_health["all_prs"], 7);

    supervisor
        .set_pr_paused("openai/symphony#1", true)
        .await
        .expect("pause operation should succeed")
        .expect("tracked PR should exist");
    supervisor
        .set_pr_paused("openai/symphony#1", false)
        .await
        .expect("resume operation should succeed")
        .expect("tracked PR should exist");

    wait_for_invocations(&provider.targeted_fetches, 1).await;

    let resumed_health = get_json(supervisor, "/api/health").await;
    assert_eq!(resumed_health["tracked_prs"], 1);
    assert_eq!(resumed_health["all_prs"], 7);
    assert_eq!(
        provider.full_fetches.load(Ordering::SeqCst),
        1,
        "targeted resume checks should preserve the prior full-poll total instead of forcing another full refresh",
    );
}

#[tokio::test]
async fn paused_state_survives_supervisor_restart() {
    let state_path = unique_temp_path("state.json");
    let workspace_root = unique_temp_path("workspaces");

    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(state_path.clone(), workspace_root.clone()),
            Arc::new(FakeGitHubProvider {
                prs: vec![idle_pr()],
            }),
            Arc::new(FakeAgentRunner {
                invocations: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(Semaphore::new(0)),
                allow_finish: Arc::new(Semaphore::new(0)),
            }),
        )
        .expect("supervisor should initialize"),
    );

    supervisor
        .poll_once()
        .await
        .expect("initial poll should populate dashboard");
    post_json(
        supervisor,
        "/api/prs/pause",
        json!({
            "key": "openai/symphony#1",
            "paused": true,
        }),
    )
    .await;
    let persisted = std::fs::read_to_string(&state_path).expect("state file should exist");
    assert!(
        persisted.contains(r#""paused": true"#),
        "pause state should be written to disk, got: {persisted}",
    );

    let restarted = Arc::new(
        Supervisor::new(
            sample_config(state_path, workspace_root),
            Arc::new(FakeGitHubProvider {
                prs: vec![idle_pr()],
            }),
            Arc::new(FakeAgentRunner {
                invocations: Arc::new(AtomicUsize::new(0)),
                started: Arc::new(Semaphore::new(0)),
                allow_finish: Arc::new(Semaphore::new(0)),
            }),
        )
        .expect("restarted supervisor should initialize"),
    );

    restarted
        .poll_once()
        .await
        .expect("restarted poll should populate dashboard");

    let prs_payload = get_json(restarted, "/api/prs").await;
    let prs = prs_payload["prs"].as_array().expect("prs array");
    assert_eq!(status_for(prs, "openai/symphony#1"), Some("paused"));
    assert_eq!(is_paused_for(prs, "openai/symphony#1"), Some(true));
}

#[tokio::test]
async fn failed_runs_retry_on_subsequent_polls_and_auto_pause_after_five_retries() {
    let runner = AlwaysFailingAgentRunner {
        invocations: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(Semaphore::new(0)),
    };
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider {
                prs: vec![actionable_pr()],
            }),
            Arc::new(runner.clone()),
        )
        .expect("supervisor should initialize"),
    );

    for expected_runs in 1..=5 {
        supervisor
            .poll_once()
            .await
            .expect("poll should succeed while retries remain");

        let prs_payload = get_json(supervisor.clone(), "/api/prs").await;
        let prs = prs_payload["prs"].as_array().expect("prs array");
        assert_eq!(
            status_for(prs, "openai/symphony#7"),
            Some("retrying"),
            "failed runs should stay retry-scheduled until the retry budget is exhausted",
        );
        assert_eq!(is_paused_for(prs, "openai/symphony#7"), Some(false));
        assert_eq!(
            runner.invocations.load(Ordering::SeqCst),
            expected_runs,
            "each poll should trigger one retry while retries remain",
        );
    }

    supervisor
        .poll_once()
        .await
        .expect("final retry poll should still succeed");

    let prs_payload = get_json(supervisor, "/api/prs").await;
    let prs = prs_payload["prs"].as_array().expect("prs array");
    assert_eq!(
        status_for(prs, "openai/symphony#7"),
        Some("paused"),
        "the PR should auto-pause after the fifth retry fails",
    );
    assert_eq!(is_paused_for(prs, "openai/symphony#7"), Some(true));
    assert_eq!(
        runner.invocations.load(Ordering::SeqCst),
        6,
        "one initial failure plus five retries should have been attempted",
    );
}

#[tokio::test]
async fn resume_clears_retry_state_and_rechecks_the_current_signal() {
    let runner = AlwaysFailingAgentRunner {
        invocations: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(Semaphore::new(0)),
    };
    let supervisor = Arc::new(
        Supervisor::new(
            sample_config(
                unique_temp_path("state.json"),
                unique_temp_path("workspaces"),
            ),
            Arc::new(FakeGitHubProvider {
                prs: vec![actionable_pr()],
            }),
            Arc::new(runner.clone()),
        )
        .expect("supervisor should initialize"),
    );

    for _ in 0..6 {
        supervisor
            .poll_once()
            .await
            .expect("poll should succeed while driving toward auto-pause");
    }

    let resumed = supervisor
        .set_pr_paused("openai/symphony#7", false)
        .await
        .expect("resume operation should succeed")
        .expect("tracked PR should exist");
    assert_eq!(
        resumed.status,
        TrackingStatus::NeedsAttention,
        "resume should recalculate status from the current PR signal instead of staying paused",
    );
    assert!(!resumed.persisted.paused);
    assert_eq!(resumed.persisted.consecutive_failures, 0);

    wait_for_runner_start(&runner.started).await;
    wait_for_invocations(&runner.invocations, 7).await;
    assert_eq!(
        runner.invocations.load(Ordering::SeqCst),
        7,
        "resume should immediately recheck the current actionable signal",
    );

    let prs_payload = get_json(supervisor, "/api/prs").await;
    let prs = prs_payload["prs"].as_array().expect("prs array");
    assert_eq!(status_for(prs, "openai/symphony#7"), Some("retrying"));
    assert_eq!(is_paused_for(prs, "openai/symphony#7"), Some(false));
}

async fn get_json(supervisor: Arc<Supervisor>, path: &str) -> Value {
    request_json(
        supervisor,
        Request::builder()
            .uri(path)
            .body(Body::empty())
            .expect("request should build"),
    )
    .await
}

async fn post_json(supervisor: Arc<Supervisor>, path: &str, payload: Value) -> Value {
    request_json(
        supervisor,
        Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .expect("request should build"),
    )
    .await
}

async fn request_json(supervisor: Arc<Supervisor>, request: Request<Body>) -> Value {
    let response = web::router(supervisor)
        .oneshot(request)
        .await
        .expect("route should respond");
    assert!(
        response.status().is_success(),
        "request should succeed with 2xx status, got {}",
        response.status()
    );

    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should collect");
    serde_json::from_slice(&bytes).expect("response should be JSON")
}

async fn request_text(supervisor: Arc<Supervisor>, request: Request<Body>) -> String {
    let response = web::router(supervisor)
        .oneshot(request)
        .await
        .expect("route should respond");
    assert!(
        response.status().is_success(),
        "request should succeed with 2xx status, got {}",
        response.status()
    );

    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should collect");
    String::from_utf8(bytes.to_vec()).expect("response should be UTF-8")
}

async fn wait_for_runner_start(started: &Arc<Semaphore>) {
    let _ = timeout(Duration::from_secs(2), started.clone().acquire_owned())
        .await
        .expect("runner should start promptly after resume")
        .expect("start semaphore should remain open");
}

async fn wait_for_invocations(invocations: &Arc<AtomicUsize>, expected: usize) {
    timeout(Duration::from_secs(2), async {
        loop {
            if invocations.load(Ordering::SeqCst) >= expected {
                break;
            }

            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("runner invocation count should reach the expected value");
}

fn status_for<'a>(prs: &'a [Value], key: &str) -> Option<&'a str> {
    prs.iter()
        .find(|pr| pr["key"] == key)
        .and_then(|pr| pr["status"].as_str())
}

fn summary_for<'a>(prs: &'a [Value], key: &str) -> Option<&'a str> {
    prs.iter()
        .find(|pr| pr["key"] == key)
        .and_then(|pr| pr["latest_summary"].as_str())
}

fn is_paused_for(prs: &[Value], key: &str) -> Option<bool> {
    prs.iter()
        .find(|pr| pr["key"] == key)
        .and_then(|pr| pr["is_paused"].as_bool())
}

fn sample_config(state_path: PathBuf, workspace_root: PathBuf) -> ResolvedConfig {
    ResolvedConfig {
        github: ResolvedGitHubConfig {
            api_token: "test-token".to_owned(),
            api_base_url: "https://api.github.test".to_owned(),
            author: Some("connor".to_owned()),
            query: None,
            max_prs: 10,
        },
        daemon: DaemonConfig {
            poll_interval_secs: 1,
            max_concurrent_runs: 1,
        },
        workspace: ResolvedWorkspaceConfig {
            root: workspace_root,
            repo_map: BTreeMap::new(),
            git_transport: symphony_rs::config::GitTransport::Https,
        },
        agent: AgentConfig {
            command: "fake-agent".to_owned(),
            args: vec![],
            dangerously_bypass_approvals_and_sandbox: false,
            additional_instructions: None,
        },
        ui: UiConfig::default(),
        state_path,
    }
}

fn idle_pr() -> PullRequest {
    base_pr(
        "openai/symphony#1",
        1,
        "Keep polling healthy",
        "idle-sha",
        CiStatus::Success,
        None,
        ReviewDecision::Clean,
        0,
        None,
    )
}

fn actionable_pr() -> PullRequest {
    base_pr(
        "openai/symphony#7",
        7,
        "Fix broken CI",
        "actionable-sha",
        CiStatus::Failure,
        Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 5, 0).unwrap()),
        ReviewDecision::Clean,
        0,
        None,
    )
}

fn approved_green_pr() -> PullRequest {
    base_pr(
        "openai/symphony#9",
        9,
        "Ready to land",
        "approved-green-sha",
        CiStatus::Success,
        None,
        ReviewDecision::Approved,
        1,
        Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 12, 0).unwrap()),
    )
}

fn approved_pending_pr() -> PullRequest {
    base_pr(
        "openai/symphony#10",
        10,
        "Approved but checks still running",
        "approved-pending-sha",
        CiStatus::Pending,
        Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 15, 0).unwrap()),
        ReviewDecision::Approved,
        1,
        Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 14, 0).unwrap()),
    )
}

fn conflicting_pr() -> PullRequest {
    let mut pr = base_pr(
        "openai/symphony#11",
        11,
        "Needs base branch merge",
        "conflict-sha",
        CiStatus::Success,
        None,
        ReviewDecision::Clean,
        0,
        None,
    );
    pr.has_conflicts = true;
    pr.mergeable_state = Some("dirty".to_owned());
    pr
}

fn review_and_ci_pr() -> PullRequest {
    base_pr(
        "openai/symphony#12",
        12,
        "Needs review updates and CI fixes",
        "review-ci-sha",
        CiStatus::Failure,
        Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 20, 0).unwrap()),
        ReviewDecision::ChangesRequested,
        0,
        Some(Utc.with_ymd_and_hms(2026, 3, 30, 18, 19, 0).unwrap()),
    )
}

fn base_pr(
    key: &str,
    number: u64,
    title: &str,
    head_sha: &str,
    ci_status: CiStatus,
    ci_updated_at: Option<chrono::DateTime<Utc>>,
    review_decision: ReviewDecision,
    approval_count: usize,
    latest_reviewer_activity_at: Option<chrono::DateTime<Utc>>,
) -> PullRequest {
    PullRequest {
        key: key.to_owned(),
        repo_full_name: "openai/symphony".to_owned(),
        number,
        title: title.to_owned(),
        body: Some("Test PR".to_owned()),
        url: format!("https://github.com/openai/symphony/pull/{number}"),
        author_login: "connor".to_owned(),
        labels: vec![],
        created_at: Utc.with_ymd_and_hms(2026, 3, 30, 18, 0, 0).unwrap(),
        updated_at: Utc
            .with_ymd_and_hms(2026, 3, 30, 18, 1, number as u32)
            .unwrap(),
        head_sha: head_sha.to_owned(),
        head_ref: format!("feature/{number}"),
        base_sha: format!("base-sha-{number}"),
        base_ref: "main".to_owned(),
        clone_url: "https://github.com/openai/symphony.git".to_owned(),
        ssh_url: "git@github.com:openai/symphony.git".to_owned(),
        ci_status,
        ci_updated_at,
        review_decision,
        approval_count,
        review_comment_count: usize::from(matches!(review_decision, ReviewDecision::Commented)),
        issue_comment_count: 0,
        latest_reviewer_activity_at,
        has_conflicts: false,
        mergeable_state: Some("clean".to_owned()),
        is_draft: false,
        is_closed: false,
        is_merged: false,
    }
}

fn fake_cli_transcript(request: &RunRequest, output: &str) -> String {
    let prompt = build_prompt(
        &request.pull_request,
        request.trigger,
        request.agent.additional_instructions.as_deref(),
    );

    format!(
        "=== Prompt Sent To Codex CLI ===\n{}\n\n=== Codex CLI Output ===\n{}",
        prompt.trim_end_matches('\n'),
        output
    )
}

fn unique_temp_path(file_name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should work")
        .as_nanos();
    let counter = TEMP_PATH_COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("symphony-rs-{nonce}-{counter}-{file_name}"))
}
