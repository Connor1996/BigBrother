use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
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
    model::{CiStatus, PersistentPrState, PullRequest, ReviewDecision, TrackedPr, TrackingStatus},
    runner::{RunOutcome, RunRequest},
    service::{AgentRunner, GitHubProvider, Supervisor},
    web,
};
use tokio::sync::Semaphore;
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
            if let Some(output_updates) = request.output_updates.as_ref() {
                let _ =
                    output_updates.send("codex: inspecting workspace\ncargo test -q\n".to_owned());
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
}

impl AgentRunner for AlwaysFailingAgentRunner {
    fn run(&self, request: RunRequest) -> BoxFuture<'static, RunOutcome> {
        let invocations = self.invocations.clone();

        Box::pin(async move {
            invocations.fetch_add(1, Ordering::SeqCst);
            if let Some(output_updates) = request.output_updates.as_ref() {
                let _ = output_updates.send("codex: run failed before fix\n".to_owned());
            }

            RunOutcome {
                started_at: Utc::now(),
                finished_at: Utc::now(),
                success: false,
                exit_code: Some(1),
                summary: format!("failed {}", request.pull_request.key),
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
        Some("fixed openai/symphony#7"),
        "latest summary should reflect runner output",
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
async fn approved_pr_with_pending_ci_stays_waiting_review() {
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
        Some("waiting review")
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
async fn running_pr_exposes_live_codex_output() {
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
    assert_eq!(
        running_pr["live_output"],
        json!("codex: inspecting workspace\ncargo test -q\n")
    );

    let detail_payload = get_json(supervisor.clone(), "/api/pr?key=openai%2Fsymphony%237").await;
    assert_eq!(detail_payload["key"], json!("openai/symphony#7"));
    assert_eq!(
        detail_payload["live_output"],
        json!("codex: inspecting workspace\ncargo test -q\n")
    );

    runner.allow_finish.add_permits(1);
    poll_task
        .await
        .expect("poll task should join")
        .expect("poll should succeed");
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

    supervisor
        .poll_once()
        .await
        .expect("poll should succeed after resume");
    assert_eq!(
        runner.invocations.load(Ordering::SeqCst),
        1,
        "resumed actionable PR should trigger the runner again",
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

    supervisor
        .poll_once()
        .await
        .expect("poll should succeed after resume");
    assert_eq!(
        runner.invocations.load(Ordering::SeqCst),
        7,
        "resume should allow the current actionable signal to be retried again",
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

fn unique_temp_path(file_name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should work")
        .as_nanos();
    let counter = TEMP_PATH_COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("symphony-rs-{nonce}-{counter}-{file_name}"))
}
