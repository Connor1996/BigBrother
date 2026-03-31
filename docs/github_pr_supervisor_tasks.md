# GitHub PR Supervisor Task List

Status: Working plan

This file is the catch-up checklist for continuing the Rust implementation after the architecture pivot away from native GUI.

## 1. MVP Now

This section is the only task list that matters for the next implementation pass.

### 1.1 MVP Definition

- [ ] Freeze MVP scope as:
  - one Rust process
  - one local HTTP server
  - one minimal web page
  - in-memory daemon state
  - fake-provider-backed simulated integration test
- [ ] Explicitly defer SQLite, notifications, SSE, Tauri, and full frontend scaffolding

### 1.2 Backend Skeleton

- [ ] Introduce `axum` and start a local HTTP server from the Rust binary
- [ ] Add a shared app state that holds:
  - tracked PRs
  - last poll timestamps
  - latest run summaries
  - active run markers
- [ ] Add graceful startup and shutdown

### 1.3 Minimal Domain Abstractions

- [ ] Define a `GitHubProvider` trait
- [ ] Define an `AgentRunner` trait
- [ ] Keep the PR state model intentionally small for MVP:
  - PR identity
  - CI status
  - review status
  - attention reason
  - tracking status
  - latest summary

### 1.4 Poll And Decision Loop

- [ ] Implement a single poll cycle that fetches PRs from the provider
- [ ] Derive actionable status for each PR
- [ ] Trigger a runner for one actionable PR when policy allows
- [ ] Prevent repeated reruns for the same unchanged simulated signal

### 1.5 Minimal API

- [ ] Implement `GET /api/health`
- [ ] Implement `GET /api/prs`
- [ ] Implement one trigger endpoint if needed for local testing

### 1.6 Minimal Web UI

- [ ] Serve a single static HTML page from the backend
- [ ] Render a table of tracked PRs using fetch against `/api/prs`
- [ ] Show:
  - PR
  - title
  - status
  - CI
  - reviews
  - latest summary
- [ ] Keep the page intentionally plain; no frontend framework is required for MVP

### 1.7 Simulated Integration Test

- [ ] Add one integration-style test that runs with `cargo test`
- [ ] Use a fake GitHub provider fixture containing at least:
  - one idle PR
  - one actionable PR with failing CI or new review feedback
- [ ] Use a fake agent runner that records invocations and returns a deterministic success result
- [ ] Start the app in-process with the fake provider and fake runner
- [ ] Execute one poll cycle
- [ ] Assert through the API or shared state that:
  - both PRs are visible
  - the actionable PR transitions to run state and then to post-run state
  - the fake runner is invoked exactly once
  - the same unchanged signal does not trigger a second run in the same scenario

### 1.8 MVP Exit Criteria

- [ ] `cargo test` passes with the simulated integration test included
- [ ] running the app locally exposes a page in the browser
- [ ] that page shows live data from the backend for the fake or real provider path
- [ ] the codebase is organized clearly enough that later work can layer SQLite and richer UI on top

## 2. Background Decisions

- [x] Decide that the long-term UI is web-based, not a native Rust widget app
- [x] Treat current `egui` UI as a spike, not the final product surface
- [x] Write the architecture spec in `docs/github_pr_supervisor_spec.md`

## 3. Repo Organization

- [ ] Create a stable directory layout for `backend/` and `frontend/` under the repo root
- [ ] Decide whether existing single-crate Rust code should remain one crate or split into workspace members
- [ ] Move or annotate exploratory spike files so future work does not accidentally keep building the wrong UI path
- [ ] Add a top-level roadmap note in `README.md` pointing to the spec and task list as the source of truth

## 4. Backend Foundation

- [ ] Introduce `axum` and a local HTTP server skeleton
- [ ] Add structured config loading for backend and frontend serving
- [ ] Add lifecycle management for daemon + API server in one process
- [ ] Define shared app state that can power both daemon logic and HTTP responses
- [ ] Add graceful shutdown handling

## 5. Persistence

- [ ] Add SQLite schema and migrations
- [ ] Replace JSON-file persistence with SQLite-backed durable state
- [ ] Store tracked PR state, run history, and notification history
- [ ] Add migration or one-time import path from the current JSON spike if worth preserving
- [ ] Add tests for restart persistence and processed-marker correctness

## 6. GitHub Integration

- [ ] Keep authored PR discovery via GitHub Search API
- [ ] Normalize PR detail, reviews, inline comments, top-level comments, checks, and commit status
- [ ] Make API pagination explicit and tested
- [ ] Add rate-limit awareness and backoff behavior
- [ ] Add clear error classification for auth failure, transport failure, and malformed payloads

## 7. Decision Engine

- [ ] Centralize `attention_reason` calculation into a dedicated policy module
- [ ] Prevent repeat triggers on unchanged CI failure or already-processed comments
- [ ] Add configurable policy flags for which review states should trigger auto-runs
- [ ] Add per-PR pause or mute support
- [ ] Add retry and cooldown rules for repeated failing runs

## 8. Workspace Manager

- [ ] Replace ad hoc workspace naming with the spec’s per-PR workspace key
- [ ] Validate workspace paths stay under configured root
- [ ] Support SSH and HTTPS Git transports cleanly
- [ ] Improve workspace sync logic for clone, fetch, checkout, reset, and branch divergence cases
- [ ] Add tests for safe path handling and branch sync behavior

## 9. Agent Runner

- [ ] Extract agent execution into a backend service module
- [ ] Add run timeouts and cancellation
- [ ] Capture stdout, stderr, exit code, duration, and compact summary
- [ ] Persist run records into SQLite
- [ ] Add backpressure so one PR cannot launch overlapping runs
- [ ] Make the prompt builder explicit and versioned

## 10. Notifications

- [ ] Define a notification trait and sink abstraction
- [ ] Implement a local desktop notification sink first
- [ ] Add webhook sink support
- [ ] Emit notifications for blocked runs, repeated failures, and required human decisions
- [ ] Persist notification records for UI display

## 11. API Layer

- [ ] Implement `GET /api/health`
- [ ] Implement `GET /api/state`
- [ ] Implement `GET /api/prs`
- [ ] Implement `GET /api/prs/:repo/:number`
- [ ] Implement `GET /api/prs/:repo/:number/events`
- [ ] Implement `GET /api/prs/:repo/:number/runs`
- [ ] Implement `POST /api/prs/:repo/:number/recheck`
- [ ] Implement `POST /api/prs/:repo/:number/run-now`
- [ ] Implement `POST /api/prs/:repo/:number/pause`
- [ ] Implement `POST /api/prs/:repo/:number/resume`
- [ ] Implement SSE stream for live UI updates

## 12. Web UI Foundation

- [ ] Scaffold `frontend/` with Vite + React + TypeScript
- [ ] Set up local development proxy or static serving strategy
- [ ] Build the global app shell with list pane, detail pane, and activity feed
- [ ] Add typed API client layer
- [ ] Add SSE subscription handling

## 13. Web UI Screens

- [ ] Implement PR list table
- [ ] Implement PR detail panel
- [ ] Implement run history view
- [ ] Implement recent activity feed
- [ ] Implement daemon status header
- [ ] Implement manual actions for recheck, run-now, pause, and resume

## 14. Packaging

- [ ] Decide whether to ship browser-only first or add Tauri in the same milestone
- [ ] If Tauri is chosen, keep it as a thin shell around the existing web app
- [ ] Ensure packaging does not duplicate backend logic

## 15. Testing

- [ ] Add backend unit tests for trigger detection and state transitions
- [ ] Add backend integration tests for GitHub payload normalization
- [ ] Add tests for workspace safety and runner concurrency limits
- [ ] Add API tests for the JSON routes
- [ ] Add frontend smoke tests for the main dashboard flows

## 16. Cleanup Of The Current Spike

- [ ] Decide which modules from the current Rust spike are worth preserving
- [ ] Remove or archive the `egui` UI path once the web UI reaches parity
- [ ] Update `README.md` so it no longer presents the native UI path as the target architecture

## 17. Later Milestones

These are intentionally deferred until after the MVP above lands.

- [ ] Replace static page with a richer frontend app if needed
- [ ] Add SQLite persistence
- [ ] Add notifications
- [ ] Add SSE or WebSocket streaming
- [ ] Add run history UI
- [ ] Add Tauri packaging

## 18. Risks To Watch

- [ ] GitHub API rate-limit handling
- [ ] false positives on comment-triggered reruns
- [ ] unsafe push behavior to the wrong branch or remote
- [ ] UI drift from backend truth if live event semantics are sloppy
- [ ] letting the spike architecture leak into the final product
