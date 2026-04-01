use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{Context, Result};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Html,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::{
    model::{ActivityEvent, TrackedPr},
    service::Supervisor,
};

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
          <h1>Symphony RS</h1>
          <p>Tracking authored GitHub pull requests, surfacing CI and review changes, and showing when the local agent has already taken a pass.</p>
        </div>
        <div class="view-tabs" role="tablist" aria-label="Dashboard views">
          <button id="tab-prs" class="view-tab active" type="button" data-view="prs" aria-selected="true">PRs</button>
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
    const optimisticPausedStates = new Map();
    const dashboardViewStorageKey = "symphony-rs.dashboard-view";
    let latestPrs = [];
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
      if (label.includes("fail") || label.includes("block") || label.includes("conflict")) return "pill bad";
      if (label.includes("pause")) return "pill warn";
      if (label.includes("need") || label.includes("pending") || label.includes("comment") || label.includes("retry")) return "pill warn";
      return "pill good";
    }

    function effectivePaused(pr) {
      return optimisticPausedStates.has(pr.key) ? optimisticPausedStates.get(pr.key) : pr.is_paused;
    }

    function rowClass(pr) {
      const classes = ["pr-row"];
      if (effectivePaused(pr)) classes.push("paused-row");
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
      if (!pr.can_toggle_pause) return "-";

      const pending = pendingPauseKeys.has(pr.key);
      const isPaused = effectivePaused(pr);
      const nextPaused = !isPaused;
      const label = pending ? "Updating..." : (isPaused ? "Resume" : "Pause");
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

    function renderPrs(prs) {
      const tbody = document.getElementById("prs-table");
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

    function setDashboardView(view, persist = true) {
      currentView = view === "activity" ? "activity" : "prs";
      document.getElementById("view-prs").classList.toggle("is-hidden", currentView !== "prs");
      document.getElementById("view-activity").classList.toggle("is-hidden", currentView !== "activity");

      const prTab = document.getElementById("tab-prs");
      const activityTab = document.getElementById("tab-activity");
      prTab.classList.toggle("active", currentView === "prs");
      activityTab.classList.toggle("active", currentView === "activity");
      prTab.setAttribute("aria-selected", String(currentView === "prs"));
      activityTab.setAttribute("aria-selected", String(currentView === "activity"));
      prTab.setAttribute("tabindex", currentView === "prs" ? "0" : "-1");
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
      const [healthRes, prsRes, activityRes] = await Promise.all([
        fetch("/api/health"),
        fetch("/api/prs"),
        fetch("/api/activity")
      ]);

      const health = await healthRes.json();
      const prsPayload = await prsRes.json();
      const activityPayload = await activityRes.json();

      document.getElementById("health-status").textContent = health.ok ? "Healthy" : "Attention needed";
      document.getElementById("health-count").textContent = String(health.tracked_prs);
      document.getElementById("health-running").textContent = String(health.running_prs);
      document.getElementById("health-poll").textContent = fmtTime(health.last_poll_finished_at);
      latestPrs = prsPayload.prs || [];
      renderPrs(latestPrs);
      renderActivity(activityPayload.events || []);
    }

    setDashboardView(restoreDashboardView(), false);
    document.getElementById("tab-prs").addEventListener("click", () => setDashboardView("prs"));
    document.getElementById("tab-activity").addEventListener("click", () => setDashboardView("activity"));

    refresh().catch((error) => {
      document.getElementById("prs-table").innerHTML =
        `<tr><td colspan="7" class="empty">Failed to load dashboard: ${error.message}</td></tr>`;
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
  <title>Symphony RS Run View</title>
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

    .meta {
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
      gap: 14px;
      margin-top: 20px;
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

    .back-link,
    .pr-link {
      color: var(--accent);
      text-decoration: none;
      border-bottom: 1px solid rgba(29, 107, 87, 0.25);
      width: fit-content;
    }

    .back-link:hover,
    .pr-link:hover {
      border-bottom-color: rgba(29, 107, 87, 0.8);
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
      font: 0.84rem/1.5 "SFMono-Regular", "SF Mono", ui-monospace, monospace;
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
      <a class="back-link" href="/">Back to dashboard</a>
      <h1 id="title" style="margin: 12px 0 6px; font-size: clamp(2rem, 3vw, 3rem); letter-spacing: -0.04em;">Loading run…</h1>
      <div id="subtitle" style="color: var(--muted); line-height: 1.55;">Fetching PR details…</div>
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
      <div class="toolbar">
        <a id="pr-link" class="pr-link" href="#" target="_blank" rel="noreferrer">Open GitHub PR</a>
        <span id="attention-text" class="empty">Attention: -</span>
      </div>
      <div class="summary-block">
        <span class="section-label">Latest Summary</span>
        <div id="summary-text" class="summary-text">-</div>
      </div>
      <div>
        <span id="output-label" class="section-label">Codex CLI Output</span>
        <pre id="output" class="output">Waiting for output…</pre>
      </div>
    </section>
  </main>

  <script>
    function fmtTime(value) {
      if (!value) return "-";
      return new Date(value).toLocaleString();
    }

    function pillClass(value) {
      const label = String(value || "").toLowerCase();
      if (label.includes("fail") || label.includes("block") || label.includes("conflict")) return "pill bad";
      if (label.includes("pause")) return "pill warn";
      if (label.includes("need") || label.includes("pending") || label.includes("comment") || label.includes("retry")) return "pill warn";
      return "pill good";
    }

    function setPill(id, value) {
      document.getElementById(id).innerHTML = `<span class="${pillClass(value)}">${String(value || "-")}</span>`;
    }

    async function refresh() {
      const key = new URLSearchParams(window.location.search).get("key");
      if (!key) {
        document.getElementById("title").textContent = "Missing PR key";
        document.getElementById("subtitle").textContent = "Open this page from the dashboard so the PR key is included.";
        document.getElementById("output").textContent = "No PR key was provided.";
        return;
      }

      const response = await fetch(`/api/pr?key=${encodeURIComponent(key)}`);
      if (!response.ok) {
        const message = await response.text();
        throw new Error(message || `HTTP ${response.status}`);
      }

      const pr = await response.json();
      document.title = `${pr.repo_full_name} #${pr.number} · Symphony RS`;
      document.getElementById("title").textContent = `${pr.repo_full_name} #${pr.number}`;
      document.getElementById("subtitle").textContent = pr.title;
      document.getElementById("pr-link").href = pr.url;
      document.getElementById("attention-text").textContent = `Attention: ${pr.attention_reason || "-"}`;
      document.getElementById("updated-at").textContent = fmtTime(pr.updated_at);
      document.getElementById("summary-text").textContent = pr.latest_summary || "-";
      document.getElementById("output-label").textContent =
        pr.status === "running" ? "Live Codex CLI Output" : "Saved Codex CLI Output";
      document.getElementById("output").textContent = pr.live_output || (
        pr.status === "running"
          ? "No live output is available for this PR right now."
          : "No saved output is available for the last run."
      );
      setPill("status-pill", pr.status);
      setPill("ci-pill", pr.ci_status);
      setPill("review-pill", pr.review_status);
    }

    refresh().catch((error) => {
      document.getElementById("title").textContent = "Failed to load run";
      document.getElementById("subtitle").textContent = error.message;
      document.getElementById("output").textContent = error.message;
    });
    setInterval(() => refresh().catch(() => {}), 1500);
  </script>
</body>
</html>"##;

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
    live_output: Option<String>,
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
        .route("/pr", get(pr_detail_page))
        .route("/api/health", get(health))
        .route("/api/activity", get(activity))
        .route("/api/prs", get(list_prs))
        .route("/api/pr", get(get_pr))
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

async fn pr_detail_page() -> Html<&'static str> {
    Html(PR_DETAIL_HTML)
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
        .collect::<Vec<_>>();

    prs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));

    Json(PullRequestsResponse { prs })
}

async fn get_pr(
    State(supervisor): State<Arc<Supervisor>>,
    Query(query): Query<PrQuery>,
) -> Result<Json<PullRequestSummary>, (StatusCode, String)> {
    let snapshot = supervisor.snapshot();
    let Some(tracked) = snapshot.tracked_prs.get(&query.key) else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("unknown PR key: {}", query.key),
        ));
    };

    Ok(Json(summarize_pr(tracked)))
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
        latest_summary: tracked
            .runner
            .as_ref()
            .map(|runner| runner.summary.clone())
            .or_else(|| tracked.persisted.last_run_summary.clone()),
        live_output: tracked
            .runner
            .as_ref()
            .map(|runner| runner.live_output.clone())
            .unwrap_or_else(|| tracked.persisted.last_run_output.clone()),
        details_label,
        details_at,
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
