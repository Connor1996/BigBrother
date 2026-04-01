# GitHub PR Supervisor Spec

Status: Draft v0.2

Owner: `symphony-rs` implementation track

Repository note:

- this specification applies to the standalone `symphony-rs` project under `~/Coding/symphony-rs`
- paths in this document are relative to that repository root unless stated otherwise

## 1. Goal

Build a new Rust-based Symphony implementation that:

1. tracks GitHub pull requests authored by the configured user
2. monitors CI, reviews, inline comments, and top-level PR comments
3. autonomously runs an agent to address actionable feedback when it is safe to do so
4. notifies the user when the system is blocked or needs a human decision
5. provides a non-native GUI so the user can inspect tracked PRs and daemon activity at a glance
6. evolves from a review-only supervisor into a lifecycle workspace with dedicated review, develop,
   and merge surfaces

This spec intentionally replaces the earlier `egui/eframe`-style UI direction. The target UI is a web application, not a native widget tree.

## 2. Why Not Native GUI

The target experience should feel closer to modern agent control surfaces:

- rich lists, timelines, diffs, logs, and detail panes
- easy iteration on layout without platform-specific UI work
- straightforward remote/local access via browser
- optional desktop packaging later without changing the core UI

The product decision is:

- primary UI: local web app
- optional packaging: Tauri shell around the same web app
- non-goal: a bespoke native Rust GUI framework as the main surface

## 3. Notes On Existing Tools

Public sources confirm:

- OpenAI has a Codex desktop app
- Claude Code itself is primarily a terminal-based tool, with IDE and other integrations around it

Public official sources do not clearly state the internal GUI framework used by Codex app, and this spec does not depend on guessing that implementation detail.

## 4. Product Shape

The implementation is a local-first application with three layers:

1. `Daemon`
   Polls GitHub, decides what needs action, manages workspaces, runs the coding agent, persists state, and dispatches notifications.

2. `HTTP/API Server`
   Exposes JSON APIs and a live event stream for the UI.

3. `Web UI`
   Renders tracked PRs, statuses, activity feed, execution history, and controls.

Optional later layer:

4. `Desktop Shell`
   A thin Tauri wrapper for distribution convenience. This shell must not own business logic.

### 4.1 Lifecycle-Oriented UI

The long-term UI direction is not a single PR table. It should organize work by lifecycle phase,
similar to tools that switch between materially different working modes instead of forcing one view
to handle every job.

Initial target panels:

- `Review`
  Focused on authored PRs, CI, reviewer feedback, wait states, and operator actions.
- `Develop`
  Focused on project-based implementation work, cross-repo code directories, resource context, and
  task management.
- `Merge`
  Focused on PRs that already satisfy merge policy and are waiting for final landing.

The MVP may still ship as a single lightweight page, but backend models and API responses should not
lock the product into a forever review-only shape.

## 5. Tech Stack

## 5A. MVP Profile

The first deliverable should optimize for the smallest end-to-end slice, not the final architecture.

MVP choices:

- single Rust binary
- `axum` backend
- in-memory runtime state
- JSON-file persistence only if needed for convenience
- static HTML, CSS, and JavaScript served by the Rust backend
- no React/Vite requirement for the first working version
- no SQLite requirement for the first working version
- no Tauri requirement for the first working version
- fake-provider-driven integration test required before calling the MVP done

This means the long-term architecture remains `Rust backend + Web UI`, but the MVP web UI can be a
minimal static page instead of a full frontend app.

### 5B. Post-MVP Expansion Direction

After the review-first MVP is working, the next architectural expansion should add:

- lifecycle panel routing in the UI
- a project model for development-phase work
- a shared resource pool with lazy-loaded document content
- Linear-like task primitives for planning and execution
- Feishu CLI-backed notification hooks for review and merge wait states

### 5.1 Backend

- Rust
- `tokio` for async runtime
- `axum` for HTTP server and API routes
- `reqwest` for GitHub API access
- `sqlx` with SQLite for durable local state
- `serde` / `serde_json` for data interchange
- `tokio::process` for agent subprocess execution

### 5.2 Frontend

- TypeScript
- React
- Vite
- a lightweight component system, not a large enterprise framework requirement

For MVP, replace this temporarily with:

- static HTML served by backend
- minimal client-side JavaScript
- no bundler requirement

### 5.3 Packaging

- initial delivery: browser UI served locally
- later optional packaging: Tauri

## 6. Core User Flows

### 6.1 Passive Monitoring

The user opens the UI and sees:

- which PRs are currently tracked
- current CI status
- current review state
- whether the daemon is idle, acting, retrying, blocked, waiting for review, or waiting for merge
- whether a PR is currently blocked by merge conflicts with the base branch
- what happened most recently for each PR
- recent live agent output for any PR that is currently running

### 6.2 Autonomous Repair

When CI fails or new actionable feedback appears:

1. daemon detects the trigger
2. daemon decides whether auto-repair is allowed
3. daemon resolves and syncs an existing local checkout for the PR
4. daemon runs the configured agent command with a prompt derived from PR state
5. daemon captures result, updates state, and refreshes UI
6. if the run fails, the daemon retries the same still-actionable signal on the next poll
7. after five automatic retries for the same signal, the daemon auto-pauses that PR until resumed

Scheduled GitHub polling should minimize rate-limit pressure by using two stages:

- first fetch the authored PR list plus lightweight pull details needed for routing and workspace sync
- then fetch reviews, comments, and check status only for candidate PRs whose lightweight state has
  changed since the previous dashboard snapshot or whose prior CI status is still unsettled

### 6.3 Human Escalation

When the agent cannot safely continue:

- unclear product decision
- missing credentials or permissions
- repeated failed repair attempts
- merge conflict or branch protection issue
- ambiguous reviewer feedback

the daemon emits a notification and marks the PR as `blocked` or `needs human`.

### 6.4 Project-Centered Development

Outside the review panel, the user should be able to organize work by project instead of by single
PR. A project may span multiple local code directories across repositories and serves as the anchor
for planning, implementation context, and related review artifacts.

Each project should be able to reference:

- multiple repository working directories
- related tasks
- related PRs
- a resource pool of summarized external context

### 6.5 Resource-Assisted Sessions

Sessions should have access to curated context without eagerly loading every linked document body.
The system should keep lightweight metadata plus summaries for resources such as Feishu docs and web
pages, then fetch full content only when a workflow step actually needs it.

## 7. Scope

### 7.0 MVP Scope

The simplest acceptable MVP includes only:

- local daemon polling loop
- one web page that lists tracked PRs and their current status
- one or two JSON endpoints used by that page
- a pluggable fake GitHub provider for testing
- a pluggable fake agent runner for testing
- one simulated integration test that exercises the loop end-to-end

The MVP does not need:

- SQLite
- notifications
- SSE or WebSockets
- detailed PR detail pages
- run history UI
- Tauri packaging
- full GitHub write automation

### 7.1 In Scope

- authored PR discovery
- CI and comment monitoring
- automatic reruns based on explicit policy
- workspace sync per PR
- agent execution
- per-PR pause/resume watching controls
- local persistence
- local browser-based GUI
- notification plugin interface
- lifecycle-aware review states
- project/resource/task modeling needed for the develop panel

### 7.2 Out Of Scope For v0

- full GitHub write automation for every comment thread
- full diff rendering inside the backend
- remote multi-user control plane
- cloud orchestration service
- Electron-specific work
- native widget GUI

## 8. PR State Model

Each tracked PR should normalize to a durable local record with:

- `repo_full_name`
- `pr_number`
- `title`
- `url`
- `author_login`
- `base_ref`
- `head_ref`
- `head_sha`
- `is_draft`
- `is_closed`
- `is_merged`
- `labels`
- `created_at`
- `updated_at`
- `ci_status`
- `ci_updated_at`
- `review_state`
- `approval_count`
- `review_comment_count`
- `issue_comment_count`
- `latest_reviewer_activity_at`
- `review_lifecycle_state`
- `merge_ready`
- `merge_blockers`
- `base_sha`
- `has_conflicts`
- `mergeable_state`
- `attention_reason`
- `tracking_status`
- `paused`
- `last_agent_run_status`
- `last_agent_run_started_at`
- `last_agent_run_finished_at`
- `last_agent_run_summary`
- `last_processed_review_comment_at`
- `last_processed_ci_signal_at`
- `last_processed_ci_head_sha`
- `last_processed_conflict_head_sha`
- `last_processed_conflict_base_sha`
- `last_processed_comment_at`
- `last_processed_ci_at`
- `last_processed_head_sha`
- `consecutive_failure_count`
- `retry_signal_trigger`
- `retry_signal_head_sha`
- `retry_signal_comment_at`
- `retry_signal_ci_at`
- `notification_state`

## 9. Tracking Statuses

Canonical statuses:

- `draft`
- `paused`
- `conflict`
- `waiting_review`
- `waiting_merge`
- `needs_attention`
- `running`
- `retry_scheduled`
- `blocked`
- `needs_human`
- `closed`
- `merged`

## 10. Attention Triggers

The daemon should consider a PR actionable when at least one of these becomes true after the last processed marker:

1. CI for the current head SHA changes to failing
2. a non-author reviewer leaves a new inline comment
3. a non-author reviewer leaves a new top-level PR comment
4. a review transitions to `changes_requested`
5. policy explicitly allows acting on `commented` reviews
6. the PR can no longer merge cleanly with the latest base branch

The daemon should not re-trigger forever on the same unchanged signal. It must compare new data against persisted processed markers.

Failed runs are special:

- a failed run must not consume the processed marker for the signal it was trying to fix
- the same still-actionable signal should therefore be retried on the next poll
- if the signal clears before the next poll, the PR should fall back out of retry state instead of remaining blocked forever

Processed markers are signal-specific:

- a successful review-feedback run only consumes the review-feedback marker
- a successful CI repair run only consumes the CI-failure marker for that head SHA
- a successful conflict-handling run only consumes the conflict marker for that `(head_sha, base_sha)` pair
- a successful run for one signal must not silently consume any other still-actionable signal on the same PR

## 11. Review Lifecycle State Derivation

In addition to attention triggers, the review panel needs a clear passive-state derivation:

- `waiting_review`
  The PR is open, not draft, not paused, not merged, not currently actionable, and does not yet
  satisfy merge-ready policy.
- `conflict`
  The PR is open and has a detected merge conflict against the current base branch snapshot. This
  state should override passive wait states and surface clearly in the review panel.
- `waiting_merge`
  The PR is open, not draft, not paused, not merged, has at least one non-author approval, and
  satisfies merge-ready policy for the current implementation tier.

For the first implementation tier, merge-ready policy should at least require:

- CI status is green
- review decision is approved
- PR is not draft, closed, or merged
- no merge conflict is currently detected

Later tiers may additionally incorporate:

- branch protection requirements
- mergeability or merge queue state
- minimum required approvals
- repository-specific landing policy

## 12. Auto-Repair Policy

Default v0 policy:

- auto-act on failing CI for the latest head SHA
- auto-act on new `changes requested`
- optionally auto-act on ordinary review comments
- do not auto-act on draft PRs
- do not auto-act on paused PRs
- do not auto-act on merged or closed PRs
- do not launch more than the configured global concurrency
- do not launch a second run for a PR that already has an active run
- retry a failed run on the next poll while the same actionable signal is still present
- allow up to five automatic retries after the initial failed run, then auto-pause the PR
- resuming a paused PR resets retry bookkeeping and triggers an immediate targeted re-check for that PR instead of waiting for the next daemon poll
- the immediate resume re-check should prefer the resumed PR over unrelated actionable PRs, while still respecting the configured global concurrency
- the immediate resume re-check should fetch only the resumed PR from GitHub rather than refreshing the entire authored PR set

## 13. Workspace Model

The local repository root is configurable.

Per-PR checkout resolution order:

1. use an explicit `workspace.repo_map["owner/repo"]` entry when present
2. otherwise look for `<workspace.root>/<repo-name>` where `repo-name` is the final path segment of `owner/repo`
3. if neither path exists, fail the run with a clear operator-facing error instead of cloning a new directory

Rules:

- resolved checkout paths must point to an existing local git repository
- automatically discovered paths must stay under `workspace.root`
- the daemon must sync the tracked PR branch before each run
- the daemon must also fetch the latest base branch before each run
- the daemon should not merge the base branch into the checked-out PR branch before launching the
  agent
- the agent should perform the base-branch merge itself inside the prepared workspace
- if the agent leaves a conflicted merge state behind, a later run should keep that conflicted
  workspace available so the agent can resume resolving it instead of starting over
- if a later run sees the same PR head SHA, the same base SHA in `MERGE_HEAD`, and unresolved
  paths already present in the working tree, it should resume from that conflict workspace instead
  of rejecting it as a generic dirty checkout
- the daemon must not create a brand-new clone as part of the normal PR execution path
- the daemon should refuse to reuse a checkout with tracked local modifications

Git transport:

- `ssh` or `https`
- default `ssh`

## 14. Agent Execution Model

Each run includes:

- trigger reason
- PR context
- workspace path
- workspace sync result
- agent command
- timeout
- the exact prompt sent to the agent stdin
- stdout and stderr capture
- exit code
- compact final summary

The agent prompt must include:

- repository and PR identity
- trigger reason
- CI and review summary
- current base and head refs
- current base and head SHAs
- current head SHA
- whether the daemon resumed an existing unresolved conflict workspace
- explicit instruction that the agent itself should merge the latest base branch into the PR branch
  when needed
- explicit instruction that merge conflicts must be resolved before the trigger-specific fix is
  considered complete
- explicit instruction to work only in the synced workspace
- explicit instruction to push only to the PR branch when safe
- explicit instruction to stop and explain blockers when unsafe

## 15. Notification Model

Notifications are emitted when:

- repeated failures exceed threshold
- a required credential is missing
- the agent reports ambiguity or blocker
- branch protection or push failure prevents completion
- a PR remains blocked beyond a configured window

Notification sinks should be pluggable. Initial sinks:

- local desktop notification
- webhook-based sink

Lifecycle transition notifications should also be supported for:

- PR entered `waiting_review`
- PR entered `waiting_merge`

Potential later sinks:

- Feishu CLI-backed sink
- Feishu
- Slack
- email

Notification deduplication should key off lifecycle signal identity so the same unchanged PR state
does not spam the user on every poll.

## 16. Project And Resource Model

The develop phase needs first-class local entities beyond PRs.

### 16.1 Project

A project represents a unit of work that may span multiple repositories and multiple tasks.

Suggested fields:

- `project_id`
- `name`
- `status`
- `repo_roots`
- `linked_pr_keys`
- `linked_task_ids`
- `resource_pool_ids`

### 16.2 Resource Pool Entry

A resource pool entry is lightweight context attached to a project or session.

Suggested fields:

- `resource_id`
- `kind` (`feishu_doc`, `web_link`, `local_file`, ...)
- `title`
- `url_or_path`
- `summary`
- `last_fetched_at`
- `cached_content_ref`

The summary should be loaded by default; full content should be fetched lazily.

## 17. Task Model And Workflow Migration

The develop panel should expose Linear-like task management while staying compatible with the
workflow ideas in `~/Coding/symphony`.

The migration intent is:

- keep explicit state routing between implementation, human review, and merge
- let projects own tasks and related resources
- let review tracking remain linked to, but not replace, development planning

The first migration should preserve the existing review-focused backend while introducing data models
that allow develop and merge panels to be added incrementally.

## 18. Persistence

SQLite is the target durable store.

Suggested tables:

- `tracked_prs`
- `pr_events`
- `agent_runs`
- `notifications`
- `settings_cache`

The current JSON-file spike can remain as a temporary migration source, but SQLite is the target design for catch-up work.

## 19. Backend API

### 19.0 MVP API Surface

Minimum API surface for MVP:

- `GET /api/health`
- `GET /api/activity`
- `GET /api/prs`
- `POST /api/prs/pause` with a JSON body containing the PR key and desired paused state
- an equivalent local test hook for manual triggering if needed

Optional for MVP:

- `GET /api/state`

### 19.1 Read APIs

- `GET /api/health`
- `GET /api/activity`
- `GET /api/state`
- `GET /api/prs`
- `GET /api/prs/:repo/:number`
- `GET /api/prs/:repo/:number/events`
- `GET /api/prs/:repo/:number/runs`
- `GET /api/config/redacted`

### 19.2 Live Updates

At least one of:

- `GET /api/events` via Server-Sent Events
- `GET /api/ws` via WebSocket

SSE is preferred for v0 because the UI is mostly dashboard-style and low-frequency.

### 19.3 Action APIs

Current prototype-compatible action API:

- `POST /api/prs/pause` with `{ "key": "<repo>#<number>", "paused": true|false }`
  When `paused` is `false`, the backend should queue an immediate background re-check for that PR.

Potential richer follow-up actions:

- `POST /api/prs/:repo/:number/recheck`
- `POST /api/prs/:repo/:number/run-now`
- `POST /api/prs/:repo/:number/pause`
- `POST /api/prs/:repo/:number/resume`

These actions are local operator actions only.

## 20. Web UI

Primary screens:

### 20.0 MVP UI

The MVP UI can be a single page that shows:

- daemon health
- last poll time
- a right-aligned dashboard tab switch for `PRs` and `Activity`
- current tracked PR rows
- each PR’s status, CI state, review state, and latest action summary
- a row-level link into a dedicated PR detail page for run output, showing a Codex CLI transcript that includes both the prompt sent to Codex and the live or saved Codex output
- a row-level pause/resume control for each tracked PR
- a visually subdued treatment for paused rows so they read as intentionally muted rather than inactive by accident

The MVP UI does not need:

- split panes
- SSE or WebSocket delivery for live updates
- advanced filtering
- broader manual controls beyond pause/resume and the minimum required for testing

### 20.1 PR List

Columns:

- PR
- repo
- title
- status
- CI
- reviews
- attention reason
- last action
- updated time
- action

### 20.2 PR Detail

Panels:

- summary
- recent comments/reviews summary
- run history
- live Codex CLI transcript for the current run, including the prompt input and Codex output
- saved latest run transcript when no run is currently active, including the prompt input and Codex output
- latest run output summary
- workspace path
- notification state

### 20.3 Activity Feed

Global chronological feed of poll cycles, state transitions, run starts, run finishes, and notification events, shown behind the dashboard's `Activity` tab.

The feed should make daemon progress legible even when no PR is currently running, including:

- scheduled poll start events
- targeted re-check start events
- per-poll GitHub request-count summaries that show how many API calls each poll consumed and how
  much heavy hydration work happened
- explicit idle/no-actionable poll summaries
- runner-slot backpressure messages when work exists but concurrency is full

### 20.4 Control Bar

- daemon health
- poll interval
- active run count
- next poll countdown

## 21. Security And Safety

- local-first only by default
- GitHub token stored via environment variable or local config reference
- UI must never expose raw tokens
- auto-discovered checkout paths must stay under the configured `workspace.root`
- explicitly configured `workspace.repo_map` paths may live elsewhere, but must resolve to an existing local git repository
- agent subprocess cwd must equal workspace path
- existing local checkouts must not be silently rewritten when they contain tracked local modifications
- shell packaging must not bypass backend policy

## 22. Migration Note

Current state in this repository is a prototype spike that already contains:

- the standalone repository split from the parent `symphony` repo
- GitHub polling logic
- local state handling
- persisted per-PR pause/resume state
- retry bookkeeping for failed runs and auto-pause after repeated retry exhaustion
- existing-checkout resolution via `workspace.root` plus explicit `workspace.repo_map` overrides
- agent runner skeleton
- a minimal local web dashboard

That code should be treated as exploratory, not the final architecture. The next implementation pass should preserve reusable backend logic where practical, but move the product surface to HTTP + Web UI.

## 23. Deliverables

Phase-complete v0 means:

1. daemon can track configured authored PRs
2. web UI shows current state and live updates
3. actionable CI/review changes trigger runs exactly once per new signal
4. agent runs happen in resolved existing local checkouts, without cloning new per-PR workspaces in the normal path
5. blocker cases notify the user
6. runtime state survives restart via SQLite

## 24. MVP Done Definition

The MVP is done when all of the following are true:

1. running the binary starts a local web server
2. opening the local page shows tracked PR rows from the backend
3. the daemon can poll a provider and derive `waiting_review`, `waiting_merge`, `needs_attention`, and `running` or `blocked`
4. the backend can invoke a runner abstraction for one actionable PR
5. there is at least one `cargo test` simulated integration test that:
   - uses a fake GitHub provider
   - uses a fake agent runner
   - starts the app in-process
   - exercises a poll cycle
   - verifies state through the API
   - verifies that an actionable PR triggers a run exactly once for the simulated signal
