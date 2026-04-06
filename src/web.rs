use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{header, StatusCode},
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::{
    model::{
        ActivityEvent, AttentionReason, PersistentPrState, ReviewRequestPr, TrackedPr,
        NEEDS_DECISION_SUMMARY,
    },
    runner::OUTPUT_TRANSCRIPT_HEADER,
    service::{Supervisor, TerminalSubscription},
};

const BIGBROTHER_MARK_PATH: &str = "/assets/bigbrother-mark.png";
const BIGBROTHER_MARK_PNG: &[u8] = include_bytes!("../assets/bigbrother-mark.png");
const XTERM_CSS_PATH: &str = "/assets/xterm.min.css";
const XTERM_CSS: &[u8] = include_bytes!("../assets/xterm.min.css");
const XTERM_JS_PATH: &str = "/assets/xterm.min.js";
const XTERM_JS: &[u8] = include_bytes!("../assets/xterm.min.js");
const XTERM_ADDON_FIT_JS_PATH: &str = "/assets/xterm-addon-fit.min.js";
const XTERM_ADDON_FIT_JS: &[u8] = include_bytes!("../assets/xterm-addon-fit.min.js");

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>BigBrother</title>
  <link rel="icon" type="image/png" href="/assets/bigbrother-mark.png">
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

    .brand-lockup {
      display: flex;
      align-items: center;
      gap: 18px;
      margin-bottom: 8px;
    }

    .brand-mark {
      width: clamp(78px, 8vw, 104px);
      flex-shrink: 0;
    }

    .brand-mark img {
      display: block;
      width: 100%;
      height: auto;
    }

    .brand-copy {
      min-width: 0;
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

    .hero-head {
      display: flex;
      align-items: flex-start;
      justify-content: space-between;
      gap: 18px;
    }

    .hero-copy {
      min-width: 0;
    }

    .view-tabs {
      display: inline-flex;
      align-items: center;
      gap: 8px;
      padding: 6px;
      border-radius: 999px;
      border: 1px solid rgba(30, 34, 40, 0.08);
      background: rgba(255, 255, 255, 0.72);
      flex-shrink: 0;
    }

    .view-tab {
      border: none;
      border-radius: 999px;
      background: transparent;
      color: var(--muted);
      cursor: pointer;
      font: inherit;
      padding: 8px 14px;
      transition: background 120ms ease, color 120ms ease, transform 120ms ease;
    }

    .view-tab:hover {
      color: var(--ink);
      transform: translateY(-1px);
    }

    .view-tab.active {
      background: rgba(29, 107, 87, 0.12);
      color: var(--accent);
      box-shadow: inset 0 0 0 1px rgba(29, 107, 87, 0.14);
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

    .panel + .panel {
      margin-top: 18px;
    }

    .dashboard-view.is-hidden {
      display: none;
    }

    table {
      width: 100%;
      border-collapse: collapse;
    }

    .description-col {
      width: 38%;
    }

    .description-cell {
      min-width: 280px;
    }

    .metric-col,
    .metric-cell {
      text-align: center;
    }

    .metric-cell {
      vertical-align: middle;
    }

    .pr-title {
      margin-top: 6px;
      font-weight: 600;
    }

    .pr-meta {
      margin-top: 4px;
      color: var(--muted);
      font-size: 0.9rem;
    }

    .pr-row td {
      transition: background 160ms ease, color 160ms ease;
    }

    .pr-row.untracked-row td {
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
      align-items: center;
      justify-content: center;
      box-sizing: border-box;
      min-height: 1.9rem;
      padding: 4px 10px;
      border-radius: 999px;
      font-size: 0.84rem;
      line-height: 1;
      font-weight: 600;
      border: 1px solid var(--line);
      background: rgba(255, 255, 255, 0.7);
      white-space: nowrap;
    }

    .pill.good { color: var(--accent); }
    .pill.warn { color: var(--warn); }
    .pill.bad { color: var(--bad); }

    .status-stack {
      display: grid;
      gap: 6px;
    }

    .status-cell .status-stack,
    .details-cell .details-stack {
      justify-items: center;
    }

    .summary {
      max-width: 34ch;
      color: var(--muted);
      text-align: center;
      white-space: pre-wrap;
    }

    .details-stack {
      display: grid;
      gap: 8px;
    }

    .detail-meta {
      font-size: 0.78rem;
      color: var(--muted);
      white-space: nowrap;
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
      display: inline-flex;
      align-items: center;
      justify-content: center;
      gap: 8px;
      border: none;
      border-radius: 999px;
      background: rgba(255, 255, 255, 0.82);
      color: var(--ink);
      cursor: pointer;
      font: inherit;
      font-weight: 600;
      padding: 6px 12px;
      transition: background 120ms ease, transform 120ms ease;
    }

    .action-stack {
      display: inline-grid;
      gap: 8px;
      justify-items: center;
    }

    .action-stack .action-button {
      min-width: 104px;
    }

    .action-button.untrack-button {
      background: #a6927a;
      color: #fff;
    }

    .action-button.untrack-button:hover:not(:disabled) {
      background: #958069;
    }

    .action-button.track-button {
      background: #557da9;
      color: #fff;
    }

    .action-button.track-button:hover:not(:disabled) {
      background: #476f99;
    }

    .action-button.deep-review-button {
      background: #6f8896;
      color: #fff;
    }

    .action-button.deep-review-button:hover:not(:disabled) {
      background: #627987;
    }

    .action-button.retry-button,
    .action-button.addressed-button {
      background: #b99a49;
      color: #fff;
    }

    .action-button.retry-button:hover:not(:disabled),
    .action-button.addressed-button:hover:not(:disabled) {
      background: #a9893d;
    }

    .action-button:hover:not(:disabled) {
      transform: translateY(-1px);
    }

    .action-button:not(.untrack-button):not(.track-button):not(.deep-review-button):not(.retry-button):not(.addressed-button):hover:not(:disabled) {
      background: rgba(255, 255, 255, 1);
    }

    .action-button:disabled {
      cursor: wait;
      opacity: 0.65;
    }

    .button-icon {
      font-size: 0.9em;
      line-height: 1;
    }

    .button-icon svg {
      display: block;
      width: 0.95em;
      height: 0.95em;
    }

    .detail-link {
      display: inline-flex;
      align-items: center;
      width: fit-content;
      color: var(--accent);
      text-decoration: none;
      font-size: 0.88rem;
      border-bottom: 1px solid rgba(29, 107, 87, 0.25);
    }

    .detail-link:hover {
      border-bottom-color: rgba(29, 107, 87, 0.8);
    }

    .empty {
      padding: 24px 18px;
      color: var(--muted);
    }

    .panel-header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 12px;
      padding: 18px 18px 0;
    }

    .panel-title {
      margin: 0;
      font-size: 1rem;
      letter-spacing: 0.02em;
    }

    .activity-feed {
      padding: 12px 18px 18px;
      display: grid;
      gap: 10px;
    }

    .activity-item {
      padding: 12px 14px;
      border-radius: 16px;
      border: 1px solid rgba(30, 34, 40, 0.08);
      background: rgba(255, 255, 255, 0.62);
    }

    .activity-item.error {
      border-color: rgba(165, 63, 63, 0.18);
      background: rgba(165, 63, 63, 0.05);
    }

    .activity-meta {
      display: flex;
      flex-wrap: wrap;
      gap: 8px 10px;
      align-items: center;
      margin-bottom: 6px;
      color: var(--muted);
      font-size: 0.8rem;
      text-transform: uppercase;
      letter-spacing: 0.06em;
    }

    .activity-message {
      margin: 0;
      color: var(--ink);
      line-height: 1.5;
      white-space: pre-wrap;
      word-break: break-word;
    }

    @media (max-width: 900px) {
      .hero-head {
        flex-direction: column;
        align-items: stretch;
      }

      .view-tabs {
        width: fit-content;
      }

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
      <div class="hero-head">
        <div class="hero-copy">
          <div class="brand-lockup">
            <div class="brand-mark" aria-hidden="true">
              <img src="/assets/bigbrother-mark.png" alt="" />
            </div>
            <div class="brand-copy">
              <h1>BigBrother</h1>
            </div>
          </div>
          <p>Tracking authored GitHub pull requests, surfacing CI and review changes, and showing when the local agent has already taken a pass.</p>
        </div>
        <div class="view-tabs" role="tablist" aria-label="Dashboard views">
          <button id="tab-prs" class="view-tab active" type="button" data-view="prs" aria-selected="true">PRs</button>
          <button id="tab-review-requests" class="view-tab" type="button" data-view="review-requests" aria-selected="false">Review Requests</button>
          <button id="tab-activity" class="view-tab" type="button" data-view="activity" aria-selected="false">Activity</button>
        </div>
      </div>
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

    <section id="view-prs" class="panel dashboard-view">
      <table>
        <thead>
          <tr>
            <th class="description-col">PR</th>
            <th class="metric-col">Status</th>
            <th class="metric-col">CI</th>
            <th class="metric-col">Reviews</th>
            <th class="metric-col">Details</th>
            <th class="metric-col">Action</th>
          </tr>
        </thead>
        <tbody id="prs-table">
          <tr><td colspan="6" class="empty">Loading pull requests...</td></tr>
        </tbody>
      </table>
    </section>

    <section id="view-review-requests" class="panel dashboard-view is-hidden">
      <table>
        <thead>
          <tr>
            <th class="description-col">PR</th>
            <th class="metric-col">Status</th>
            <th class="metric-col">Details</th>
            <th class="metric-col">Action</th>
          </tr>
        </thead>
        <tbody id="review-requests-table">
          <tr><td colspan="4" class="empty">Loading requested reviews...</td></tr>
        </tbody>
      </table>
    </section>

    <section id="view-activity" class="panel dashboard-view is-hidden">
      <div class="panel-header">
        <h2 class="panel-title">Daemon Activity</h2>
        <span id="activity-count" class="output-label">-</span>
      </div>
      <div id="activity-feed" class="activity-feed">
        <div class="empty">Loading daemon activity...</div>
      </div>
    </section>
  </main>

  <script>
    const pendingPauseKeys = new Set();
    const pendingRetryKeys = new Set();
    const pendingDeepReviewKeys = new Set();
    const optimisticPausedStates = new Map();
    const dashboardViewStorageKey = "bigbrother.dashboard-view";
    let latestPrs = [];
    let latestReviewRequests = [];
    let currentView = "prs";

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
      if (!label || label === "-" || label === "not loaded") return "pill";
      if (label === "requested review") return "pill warn";
      if (label === "reviewed") return "pill good";
      if (label.includes("fail") || label.includes("block") || label.includes("conflict")) return "pill bad";
      if (label.includes("untrack") || label.includes("pause")) return "pill warn";
      if (label.includes("need") || label.includes("pending") || label.includes("comment") || label.includes("retry")) return "pill warn";
      return "pill good";
    }

    function effectivePaused(pr) {
      return optimisticPausedStates.has(pr.key) ? optimisticPausedStates.get(pr.key) : pr.is_paused;
    }

    function displayUntracked(pr) {
      return effectivePaused(pr) && pr.status === "untracked";
    }

    function rowClass(pr) {
      const classes = ["pr-row"];
      if (displayUntracked(pr)) classes.push("untracked-row");
      if (pr.status === "running") classes.push("running-row");
      return classes.join(" ");
    }

    function renderDetails(pr) {
      const summary = `<div class="summary">${escapeHtml(pr.latest_summary || "-")}</div>`;
      const detailLabel = pr.status === "running" ? "Open live output" : "Open details";
      const detailMeta = pr.details_at && pr.details_label
        ? `<div class="detail-meta">${escapeHtml(pr.details_label)} ${escapeHtml(fmtTime(pr.details_at))}</div>`
        : "";
      return `
        <div class="details-stack">
          ${summary}
          <a class="detail-link" href="/pr?key=${encodeURIComponent(pr.key)}">${detailLabel}</a>
          ${detailMeta}
        </div>
      `;
    }

    function renderAction(pr) {
      const actions = [];

      if (pr.status === "failed") {
        const pending = pendingRetryKeys.has(pr.key);
        actions.push(`
          <button
            class="action-button retry-button"
            ${pending ? "disabled" : ""}
            onclick="triggerRetry('${encodeURIComponent(pr.key)}')"
          >
            <span class="button-icon" aria-hidden="true">&#8635;</span>
            <span>Retry</span>
          </button>
        `);
      }

      if (pr.status === "needs decision") {
        const pending = pendingPauseKeys.has(pr.key);
        actions.push(`
          <button
            class="action-button addressed-button"
            ${pending ? "disabled" : ""}
            onclick="togglePause('${encodeURIComponent(pr.key)}', false)"
          >
            <span class="button-icon" aria-hidden="true">&#10003;</span>
            <span>${pending ? "Updating..." : "Addressed"}</span>
          </button>
        `);
      }

      if (pr.can_toggle_pause) {
        const pending = pendingPauseKeys.has(pr.key);
        const isPaused = effectivePaused(pr);
        const nextPaused = !isPaused;
        const label = pending ? "Updating..." : (isPaused ? "Track" : "Untrack");
        const variantClass = isPaused ? "track-button" : "untrack-button";
        const icon = isPaused ? "&#43;" : "&#8722;";
        actions.push(`
          <button
            class="action-button ${variantClass}"
            ${pending ? "disabled" : ""}
            onclick="togglePause('${encodeURIComponent(pr.key)}', ${nextPaused})"
          >
            <span class="button-icon" aria-hidden="true">${icon}</span>
            <span>${label}</span>
          </button>
        `);
      }

      if (!actions.length) return "-";
      return `<div class="action-stack">${actions.join("")}</div>`;
    }

    function renderStatus(pr) {
      return `
        <div class="status-stack">
          <span class="${pillClass(pr.status)}">${escapeHtml(pr.status)}</span>
        </div>
      `;
    }

    function renderReviewRequestAction(pr) {
      const pending = pendingDeepReviewKeys.has(pr.key);
      const running = pr.status === "running";
      const label = pending ? "Starting..." : (running ? "Running..." : "Deep Review");
      const icon = `
        <span class="button-icon" aria-hidden="true">
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="7" cy="7" r="4.5"></circle>
            <path d="M11.5 11.5L14 14"></path>
          </svg>
        </span>
      `;
      return `
        <button
          class="action-button deep-review-button"
          ${(pending || running) ? "disabled" : ""}
          onclick="triggerDeepReview('${encodeURIComponent(pr.key)}')"
        >
          ${icon}
          <span>${label}</span>
        </button>
      `;
    }

    async function togglePause(encodedKey, paused) {
      const key = decodeURIComponent(encodedKey);
      pendingPauseKeys.add(key);
      optimisticPausedStates.set(key, paused);
      renderPrs(latestPrs);

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

        const payload = await response.json();
        latestPrs = latestPrs.map((pr) => pr.key === key ? payload.pr : pr);
        optimisticPausedStates.delete(key);
        pendingPauseKeys.delete(key);
        renderPrs(latestPrs);
      } catch (error) {
        optimisticPausedStates.delete(key);
        pendingPauseKeys.delete(key);
        renderPrs(latestPrs);
        window.alert(`Failed to update watch state: ${error.message}`);
      } finally {
        pendingPauseKeys.delete(key);
        refresh().catch(() => {});
      }
    }

    async function triggerRetry(encodedKey) {
      const key = decodeURIComponent(encodedKey);
      pendingRetryKeys.add(key);
      renderPrs(latestPrs);

      try {
        const response = await fetch("/api/prs/retry", {
          method: "POST",
          headers: {
            "Content-Type": "application/json"
          },
          body: JSON.stringify({ key })
        });

        if (!response.ok) {
          const message = await response.text();
          throw new Error(message || `HTTP ${response.status}`);
        }

        const payload = await response.json();
        latestPrs = latestPrs.map((pr) => pr.key === key ? payload.pr : pr);
        pendingRetryKeys.delete(key);
        renderPrs(latestPrs);
      } catch (error) {
        pendingRetryKeys.delete(key);
        renderPrs(latestPrs);
        alert(`Failed to retry ${key}: ${error.message || error}`);
      }
    }

    async function triggerDeepReview(encodedKey) {
      const key = decodeURIComponent(encodedKey);
      pendingDeepReviewKeys.add(key);
      renderReviewRequests(latestReviewRequests);

      try {
        const response = await fetch("/api/review-requests/deep-review", {
          method: "POST",
          headers: {
            "Content-Type": "application/json"
          },
          body: JSON.stringify({ key })
        });

        if (!response.ok) {
          const message = await response.text();
          throw new Error(message || `HTTP ${response.status}`);
        }

        const payload = await response.json();
        latestReviewRequests = latestReviewRequests.map((pr) => pr.key === key ? payload.pr : pr);
        renderReviewRequests(latestReviewRequests);
      } catch (error) {
        window.alert(`Failed to start deep review: ${error.message}`);
      } finally {
        pendingDeepReviewKeys.delete(key);
        refresh().catch(() => {});
      }
    }

    function renderPrs(prs) {
      const tbody = document.getElementById("prs-table");
      if (!prs.length) {
        tbody.innerHTML = '<tr><td colspan="6" class="empty">No pull requests are currently being tracked.</td></tr>';
        return;
      }

      tbody.innerHTML = prs.map((pr) => `
        <tr class="${rowClass(pr)}">
          <td class="description-cell" data-label="PR">
            <a href="${pr.url}" target="_blank" rel="noreferrer">${escapeHtml(pr.repo_full_name)} #${pr.number}</a>
            <div class="pr-title">${escapeHtml(pr.title)}</div>
            <div class="pr-meta">Updated ${escapeHtml(fmtTime(pr.updated_at))}</div>
          </td>
          <td class="metric-cell status-cell" data-label="Status">${renderStatus(pr)}</td>
          <td class="metric-cell" data-label="CI"><span class="${pillClass(pr.ci_status)}">${escapeHtml(pr.ci_status)}</span></td>
          <td class="metric-cell" data-label="Reviews"><span class="${pillClass(pr.review_status)}">${escapeHtml(pr.review_status)}</span></td>
          <td class="metric-cell details-cell" data-label="Details">${renderDetails(pr)}</td>
          <td class="metric-cell action-cell" data-label="Action">${renderAction(pr)}</td>
        </tr>
      `).join("");
    }

    function renderReviewRequests(prs) {
      const tbody = document.getElementById("review-requests-table");
      if (!prs.length) {
        tbody.innerHTML = '<tr><td colspan="4" class="empty">No PRs are currently requesting your review.</td></tr>';
        return;
      }

      tbody.innerHTML = prs.map((pr) => `
        <tr class="${rowClass(pr)}">
          <td class="description-cell" data-label="PR">
            <a href="${pr.url}" target="_blank" rel="noreferrer">${escapeHtml(pr.repo_full_name)} #${pr.number}</a>
            <div class="pr-title">${escapeHtml(pr.title)}</div>
            <div class="pr-meta">Updated ${escapeHtml(fmtTime(pr.updated_at))}</div>
          </td>
          <td class="metric-cell status-cell" data-label="Status">${renderStatus(pr)}</td>
          <td class="metric-cell details-cell" data-label="Details">${renderDetails(pr)}</td>
          <td class="metric-cell action-cell" data-label="Action">${renderReviewRequestAction(pr)}</td>
        </tr>
      `).join("");
    }

    function setDashboardView(view, persist = true) {
      currentView = ["prs", "review-requests", "activity"].includes(view) ? view : "prs";
      document.getElementById("view-prs").classList.toggle("is-hidden", currentView !== "prs");
      document.getElementById("view-review-requests").classList.toggle("is-hidden", currentView !== "review-requests");
      document.getElementById("view-activity").classList.toggle("is-hidden", currentView !== "activity");

      const prTab = document.getElementById("tab-prs");
      const reviewRequestsTab = document.getElementById("tab-review-requests");
      const activityTab = document.getElementById("tab-activity");
      prTab.classList.toggle("active", currentView === "prs");
      reviewRequestsTab.classList.toggle("active", currentView === "review-requests");
      activityTab.classList.toggle("active", currentView === "activity");
      prTab.setAttribute("aria-selected", String(currentView === "prs"));
      reviewRequestsTab.setAttribute("aria-selected", String(currentView === "review-requests"));
      activityTab.setAttribute("aria-selected", String(currentView === "activity"));
      prTab.setAttribute("tabindex", currentView === "prs" ? "0" : "-1");
      reviewRequestsTab.setAttribute("tabindex", currentView === "review-requests" ? "0" : "-1");
      activityTab.setAttribute("tabindex", currentView === "activity" ? "0" : "-1");

      if (persist) {
        try {
          window.localStorage.setItem(dashboardViewStorageKey, currentView);
        } catch (_) {}
      }
    }

    function renderActivity(events) {
      const container = document.getElementById("activity-feed");
      document.getElementById("activity-count").textContent = `${events.length} recent events`;

      if (!events.length) {
        container.innerHTML = '<div class="empty">No daemon activity has been recorded yet.</div>';
        return;
      }

      container.innerHTML = events.map((event) => `
        <article class="activity-item ${escapeHtml(event.level)}">
          <div class="activity-meta">
            <span>${escapeHtml(event.level)}</span>
            <span>${escapeHtml(fmtTime(event.timestamp))}</span>
            <span>${escapeHtml(event.pr_key || "global")}</span>
          </div>
          <p class="activity-message">${escapeHtml(event.message)}</p>
        </article>
      `).join("");
    }

    function restoreDashboardView() {
      try {
        return window.localStorage.getItem(dashboardViewStorageKey) || "prs";
      } catch (_) {
        return "prs";
      }
    }

    async function refresh() {
      const [healthRes, prsRes, reviewRequestsRes, activityRes] = await Promise.all([
        fetch("/api/health"),
        fetch("/api/prs"),
        fetch("/api/review-requests"),
        fetch("/api/activity")
      ]);

      const health = await healthRes.json();
      const prsPayload = await prsRes.json();
      const reviewRequestsPayload = await reviewRequestsRes.json();
      const activityPayload = await activityRes.json();

      document.getElementById("health-status").textContent = health.ok ? "Healthy" : "Attention needed";
      const trackedCount = typeof health.active_tracked_prs === "number"
        ? health.active_tracked_prs
        : health.tracked_prs;
      document.getElementById("health-count").textContent = `${trackedCount}/${health.all_prs}`;
      document.getElementById("health-running").textContent = String(health.running_prs);
      document.getElementById("health-poll").textContent = fmtTime(health.last_poll_finished_at);
      latestPrs = prsPayload.prs || [];
      latestReviewRequests = reviewRequestsPayload.prs || [];
      renderPrs(latestPrs);
      renderReviewRequests(latestReviewRequests);
      renderActivity(activityPayload.events || []);
    }

    setDashboardView(restoreDashboardView(), false);
    document.getElementById("tab-prs").addEventListener("click", () => setDashboardView("prs"));
    document.getElementById("tab-review-requests").addEventListener("click", () => setDashboardView("review-requests"));
    document.getElementById("tab-activity").addEventListener("click", () => setDashboardView("activity"));

    refresh().catch((error) => {
      document.getElementById("prs-table").innerHTML =
        `<tr><td colspan="7" class="empty">Failed to load dashboard: ${error.message}</td></tr>`;
      document.getElementById("review-requests-table").innerHTML =
        `<tr><td colspan="4" class="empty">Failed to load review requests: ${error.message}</td></tr>`;
      document.getElementById("activity-feed").innerHTML =
        `<div class="empty">Failed to load daemon activity: ${error.message}</div>`;
    });
    setInterval(() => refresh().catch(() => {}), 1500);
  </script>
</body>
</html>"#;

const PR_DETAIL_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>BigBrother Run View</title>
  <link rel="icon" type="image/png" href="/assets/bigbrother-mark.png">
  <link rel="stylesheet" href="/assets/xterm.min.css">
  <script src="/assets/xterm.min.js"></script>
  <script src="/assets/xterm-addon-fit.min.js"></script>
  <style>
    :root {
      color-scheme: light;
      --bg: #f5f1e8;
      --panel: rgba(255, 252, 247, 0.94);
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
      max-width: 1080px;
      margin: 0 auto;
      padding: 32px 20px 64px;
    }

    a {
      color: inherit;
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

    .hero-head {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 18px;
      margin-bottom: 18px;
    }

    .brand-lockup {
      display: flex;
      align-items: center;
      gap: 18px;
    }

    .brand-mark {
      width: clamp(78px, 8vw, 104px);
      flex-shrink: 0;
    }

    .brand-mark img {
      display: block;
      width: 100%;
      height: auto;
    }

    .brand-copy {
      min-width: 0;
    }

    .hero-copy {
      min-width: 0;
      margin-bottom: 20px;
    }

    .pr-title {
      margin-top: 6px;
      font-weight: 600;
      font-size: 1.12rem;
      line-height: 1.45;
    }

    .pr-meta {
      margin-top: 4px;
      color: var(--muted);
      font-size: 0.9rem;
    }

    .meta {
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
      gap: 14px;
    }

    .meta-card {
      padding: 14px 16px;
      border-radius: 18px;
      background: rgba(255, 255, 255, 0.65);
      border: 1px solid rgba(30, 34, 40, 0.08);
    }

    .meta-card label {
      display: block;
      margin-bottom: 8px;
      color: var(--muted);
      font-size: 0.82rem;
      text-transform: uppercase;
      letter-spacing: 0.08em;
    }

    .panel {
      padding: 24px;
    }

    .toolbar {
      display: flex;
      flex-wrap: wrap;
      align-items: center;
      gap: 12px;
      margin-bottom: 18px;
    }

    .detail-link {
      color: var(--accent);
      text-decoration: none;
      border-bottom: 1px solid rgba(29, 107, 87, 0.25);
      width: fit-content;
    }

    .detail-link:hover {
      border-bottom-color: rgba(29, 107, 87, 0.8);
    }

    .is-hidden {
      display: none;
    }

    .pill {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      box-sizing: border-box;
      min-height: 1.9rem;
      padding: 4px 10px;
      border-radius: 999px;
      font-size: 0.84rem;
      line-height: 1;
      font-weight: 600;
      border: 1px solid var(--line);
      background: rgba(255, 255, 255, 0.7);
      white-space: nowrap;
    }

    .pill.good { color: var(--accent); }
    .pill.warn { color: var(--warn); }
    .pill.bad { color: var(--bad); }

    .summary-block {
      margin-bottom: 18px;
    }

    .section-label {
      display: block;
      margin-bottom: 8px;
      color: var(--muted);
      font-size: 0.82rem;
      text-transform: uppercase;
      letter-spacing: 0.08em;
    }

    .summary-text {
      color: var(--muted);
      white-space: pre-wrap;
      line-height: 1.5;
    }

    .terminal-block {
      margin-bottom: 18px;
    }

    .terminal-meta {
      margin-bottom: 8px;
      color: var(--muted);
      font-size: 0.78rem;
    }

    .terminal-stage {
      position: relative;
    }

    .terminal-shell {
      --terminal-inner-gutter: 8px;
      --terminal-scrollbar-gutter: 0px;
      min-height: 320px;
      max-height: 70vh;
      overflow: hidden;
      padding: 16px 0 16px 16px;
      border-radius: 16px;
      border: 1px solid rgba(0, 0, 0, 0.32);
      background: #1e1e1e;
      box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.03);
    }

    .terminal-shell.is-hidden,
    .output.is-hidden,
    .terminal-empty-hint.is-hidden {
      display: none;
    }

    .terminal-shell .xterm {
      width: 100%;
      box-sizing: border-box;
      height: calc(70vh - 32px);
      min-height: 288px;
      padding-right: calc(
        var(--terminal-inner-gutter) + var(--terminal-scrollbar-gutter)
      );
    }

    .terminal-shell .xterm-viewport {
      border-radius: 10px;
      scrollbar-color: rgba(255, 255, 255, 0.26) transparent;
      scrollbar-width: thin;
      scrollbar-gutter: stable both-edges;
      right: 0;
      overflow-x: hidden;
    }

    .terminal-empty-hint {
      position: absolute;
      top: 16px;
      left: 16px;
      right: calc(
        var(--terminal-inner-gutter) + var(--terminal-scrollbar-gutter)
      );
      color: rgba(204, 204, 204, 0.8);
      font: 0.82rem/1.45 Menlo, Monaco, "SF Mono", ui-monospace, monospace;
      pointer-events: none;
    }

    .output {
      margin: 0;
      min-height: 320px;
      max-height: 70vh;
      overflow: auto;
      padding: 16px;
      border-radius: 16px;
      border: 1px solid rgba(30, 34, 40, 0.08);
      background: rgba(30, 34, 40, 0.06);
      color: #23303a;
      font: 0.82rem/1.5 Menlo, Monaco, "SF Mono", ui-monospace, monospace;
      white-space: pre-wrap;
      word-break: break-word;
    }

    .empty {
      color: var(--muted);
    }
  </style>
</head>
<body>
  <main>
    <section class="hero">
      <div class="hero-head">
        <div class="brand-lockup">
          <div class="brand-mark" aria-hidden="true">
            <img src="/assets/bigbrother-mark.png" alt="" />
          </div>
          <div class="brand-copy">
            <h1>BigBrother</h1>
          </div>
        </div>
        <a class="detail-link" href="/">Back to dashboard</a>
      </div>
      <div class="hero-copy">
        <a id="pr-link" class="is-hidden" href="#" target="_blank" rel="noreferrer">PR</a>
        <div id="title" class="pr-title">Loading PR details…</div>
        <div id="subtitle" class="pr-meta">Fetching PR details…</div>
      </div>
      <div class="meta">
        <div class="meta-card">
          <label>Status</label>
          <div id="status-pill">-</div>
        </div>
        <div class="meta-card">
          <label>CI</label>
          <div id="ci-pill">-</div>
        </div>
        <div class="meta-card">
          <label>Reviews</label>
          <div id="review-pill">-</div>
        </div>
        <div class="meta-card">
          <label>Updated</label>
          <strong id="updated-at">-</strong>
        </div>
      </div>
    </section>

    <section class="panel">
      <div class="summary-block">
        <span class="section-label">Latest Summary</span>
        <div id="summary-text" class="summary-text">-</div>
      </div>
      <div class="terminal-block">
        <span id="terminal-label" class="section-label">Terminal</span>
        <div id="terminal-meta" class="terminal-meta">Waiting for terminal activity…</div>
        <div class="terminal-stage">
          <div id="terminal-shell" class="terminal-shell is-hidden"></div>
          <div id="terminal-empty-hint" class="terminal-empty-hint is-hidden">No terminal output yet. Codex may still be thinking.</div>
          <pre id="terminal-output" class="output is-hidden">No saved run output is available for the last run.</pre>
        </div>
      </div>
    </section>
  </main>

  <script>
    let terminal = null;
    let fitAddon = null;
    let terminalSocket = null;
    let terminalSocketKey = null;
    let renderedTerminalKey = null;
    let renderedTerminalRecording = null;

    function fmtTime(value) {
      if (!value) return "-";
      return new Date(value).toLocaleString();
    }

    function pillClass(value) {
      const label = String(value || "").toLowerCase();
      if (!label || label === "-" || label === "not loaded") return "pill";
      if (label === "requested review") return "pill warn";
      if (label === "reviewed") return "pill good";
      if (label.includes("fail") || label.includes("block") || label.includes("conflict")) return "pill bad";
      if (label.includes("pause")) return "pill warn";
      if (label.includes("need") || label.includes("pending") || label.includes("comment") || label.includes("retry")) return "pill warn";
      return "pill good";
    }

    function setPill(id, value) {
      document.getElementById(id).innerHTML = `<span class="${pillClass(value)}">${String(value || "-")}</span>`;
    }

    function setPrLink(url, label) {
      const link = document.getElementById("pr-link");
      if (!url || !label) {
        link.classList.add("is-hidden");
        link.removeAttribute("href");
        link.textContent = "PR";
        return;
      }

      link.href = url;
      link.textContent = label;
      link.classList.remove("is-hidden");
    }

    function terminalLabel(pr) {
      if (pr.status === "running") return "Live Terminal";
      if (pr.terminal_recording) return "Saved Terminal";
      return "Last Run Output";
    }

    function detailOutputStatusText(pr) {
      if ((pr.status === "running" || pr.terminal_recording) && pr.last_terminal_output_at) {
        return `Last terminal update: ${fmtTime(pr.last_terminal_output_at)}`;
      }

      if (pr.status !== "running" && pr.details_at) {
        return `Captured from last run at: ${fmtTime(pr.details_at)}`;
      }

      return pr.status === "running"
        ? "No terminal output yet. Codex may still be thinking."
        : "No saved run output is available for the last run.";
    }

    function ensureTerminal() {
      if (terminal) {
        return terminal;
      }

      if (!window.Terminal) {
        return null;
      }

      try {
        terminal = new window.Terminal({
          convertEol: false,
          disableStdin: true,
          cursorBlink: false,
          scrollback: 6000,
          fontFamily: 'Menlo, Monaco, "SF Mono", ui-monospace, monospace',
          fontSize: 13,
          lineHeight: 1.24,
          theme: {
            background: "#1e1e1e",
            foreground: "#cccccc",
            cursor: "#aeafad",
            cursorAccent: "#1e1e1e",
            selectionBackground: "rgba(255, 255, 255, 0.18)",
            black: "#000000",
            red: "#cd3131",
            green: "#0dbc79",
            yellow: "#e5e510",
            blue: "#2472c8",
            magenta: "#bc3fbc",
            cyan: "#11a8cd",
            white: "#e5e5e5",
            brightBlack: "#666666",
            brightRed: "#f14c4c",
            brightGreen: "#23d18b",
            brightYellow: "#f5f543",
            brightBlue: "#3b8eea",
            brightMagenta: "#d670d6",
            brightCyan: "#29b8db",
            brightWhite: "#ffffff"
          }
        });
      } catch (error) {
        console.error("Failed to initialize terminal renderer", error);
        terminal = null;
        return null;
      }

      if (window.FitAddon && window.FitAddon.FitAddon) {
        fitAddon = new window.FitAddon.FitAddon();
        terminal.loadAddon(fitAddon);
      }

      terminal.open(document.getElementById("terminal-shell"));
      fitTerminal();
      window.addEventListener("resize", fitTerminal);
      return terminal;
    }

    function fitTerminal() {
      if (fitAddon) {
        fitAddon.fit();
      }
    }

    function terminalViewport() {
      return document.querySelector("#terminal-shell .xterm-viewport");
    }

    function isViewportNearBottom(viewport) {
      if (!viewport) {
        return true;
      }

      return viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight < 24;
    }

    function preserveTerminalViewport(applyUpdate) {
      const viewport = terminalViewport();
      const previousScrollTop = viewport ? viewport.scrollTop : 0;
      const shouldFollow = isViewportNearBottom(viewport);

      applyUpdate(() => {
        window.requestAnimationFrame(() => {
          const nextViewport = terminalViewport();
          if (!nextViewport) {
            return;
          }

          if (shouldFollow) {
            nextViewport.scrollTop = nextViewport.scrollHeight;
          } else {
            nextViewport.scrollTop = previousScrollTop;
          }
        });
      });
    }

    function renderTerminalRecording(recording, key) {
      const activeTerminal = ensureTerminal();
      if (!activeTerminal) {
        return;
      }

      preserveTerminalViewport((done) => {
        activeTerminal.reset();
        fitTerminal();
        if (recording) {
          activeTerminal.write(recording, done);
        } else {
          done();
        }
      });
      renderedTerminalKey = key || null;
      renderedTerminalRecording = recording || "";
    }

    function appendTerminalChunk(chunk) {
      const activeTerminal = ensureTerminal();
      if (!activeTerminal || !chunk) {
        return;
      }

      preserveTerminalViewport((done) => {
        activeTerminal.write(chunk, done);
      });
      renderedTerminalRecording = (renderedTerminalRecording || "") + chunk;
    }

    function closeTerminalSocket() {
      if (!terminalSocket) {
        return;
      }

      terminalSocket.close();
      terminalSocket = null;
      terminalSocketKey = null;
    }

    function showTextOutput(text) {
      closeTerminalSocket();
      renderedTerminalKey = null;
      renderedTerminalRecording = null;
      document.getElementById("terminal-shell").classList.add("is-hidden");
      document.getElementById("terminal-empty-hint").classList.add("is-hidden");
      const output = document.getElementById("terminal-output");
      output.classList.remove("is-hidden");
      output.textContent = text;
    }

    function showTerminalReplay(pr, shouldReset) {
      const activeTerminal = ensureTerminal();
      if (!activeTerminal) {
        showTextOutput("Terminal renderer failed to load. Reload the page to try again.");
        return false;
      }

      document.getElementById("terminal-shell").classList.remove("is-hidden");
      document.getElementById("terminal-output").classList.add("is-hidden");
      const hint = document.getElementById("terminal-empty-hint");
      if (pr.status === "running" && !pr.terminal_recording) {
        hint.classList.remove("is-hidden");
      } else {
        hint.classList.add("is-hidden");
      }
      if (shouldReset) {
        renderTerminalRecording(pr.terminal_recording || "", pr.key);
      }
      return true;
    }

    function connectTerminalSocket(pr) {
      if (pr.status !== "running") {
        closeTerminalSocket();
        return;
      }

      if (terminalSocket && terminalSocketKey === pr.key) {
        return;
      }

      closeTerminalSocket();
      const protocol = window.location.protocol === "https:" ? "wss" : "ws";
      const socket = new WebSocket(
        `${protocol}://${window.location.host}/api/pr/terminal/ws?key=${encodeURIComponent(pr.key)}`
      );
      terminalSocket = socket;
      terminalSocketKey = pr.key;

      socket.onmessage = (event) => {
        const payload = JSON.parse(event.data);
        if (payload.kind === "reset") {
          renderTerminalRecording(payload.recording || "", pr.key);
        } else if (payload.kind === "chunk") {
          appendTerminalChunk(payload.chunk);
        }

        if (payload.last_output_at) {
          document.getElementById("terminal-meta").textContent =
            `Last terminal update: ${fmtTime(payload.last_output_at)}`;
        }

        const hint = document.getElementById("terminal-empty-hint");
        if ((payload.recording && payload.recording.length > 0) || (payload.chunk && payload.chunk.length > 0)) {
          hint.classList.add("is-hidden");
        }
      };

      socket.onclose = () => {
        if (terminalSocket === socket) {
          terminalSocket = null;
          terminalSocketKey = null;
        }
      };
    }

    function renderDetailOutput(pr) {
      document.getElementById("terminal-label").textContent = terminalLabel(pr);
      document.getElementById("terminal-meta").textContent = detailOutputStatusText(pr);

      if (pr.status === "running" || pr.terminal_recording) {
        const recording = pr.terminal_recording || "";
        const shouldReset =
          renderedTerminalKey !== pr.key ||
          (
            pr.status !== "running" &&
            renderedTerminalRecording !== recording
          ) ||
          (
            pr.status === "running" &&
            !terminalSocket &&
            renderedTerminalRecording !== recording
          );
        if (showTerminalReplay(pr, shouldReset)) {
          connectTerminalSocket(pr);
          return;
        }

        return;
      }

      showTextOutput(pr.detail_output || detailOutputStatusText(pr));
    }

    async function refresh() {
      const key = new URLSearchParams(window.location.search).get("key");
      if (!key) {
        setPrLink(null, null);
        document.getElementById("title").textContent = "Missing PR key";
        document.getElementById("subtitle").textContent = "Open this page from the dashboard so the PR key is included.";
        document.getElementById("terminal-meta").textContent = "-";
        document.getElementById("terminal-label").textContent = "Last Run Output";
        showTextOutput("No PR key was provided.");
        return;
      }

      const response = await fetch(`/api/pr?key=${encodeURIComponent(key)}`);
      if (!response.ok) {
        const message = await response.text();
        throw new Error(message || `HTTP ${response.status}`);
      }

      const pr = await response.json();
      document.title = `${pr.repo_full_name} #${pr.number} · BigBrother`;
      setPrLink(pr.url, `${pr.repo_full_name} #${pr.number}`);
      document.getElementById("title").textContent = pr.title;
      document.getElementById("subtitle").textContent = `Updated ${fmtTime(pr.updated_at)}`;
      document.getElementById("updated-at").textContent = fmtTime(pr.updated_at);
      document.getElementById("summary-text").textContent = pr.latest_summary || "-";
      renderDetailOutput(pr);
      setPill("status-pill", pr.status);
      setPill("ci-pill", pr.ci_status);
      setPill("review-pill", pr.review_status);
    }

    refresh().catch((error) => {
      setPrLink(null, null);
      document.getElementById("title").textContent = "Failed to load run";
      document.getElementById("subtitle").textContent = error.message;
      document.getElementById("terminal-label").textContent = "Last Run Output";
      document.getElementById("terminal-meta").textContent = "-";
      showTextOutput(error.message);
    });
    setInterval(() => refresh().catch(() => {}), 1500);
  </script>
</body>
</html>"##;

#[derive(Debug, Serialize)]
struct HealthResponse {
    ok: bool,
    tracked_prs: usize,
    active_tracked_prs: usize,
    all_prs: usize,
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
struct ReviewRequestsResponse {
    prs: Vec<PullRequestSummary>,
}

#[derive(Debug, Serialize)]
struct ActivityResponse {
    events: Vec<ActivitySummary>,
}

#[derive(Debug, Serialize)]
struct ActivitySummary {
    timestamp: DateTime<Utc>,
    level: String,
    pr_key: Option<String>,
    message: String,
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
    terminal_recording: Option<String>,
    detail_output: Option<String>,
    last_terminal_output_at: Option<DateTime<Utc>>,
    details_label: Option<String>,
    details_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct PrQuery {
    key: String,
}

#[derive(Debug, Deserialize)]
struct PauseRequest {
    key: String,
    paused: bool,
}

#[derive(Debug, Deserialize)]
struct DeepReviewRequest {
    key: String,
}

#[derive(Debug, Deserialize)]
struct RetryRequest {
    key: String,
}

#[derive(Debug, Serialize)]
struct PauseResponse {
    ok: bool,
    pr: PullRequestSummary,
}

#[derive(Debug, Serialize)]
struct DeepReviewResponse {
    ok: bool,
    pr: PullRequestSummary,
}

#[derive(Debug, Serialize)]
struct RetryResponse {
    ok: bool,
    pr: PullRequestSummary,
}

#[derive(Debug, Serialize)]
struct TerminalStreamPayload {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    recording: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chunk: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_output_at: Option<DateTime<Utc>>,
}

pub fn default_listen_addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8787)
}

pub fn router(supervisor: Arc<Supervisor>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/pr", get(pr_detail_page))
        .route(BIGBROTHER_MARK_PATH, get(bigbrother_mark))
        .route(XTERM_CSS_PATH, get(xterm_css))
        .route(XTERM_JS_PATH, get(xterm_js))
        .route(XTERM_ADDON_FIT_JS_PATH, get(xterm_addon_fit_js))
        .route("/api/health", get(health))
        .route("/api/activity", get(activity))
        .route("/api/prs", get(list_prs))
        .route("/api/review-requests", get(list_review_requests))
        .route("/api/pr", get(get_pr))
        .route("/api/pr/terminal/ws", get(pr_terminal_ws))
        .route("/api/prs/pause", post(set_pr_paused))
        .route("/api/prs/retry", post(trigger_failed_retry))
        .route(
            "/api/review-requests/deep-review",
            post(trigger_deep_review),
        )
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

    println!("BigBrother listening on http://{listen_addr}");

    axum::serve(listener, router(supervisor))
        .with_graceful_shutdown(shutdown_signal(stop_flag))
        .await
        .context("HTTP server exited unexpectedly")
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn pr_detail_page() -> Html<&'static str> {
    Html(PR_DETAIL_HTML)
}

async fn bigbrother_mark() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], BIGBROTHER_MARK_PNG)
}

async fn xterm_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        XTERM_CSS,
    )
}

async fn xterm_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        XTERM_JS,
    )
}

async fn xterm_addon_fit_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        XTERM_ADDON_FIT_JS,
    )
}

async fn health(State(supervisor): State<Arc<Supervisor>>) -> Json<HealthResponse> {
    let snapshot = supervisor.snapshot();
    let tracked_prs = snapshot.tracked_prs.len();
    let active_tracked_prs = snapshot
        .tracked_prs
        .values()
        .filter(|pr| !pr.persisted.paused)
        .count();
    let all_prs = snapshot
        .total_matching_prs
        .unwrap_or(tracked_prs)
        .max(tracked_prs);
    let running_prs = snapshot
        .tracked_prs
        .values()
        .filter(|pr| pr.status == crate::model::TrackingStatus::Running)
        .count()
        + snapshot
            .review_requests
            .values()
            .filter(|pr| pr.runner.is_some())
            .count();

    Json(HealthResponse {
        ok: snapshot.last_poll_error.is_none(),
        tracked_prs,
        active_tracked_prs,
        all_prs,
        running_prs,
        last_poll_started_at: snapshot.last_poll_started_at,
        last_poll_finished_at: snapshot.last_poll_finished_at,
        next_poll_due_at: snapshot.next_poll_due_at,
        last_poll_error: snapshot.last_poll_error,
    })
}

async fn activity(State(supervisor): State<Arc<Supervisor>>) -> Json<ActivityResponse> {
    let snapshot = supervisor.snapshot();
    Json(ActivityResponse {
        events: snapshot.activity.iter().map(summarize_activity).collect(),
    })
}

async fn list_prs(State(supervisor): State<Arc<Supervisor>>) -> Json<PullRequestsResponse> {
    let snapshot = supervisor.snapshot();
    let mut prs = snapshot
        .tracked_prs
        .values()
        .map(summarize_pr)
        .map(strip_detail_payload)
        .collect::<Vec<_>>();

    prs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));

    Json(PullRequestsResponse { prs })
}

async fn list_review_requests(
    State(supervisor): State<Arc<Supervisor>>,
) -> Json<ReviewRequestsResponse> {
    let snapshot = supervisor.snapshot();
    let mut prs = snapshot
        .review_requests
        .values()
        .map(summarize_review_request)
        .map(strip_detail_payload)
        .collect::<Vec<_>>();

    prs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));

    Json(ReviewRequestsResponse { prs })
}

async fn get_pr(
    State(supervisor): State<Arc<Supervisor>>,
    Query(query): Query<PrQuery>,
) -> Result<Json<PullRequestSummary>, (StatusCode, String)> {
    let snapshot = supervisor.snapshot();
    if let Some(tracked) = snapshot.tracked_prs.get(&query.key) {
        return Ok(Json(summarize_pr(tracked)));
    }
    let Some(review_request) = snapshot.review_requests.get(&query.key) else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("unknown PR key: {}", query.key),
        ));
    };

    Ok(Json(summarize_review_request(review_request)))
}

async fn pr_terminal_ws(
    State(supervisor): State<Arc<Supervisor>>,
    Query(query): Query<PrQuery>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let Some(subscription) = supervisor.subscribe_terminal(&query.key).await else {
        return Err((
            StatusCode::CONFLICT,
            format!("PR is not currently running: {}", query.key),
        ));
    };

    Ok(ws.on_upgrade(move |socket| stream_terminal(socket, subscription)))
}

async fn stream_terminal(mut socket: WebSocket, mut subscription: TerminalSubscription) {
    let reset = TerminalStreamPayload {
        kind: "reset",
        recording: subscription.initial_recording.take(),
        chunk: None,
        last_output_at: subscription.last_output_at,
    };
    if send_terminal_payload(&mut socket, &reset).await.is_err() {
        return;
    }

    while let Ok(chunk) = subscription.receiver.recv().await {
        let payload = TerminalStreamPayload {
            kind: "chunk",
            recording: None,
            chunk: Some(chunk.chunk),
            last_output_at: Some(chunk.last_output_at),
        };
        if send_terminal_payload(&mut socket, &payload).await.is_err() {
            break;
        }
    }
}

async fn send_terminal_payload(
    socket: &mut WebSocket,
    payload: &TerminalStreamPayload,
) -> Result<()> {
    let body = serde_json::to_string(payload).context("failed to serialize terminal payload")?;
    socket
        .send(Message::Text(body.into()))
        .await
        .context("failed sending terminal websocket message")
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

async fn trigger_deep_review(
    State(supervisor): State<Arc<Supervisor>>,
    Json(request): Json<DeepReviewRequest>,
) -> Result<Json<DeepReviewResponse>, (StatusCode, String)> {
    let updated = supervisor
        .trigger_deep_review(&request.key)
        .await
        .map_err(|error| {
            (
                StatusCode::CONFLICT,
                format!("failed starting deep review: {error:#}"),
            )
        })?;

    let Some(review_request) = updated else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("unknown review request PR key: {}", request.key),
        ));
    };

    Ok(Json(DeepReviewResponse {
        ok: true,
        pr: summarize_review_request(&review_request),
    }))
}

async fn trigger_failed_retry(
    State(supervisor): State<Arc<Supervisor>>,
    Json(request): Json<RetryRequest>,
) -> Result<Json<RetryResponse>, (StatusCode, String)> {
    let updated = supervisor
        .trigger_failed_retry(&request.key)
        .await
        .map_err(|error| {
            (
                StatusCode::CONFLICT,
                format!("failed triggering retry: {error:#}"),
            )
        })?;

    let Some(tracked) = updated else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("unknown PR key: {}", request.key),
        ));
    };

    Ok(Json(RetryResponse {
        ok: true,
        pr: summarize_pr(&tracked),
    }))
}

fn summarize_pr(tracked: &TrackedPr) -> PullRequestSummary {
    let (details_label, details_at) = if let Some(runner) = tracked.runner.as_ref() {
        (Some("Started".to_owned()), Some(runner.started_at))
    } else if let Some(finished_at) = tracked.persisted.last_run_finished_at {
        (Some("Last run".to_owned()), Some(finished_at))
    } else if let Some(started_at) = tracked.persisted.last_run_started_at {
        (Some("Last run".to_owned()), Some(started_at))
    } else {
        (None, None)
    };

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
        latest_summary: latest_operator_summary(tracked),
        terminal_recording: latest_terminal_recording(tracked),
        detail_output: latest_detail_output(tracked),
        last_terminal_output_at: tracked
            .runner
            .as_ref()
            .and_then(|runner| runner.last_terminal_output_at)
            .or_else(|| {
                if tracked.runner.is_none() {
                    tracked.persisted.last_terminal_output_at
                } else {
                    None
                }
            }),
        details_label,
        details_at,
    }
}

fn strip_detail_payload(mut summary: PullRequestSummary) -> PullRequestSummary {
    summary.terminal_recording = None;
    summary.detail_output = None;
    summary.last_terminal_output_at = None;
    summary
}

fn summarize_review_request(review_request: &ReviewRequestPr) -> PullRequestSummary {
    let (details_label, details_at) = review_request_detail_timestamps(review_request);

    PullRequestSummary {
        key: review_request.pull_request.key.clone(),
        repo_full_name: review_request.pull_request.repo_full_name.clone(),
        number: review_request.pull_request.number,
        title: review_request.pull_request.title.clone(),
        url: review_request.pull_request.url.clone(),
        status: review_request_status(review_request).to_owned(),
        ci_status: "-".to_owned(),
        review_status: "-".to_owned(),
        is_paused: false,
        can_toggle_pause: false,
        attention_reason: Some("requested reviewer".to_owned()),
        updated_at: review_request.pull_request.updated_at,
        latest_summary: latest_review_request_summary(review_request),
        terminal_recording: latest_review_request_terminal_recording(review_request),
        detail_output: latest_review_request_output(review_request),
        last_terminal_output_at: review_request
            .runner
            .as_ref()
            .and_then(|runner| runner.last_terminal_output_at)
            .or_else(|| {
                if review_request.runner.is_none()
                    && review_request.persisted.last_run_trigger
                        == Some(AttentionReason::DeepReview)
                {
                    review_request.persisted.last_terminal_output_at
                } else {
                    None
                }
            }),
        details_label,
        details_at,
    }
}

fn latest_detail_output(tracked: &TrackedPr) -> Option<String> {
    tracked
        .runner
        .as_ref()
        .and_then(|runner| detail_text_output(runner.live_output.as_deref()))
        .or_else(|| {
            if tracked.runner.is_none() {
                detail_text_output(tracked.persisted.last_run_output.as_deref())
            } else {
                None
            }
        })
}

fn latest_terminal_recording(tracked: &TrackedPr) -> Option<String> {
    tracked
        .runner
        .as_ref()
        .and_then(|runner| runner.live_terminal.clone())
        .or_else(|| {
            if tracked.runner.is_none() {
                tracked.persisted.last_run_terminal.clone()
            } else {
                None
            }
        })
}

fn latest_operator_summary(tracked: &TrackedPr) -> Option<String> {
    tracked
        .runner
        .as_ref()
        .map(|runner| runner.summary.clone())
        .or_else(|| summarize_persisted_run(&tracked.persisted))
}

fn latest_review_request_summary(review_request: &ReviewRequestPr) -> Option<String> {
    review_request
        .runner
        .as_ref()
        .filter(|runner| runner.trigger == AttentionReason::DeepReview)
        .map(|runner| runner.summary.clone())
        .or_else(|| summarize_deep_review_persisted_run(&review_request.persisted))
}

fn latest_review_request_output(review_request: &ReviewRequestPr) -> Option<String> {
    review_request
        .runner
        .as_ref()
        .filter(|runner| runner.trigger == AttentionReason::DeepReview)
        .and_then(|runner| detail_text_output(runner.live_output.as_deref()))
        .or_else(|| {
            if review_request.runner.is_none()
                && review_request.persisted.last_run_trigger == Some(AttentionReason::DeepReview)
            {
                detail_text_output(review_request.persisted.last_run_output.as_deref())
            } else {
                None
            }
        })
}

fn latest_review_request_terminal_recording(review_request: &ReviewRequestPr) -> Option<String> {
    review_request
        .runner
        .as_ref()
        .filter(|runner| runner.trigger == AttentionReason::DeepReview)
        .and_then(|runner| runner.live_terminal.clone())
}

fn summarize_deep_review_persisted_run(persisted: &PersistentPrState) -> Option<String> {
    if persisted.last_run_trigger != Some(AttentionReason::DeepReview) {
        return None;
    }

    summarize_persisted_run(persisted)
}

fn review_request_detail_timestamps(
    review_request: &ReviewRequestPr,
) -> (Option<String>, Option<DateTime<Utc>>) {
    if let Some(runner) = review_request.runner.as_ref() {
        (Some("Started".to_owned()), Some(runner.started_at))
    } else if review_request.persisted.last_run_trigger == Some(AttentionReason::DeepReview) {
        if let Some(finished_at) = review_request.persisted.last_run_finished_at {
            (Some("Last run".to_owned()), Some(finished_at))
        } else if let Some(started_at) = review_request.persisted.last_run_started_at {
            (Some("Last run".to_owned()), Some(started_at))
        } else {
            (None, None)
        }
    } else {
        (None, None)
    }
}

fn review_request_status(review_request: &ReviewRequestPr) -> &'static str {
    if review_request
        .runner
        .as_ref()
        .is_some_and(|runner| runner.trigger == AttentionReason::DeepReview)
    {
        "running"
    } else if review_request.persisted.last_run_trigger == Some(AttentionReason::DeepReview) {
        match review_request.persisted.last_run_status.as_deref() {
            Some("success") => "reviewed",
            Some("error") => "review failed",
            _ => "requested review",
        }
    } else {
        "requested review"
    }
}

fn summarize_persisted_run(persisted: &PersistentPrState) -> Option<String> {
    match (
        persisted.last_run_trigger,
        persisted.last_run_status.as_deref(),
    ) {
        (_, Some("needs_decision")) => Some(NEEDS_DECISION_SUMMARY.to_owned()),
        (Some(trigger), Some("success")) => Some(trigger.success_summary().to_owned()),
        (Some(trigger), Some("error")) => Some(trigger.failure_summary().to_owned()),
        _ => compact_summary_text(persisted.last_run_summary.as_deref()),
    }
}

fn compact_summary_text(summary: Option<&str>) -> Option<String> {
    let line = summary?.lines().map(str::trim).find(|line| {
        !line.is_empty()
            && *line != "=== Prompt Sent To Codex CLI ==="
            && *line != "=== Codex CLI Output ==="
    })?;

    let truncated = if line.chars().count() > 120 {
        let mut shortened = line.chars().take(117).collect::<String>();
        shortened.push_str("...");
        shortened
    } else {
        line.to_owned()
    };
    Some(truncated)
}

fn detail_text_output(output: Option<&str>) -> Option<String> {
    let output = output?;
    let output = output
        .find(OUTPUT_TRANSCRIPT_HEADER)
        .map(|offset| &output[offset + OUTPUT_TRANSCRIPT_HEADER.len()..])
        .unwrap_or(output)
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed != "=== Prompt Sent To Codex CLI ===" && trimmed != "=== Codex CLI Output ==="
        })
        .collect::<Vec<_>>()
        .join("\n");
    let output = output.trim();

    if output.is_empty() {
        None
    } else {
        Some(output.to_owned())
    }
}

fn summarize_activity(event: &ActivityEvent) -> ActivitySummary {
    ActivitySummary {
        timestamp: event.timestamp,
        level: event.level.label().to_owned(),
        pr_key: event.pr_key.clone(),
        message: event.message.clone(),
    }
}

async fn shutdown_signal(stop_flag: Arc<AtomicBool>) {
    let _ = tokio::signal::ctrl_c().await;
    stop_flag.store(true, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::{compact_summary_text, detail_text_output, latest_operator_summary};
    use crate::model::{
        AttentionReason, CiStatus, PersistentPrState, PullRequest, ReviewDecision, RunnerState,
        TrackedPr, TrackingStatus, NEEDS_DECISION_SUMMARY,
    };

    fn sample_pr() -> PullRequest {
        PullRequest {
            key: "openai/bigbrother#7".to_owned(),
            repo_full_name: "openai/bigbrother".to_owned(),
            number: 7,
            title: "Test".to_owned(),
            body: None,
            url: "https://github.com/openai/bigbrother/pull/7".to_owned(),
            author_login: "connor".to_owned(),
            labels: vec![],
            created_at: chrono::Utc.with_ymd_and_hms(2026, 3, 31, 18, 0, 0).unwrap(),
            updated_at: chrono::Utc
                .with_ymd_and_hms(2026, 3, 31, 18, 30, 0)
                .unwrap(),
            head_sha: "abc123".to_owned(),
            head_ref: "feature/test".to_owned(),
            base_sha: "def456".to_owned(),
            base_ref: "main".to_owned(),
            clone_url: "https://github.com/openai/bigbrother.git".to_owned(),
            ssh_url: "git@github.com:openai/bigbrother.git".to_owned(),
            ci_status: CiStatus::Failure,
            ci_updated_at: Some(
                chrono::Utc
                    .with_ymd_and_hms(2026, 3, 31, 18, 25, 0)
                    .unwrap(),
            ),
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

    #[test]
    fn latest_operator_summary_prefers_trigger_specific_short_text() {
        let tracked = TrackedPr {
            pull_request: sample_pr(),
            status: TrackingStatus::NeedsAttention,
            attention_reason: Some(AttentionReason::ReviewFeedback),
            persisted: PersistentPrState {
                last_run_status: Some("error".to_owned()),
                last_run_summary: Some("huge multiline\nsummary body".to_owned()),
                last_run_trigger: Some(AttentionReason::ReviewFeedback),
                ..PersistentPrState::default()
            },
            runner: None,
        };

        assert_eq!(
            latest_operator_summary(&tracked).as_deref(),
            Some("review feedback handling failed")
        );
    }

    #[test]
    fn running_summary_stays_short_and_trigger_aware() {
        let tracked = TrackedPr {
            pull_request: sample_pr(),
            status: TrackingStatus::Running,
            attention_reason: Some(AttentionReason::MergeConflict),
            persisted: PersistentPrState::default(),
            runner: Some(RunnerState {
                status: TrackingStatus::Running,
                started_at: chrono::Utc.with_ymd_and_hms(2026, 3, 31, 18, 0, 0).unwrap(),
                finished_at: None,
                attempt: 1,
                trigger: AttentionReason::MergeConflict,
                summary: AttentionReason::MergeConflict.active_summary().to_owned(),
                live_output: None,
                live_terminal: None,
                last_terminal_output_at: None,
                exit_code: None,
            }),
        };

        assert_eq!(
            latest_operator_summary(&tracked).as_deref(),
            Some("resolving merge conflict")
        );
    }

    #[test]
    fn compact_summary_text_trims_legacy_multiline_blobs() {
        assert_eq!(
            compact_summary_text(Some(
                "\n=== Prompt Sent To Codex CLI ===\n\nfatal: auth expired\nsecond line"
            ))
            .as_deref(),
            Some("fatal: auth expired")
        );
    }

    #[test]
    fn detail_text_output_strips_transcript_headers() {
        assert_eq!(
            detail_text_output(Some(
                "=== Prompt Sent To Codex CLI ===\nprompt body\n=== Codex CLI Output ===\n$ cargo test\nok\n"
            ))
            .as_deref(),
            Some("$ cargo test\nok")
        );
    }

    #[test]
    fn detail_text_output_strips_header_only_fallbacks() {
        assert_eq!(
            detail_text_output(Some(
                "=== Prompt Sent To Codex CLI ===\n\nfatal: auth expired\nsecond line\n"
            ))
            .as_deref(),
            Some("fatal: auth expired\nsecond line")
        );
    }

    #[test]
    fn latest_operator_summary_uses_needs_decision_short_text() {
        let tracked = TrackedPr {
            pull_request: sample_pr(),
            status: TrackingStatus::NeedsDecision,
            attention_reason: Some(AttentionReason::CiFailed),
            persisted: PersistentPrState {
                paused: true,
                needs_decision_reason: Some("requires API decision".to_owned()),
                last_run_status: Some("needs_decision".to_owned()),
                last_run_summary: Some("long operator-facing explanation".to_owned()),
                last_run_trigger: Some(AttentionReason::CiFailed),
                ..PersistentPrState::default()
            },
            runner: None,
        };

        assert_eq!(
            latest_operator_summary(&tracked).as_deref(),
            Some(NEEDS_DECISION_SUMMARY)
        );
    }
}
