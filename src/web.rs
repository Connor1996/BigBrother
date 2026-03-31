use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::StatusCode,
    response::Html,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::{model::TrackedPr, service::Supervisor};

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Symphony RS</title>
  <style>
    :root {
      color-scheme: light;
      --bg: #f5f1e8;
      --panel: rgba(255, 252, 247, 0.92);
      --ink: #1e2228;
      --muted: #666b73;
      --line: rgba(30, 34, 40, 0.12);
      --accent: #1d6b57;
      --warn: #b25d18;
      --bad: #a53f3f;
      --shadow: 0 20px 60px rgba(47, 40, 28, 0.08);
    }

    * { box-sizing: border-box; }

    body {
      margin: 0;
      font-family: "Iowan Old Style", "Palatino Linotype", "Book Antiqua", serif;
      background:
        radial-gradient(circle at top left, rgba(255, 255, 255, 0.8), transparent 35%),
        linear-gradient(180deg, #f9f4ea 0%, var(--bg) 100%);
      color: var(--ink);
    }

    main {
      max-width: 1100px;
      margin: 0 auto;
      padding: 32px 20px 64px;
    }

    h1 {
      margin: 0 0 8px;
      font-size: clamp(2.2rem, 3vw, 3.4rem);
      font-weight: 700;
      letter-spacing: -0.04em;
    }

    p {
      margin: 0;
      color: var(--muted);
      line-height: 1.55;
    }

    .hero,
    .panel {
      background: var(--panel);
      border: 1px solid var(--line);
      border-radius: 22px;
      box-shadow: var(--shadow);
      backdrop-filter: blur(8px);
    }

    .hero {
      padding: 28px;
      margin-bottom: 18px;
    }

    .stats {
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
      gap: 14px;
      margin-top: 22px;
    }

    .stat {
      padding: 16px;
      border-radius: 18px;
      background: rgba(255, 255, 255, 0.65);
      border: 1px solid rgba(30, 34, 40, 0.08);
    }

    .stat label {
      display: block;
      margin-bottom: 8px;
      color: var(--muted);
      font-size: 0.92rem;
    }

    .stat strong {
      font-size: 1.15rem;
    }

    .panel {
      overflow: hidden;
    }

    table {
      width: 100%;
      border-collapse: collapse;
    }

    .pr-row td {
      transition: background 160ms ease, color 160ms ease;
    }

    .pr-row.paused-row td {
      background: rgba(30, 34, 40, 0.09);
      color: rgba(30, 34, 40, 0.76);
    }

    .pr-row.running-row td {
      background: rgba(29, 107, 87, 0.05);
    }

    th, td {
      text-align: left;
      padding: 14px 16px;
      vertical-align: top;
      border-bottom: 1px solid var(--line);
    }

    th {
      font-size: 0.82rem;
      text-transform: uppercase;
      letter-spacing: 0.08em;
      color: var(--muted);
      background: rgba(255, 255, 255, 0.55);
    }

    tr:last-child td {
      border-bottom: none;
    }

    a {
      color: inherit;
    }

    .pill {
      display: inline-flex;
      padding: 4px 10px;
      border-radius: 999px;
      font-size: 0.84rem;
      border: 1px solid var(--line);
      background: rgba(255, 255, 255, 0.7);
      white-space: nowrap;
    }

    .pill.good { color: var(--accent); }
    .pill.warn { color: var(--warn); }
    .pill.bad { color: var(--bad); }

    .summary {
      max-width: 34ch;
      color: var(--muted);
      white-space: pre-wrap;
    }

    .details-stack {
      display: grid;
      gap: 8px;
    }

    .output-label,
    .output-empty {
      font-size: 0.74rem;
      text-transform: uppercase;
      letter-spacing: 0.08em;
      color: var(--muted);
    }

    .live-output {
      margin: 0;
      padding: 12px;
      border-radius: 14px;
      border: 1px solid rgba(30, 34, 40, 0.08);
      background: rgba(30, 34, 40, 0.06);
      color: #23303a;
      font: 0.78rem/1.45 "SFMono-Regular", "SF Mono", ui-monospace, monospace;
      max-width: min(68ch, 100%);
      max-height: 220px;
      overflow: auto;
      white-space: pre-wrap;
      word-break: break-word;
    }

    .action-button {
      border: 1px solid var(--line);
      border-radius: 999px;
      background: rgba(255, 255, 255, 0.82);
      color: var(--ink);
      cursor: pointer;
      font: inherit;
      padding: 6px 12px;
      transition: background 120ms ease, transform 120ms ease;
    }

    .action-button:hover:not(:disabled) {
      background: rgba(255, 255, 255, 1);
      transform: translateY(-1px);
    }

    .action-button:disabled {
      cursor: wait;
      opacity: 0.65;
    }

    .empty {
      padding: 24px 18px;
      color: var(--muted);
    }

    @media (max-width: 900px) {
      table, thead, tbody, th, td, tr {
        display: block;
      }

      thead {
        display: none;
      }

      tr {
        padding: 16px;
        border-bottom: 1px solid var(--line);
      }

      td {
        padding: 6px 0;
        border: none;
      }

      td::before {
        content: attr(data-label);
        display: block;
        margin-bottom: 4px;
        font-size: 0.8rem;
        text-transform: uppercase;
        letter-spacing: 0.07em;
        color: var(--muted);
      }
    }
  </style>
</head>
<body>
  <main>
    <section class="hero">
      <h1>Symphony RS</h1>
      <p>Tracking authored GitHub pull requests, surfacing CI and review changes, and showing when the local agent has already taken a pass.</p>
      <div class="stats">
        <div class="stat">
          <label>Daemon</label>
          <strong id="health-status">Loading...</strong>
        </div>
        <div class="stat">
          <label>Tracked PRs</label>
          <strong id="health-count">-</strong>
        </div>
        <div class="stat">
          <label>Running</label>
          <strong id="health-running">-</strong>
        </div>
        <div class="stat">
          <label>Last Poll</label>
          <strong id="health-poll">-</strong>
        </div>
      </div>
    </section>

    <section class="panel">
      <table>
        <thead>
          <tr>
            <th>PR</th>
            <th>Status</th>
            <th>CI</th>
            <th>Reviews</th>
            <th>Attention</th>
            <th>Details</th>
            <th>Action</th>
          </tr>
        </thead>
        <tbody id="prs-table">
          <tr><td colspan="7" class="empty">Loading pull requests...</td></tr>
        </tbody>
      </table>
    </section>
  </main>

  <script>
    const pendingPauseKeys = new Set();

    function fmtTime(value) {
      if (!value) return "-";
      return new Date(value).toLocaleString();
    }

    function escapeHtml(value) {
      return String(value ?? "")
        .replaceAll("&", "&amp;")
        .replaceAll("<", "&lt;")
        .replaceAll(">", "&gt;")
        .replaceAll('"', "&quot;")
        .replaceAll("'", "&#39;");
    }

    function pillClass(value) {
      const label = String(value || "").toLowerCase();
      if (label.includes("fail") || label.includes("block") || label.includes("conflict")) return "pill bad";
      if (label.includes("pause")) return "pill warn";
      if (label.includes("need") || label.includes("pending") || label.includes("comment") || label.includes("retry")) return "pill warn";
      return "pill good";
    }

    function rowClass(pr) {
      const classes = ["pr-row"];
      if (pr.is_paused) classes.push("paused-row");
      if (pr.status === "running") classes.push("running-row");
      return classes.join(" ");
    }

    function renderDetails(pr) {
      const summary = `<div class="summary">${escapeHtml(pr.latest_summary || "-")}</div>`;
      if (pr.status !== "running") {
        return `<div class="details-stack">${summary}</div>`;
      }

      if (pr.live_output) {
        return `
          <div class="details-stack">
            ${summary}
            <div class="output-label">Live Codex CLI Output</div>
            <pre class="live-output">${escapeHtml(pr.live_output)}</pre>
          </div>
        `;
      }

      return `
        <div class="details-stack">
          ${summary}
          <div class="output-empty">Waiting for Codex CLI output...</div>
        </div>
      `;
    }

    function renderAction(pr) {
      if (!pr.can_toggle_pause) return "-";

      const pending = pendingPauseKeys.has(pr.key);
      const nextPaused = !pr.is_paused;
      const label = pending ? "Updating..." : (pr.is_paused ? "Resume" : "Pause");
      return `
        <button
          class="action-button"
          ${pending ? "disabled" : ""}
          onclick="togglePause('${encodeURIComponent(pr.key)}', ${nextPaused})"
        >
          ${label}
        </button>
      `;
    }

    async function togglePause(encodedKey, paused) {
      const key = decodeURIComponent(encodedKey);
      pendingPauseKeys.add(key);
      refresh().catch(() => {});

      try {
        const response = await fetch("/api/prs/pause", {
          method: "POST",
          headers: {
            "Content-Type": "application/json"
          },
          body: JSON.stringify({ key, paused })
        });

        if (!response.ok) {
          const message = await response.text();
          throw new Error(message || `HTTP ${response.status}`);
        }
      } catch (error) {
        window.alert(`Failed to update watch state: ${error.message}`);
      } finally {
        pendingPauseKeys.delete(key);
        refresh().catch(() => {});
      }
    }

    async function refresh() {
      const [healthRes, prsRes] = await Promise.all([
        fetch("/api/health"),
        fetch("/api/prs")
      ]);

      const health = await healthRes.json();
      const prsPayload = await prsRes.json();

      document.getElementById("health-status").textContent = health.ok ? "Healthy" : "Attention needed";
      document.getElementById("health-count").textContent = String(health.tracked_prs);
      document.getElementById("health-running").textContent = String(health.running_prs);
      document.getElementById("health-poll").textContent = fmtTime(health.last_poll_finished_at);

      const tbody = document.getElementById("prs-table");
      const prs = prsPayload.prs || [];
      if (!prs.length) {
        tbody.innerHTML = '<tr><td colspan="7" class="empty">No pull requests are currently being tracked.</td></tr>';
        return;
      }

      tbody.innerHTML = prs.map((pr) => `
        <tr class="${rowClass(pr)}">
          <td data-label="PR">
            <a href="${pr.url}" target="_blank" rel="noreferrer">${escapeHtml(pr.repo_full_name)} #${pr.number}</a>
            <div>${escapeHtml(pr.title)}</div>
            <div style="color: var(--muted); font-size: 0.9rem;">Updated ${escapeHtml(fmtTime(pr.updated_at))}</div>
          </td>
          <td data-label="Status"><span class="${pillClass(pr.status)}">${escapeHtml(pr.status)}</span></td>
          <td data-label="CI"><span class="${pillClass(pr.ci_status)}">${escapeHtml(pr.ci_status)}</span></td>
          <td data-label="Reviews"><span class="${pillClass(pr.review_status)}">${escapeHtml(pr.review_status)}</span></td>
          <td data-label="Attention">${escapeHtml(pr.attention_reason || "-")}</td>
          <td data-label="Details">${renderDetails(pr)}</td>
          <td data-label="Action">${renderAction(pr)}</td>
        </tr>
      `).join("");
    }

    refresh().catch((error) => {
      document.getElementById("prs-table").innerHTML =
        `<tr><td colspan="7" class="empty">Failed to load dashboard: ${error.message}</td></tr>`;
    });
    setInterval(() => refresh().catch(() => {}), 1500);
  </script>
</body>
</html>"#;

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
    tracked_prs: usize,
    running_prs: usize,
    last_poll_started_at: Option<DateTime<Utc>>,
    last_poll_finished_at: Option<DateTime<Utc>>,
    next_poll_due_at: Option<DateTime<Utc>>,
    last_poll_error: Option<String>,
}

#[derive(Debug, Serialize)]
struct PullRequestsResponse {
    prs: Vec<PullRequestSummary>,
}

#[derive(Debug, Serialize)]
struct PullRequestSummary {
    key: String,
    repo_full_name: String,
    number: u64,
    title: String,
    url: String,
    status: String,
    ci_status: String,
    review_status: String,
    is_paused: bool,
    can_toggle_pause: bool,
    attention_reason: Option<String>,
    updated_at: DateTime<Utc>,
    latest_summary: Option<String>,
    live_output: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PauseRequest {
    key: String,
    paused: bool,
}

#[derive(Debug, Serialize)]
struct PauseResponse {
    ok: bool,
    pr: PullRequestSummary,
}

pub fn default_listen_addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8787)
}

pub fn router(supervisor: Arc<Supervisor>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/health", get(health))
        .route("/api/prs", get(list_prs))
        .route("/api/prs/pause", post(set_pr_paused))
        .with_state(supervisor)
}

pub async fn serve(
    supervisor: Arc<Supervisor>,
    listen_addr: SocketAddr,
    stop_flag: Arc<AtomicBool>,
) -> Result<()> {
    let listener = TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("failed to bind {listen_addr}"))?;

    println!("Symphony RS listening on http://{listen_addr}");

    axum::serve(listener, router(supervisor))
        .with_graceful_shutdown(shutdown_signal(stop_flag))
        .await
        .context("HTTP server exited unexpectedly")
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn health(State(supervisor): State<Arc<Supervisor>>) -> Json<HealthResponse> {
    let snapshot = supervisor.snapshot();
    let running_prs = snapshot
        .tracked_prs
        .values()
        .filter(|pr| pr.status == crate::model::TrackingStatus::Running)
        .count();

    Json(HealthResponse {
        ok: snapshot.last_poll_error.is_none(),
        tracked_prs: snapshot.tracked_prs.len(),
        running_prs,
        last_poll_started_at: snapshot.last_poll_started_at,
        last_poll_finished_at: snapshot.last_poll_finished_at,
        next_poll_due_at: snapshot.next_poll_due_at,
        last_poll_error: snapshot.last_poll_error,
    })
}

async fn list_prs(State(supervisor): State<Arc<Supervisor>>) -> Json<PullRequestsResponse> {
    let snapshot = supervisor.snapshot();
    let mut prs = snapshot
        .tracked_prs
        .values()
        .map(summarize_pr)
        .collect::<Vec<_>>();

    prs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));

    Json(PullRequestsResponse { prs })
}

async fn set_pr_paused(
    State(supervisor): State<Arc<Supervisor>>,
    Json(request): Json<PauseRequest>,
) -> Result<Json<PauseResponse>, (StatusCode, String)> {
    let updated = supervisor
        .set_pr_paused(&request.key, request.paused)
        .await
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed updating watch state: {error:#}"),
            )
        })?;

    let Some(tracked) = updated else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("unknown PR key: {}", request.key),
        ));
    };

    Ok(Json(PauseResponse {
        ok: true,
        pr: summarize_pr(&tracked),
    }))
}

fn summarize_pr(tracked: &TrackedPr) -> PullRequestSummary {
    PullRequestSummary {
        key: tracked.pull_request.key.clone(),
        repo_full_name: tracked.pull_request.repo_full_name.clone(),
        number: tracked.pull_request.number,
        title: tracked.pull_request.title.clone(),
        url: tracked.pull_request.url.clone(),
        status: tracked.status.label().to_owned(),
        ci_status: tracked.pull_request.ci_status.label().to_owned(),
        review_status: tracked.pull_request.review_decision.label().to_owned(),
        is_paused: tracked.persisted.paused,
        can_toggle_pause: !tracked.pull_request.is_closed && !tracked.pull_request.is_merged,
        attention_reason: tracked
            .attention_reason
            .map(|reason| reason.label().to_owned()),
        updated_at: tracked.pull_request.updated_at,
        latest_summary: tracked
            .runner
            .as_ref()
            .map(|runner| runner.summary.clone())
            .or_else(|| tracked.persisted.last_run_summary.clone()),
        live_output: tracked
            .runner
            .as_ref()
            .and_then(|runner| runner.live_output.clone()),
    }
}

async fn shutdown_signal(stop_flag: Arc<AtomicBool>) {
    let _ = tokio::signal::ctrl_c().await;
    stop_flag.store(true, Ordering::Relaxed);
}
