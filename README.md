# Symphony RS

`symphony-rs` is a new Rust-based Symphony implementation focused on GitHub pull requests.

Current status:

- the MVP now runs as a local Rust daemon plus HTTP server
- the UI is a minimal web dashboard served directly by the backend
- the source of truth for the ongoing roadmap is still:
  - `docs/github_pr_supervisor_spec.md`
  - `docs/github_pr_supervisor_tasks.md`

The current MVP combines:

- a background polling daemon that watches your authored GitHub PRs
- a lightweight review-request inbox for PRs that currently request your review
- a configurable agent runner that can try to fix CI or reviewer feedback automatically
- a local web dashboard served from the same Rust process

The planned product direction is:

- Rust daemon/backend
- richer local web UI
- optional Tauri shell later

## What It Tracks

For each open PR authored by you, Symphony RS polls GitHub and computes a live status:

- `waiting review`: nothing actionable right now, still waiting on human review
- `waiting merge`: approved and CI passed, waiting to be merged
- `conflict`: the PR does not currently merge cleanly with the latest base branch
- `needs attention`: new reviewer feedback or a newly failing CI signal
- `running`: the local agent command is currently working on the PR
- `blocked`: the last agent run failed
- `draft`, `closed`, `merged`: terminal or non-actionable states

The UI shows the authored PR list, a `Review Requests` tab for PRs that currently request your review, CI/review state, attention reason, timestamps, top-right dashboard tabs for switching between the PR view, review inbox, and live daemon activity, a dedicated run-details page that streams live Codex CLI output for active runs and preserves the latest run output after completion, and a visibly subdued row state when a PR is paused.

## Requirements

- Rust toolchain `1.93.0` or newer
- `git`
- a GitHub token in `GITHUB_TOKEN` or `GH_TOKEN`
- an agent command available on your machine
  - the example config assumes `codex`
  - if you want Codex to run with unsandboxed full local access, set
    `agent.dangerously_bypass_approvals_and_sandbox = true`
- existing local checkouts for the repositories you want Symphony RS to operate on
  - by default it looks under `workspace.root` for a directory named after the repo, such as `../tikv` for `tikv/tikv`
  - if auto-discovery is not enough, you can provide `workspace.repo_map` entries in the config
- credentials for pushing back to your PR branches
  - SSH is the default git transport in the example config

## Quick Start

1. Copy the example config:

```bash
cp symphony-rs.example.toml symphony-rs.toml
```

2. Export a GitHub token:

```bash
export GITHUB_TOKEN=...
```

3. Build and launch the local daemon and dashboard server from the project root using the optimized release binary:

```bash
cargo build --release
target/release/symphony-rs --config symphony-rs.toml
```

The example config sets `workspace.root = ".."`, which means Symphony RS will look for sibling
repositories next to `symphony-rs` before it tries any explicit `workspace.repo_map` overrides.
If you enable `agent.dangerously_bypass_approvals_and_sandbox`, Symphony will invoke Codex with
full unsandboxed access, so only use that on a machine you already trust.

4. Open the dashboard in your browser:

```bash
http://127.0.0.1:8787/
```

For long-running local use, prefer `target/release/symphony-rs` over `cargo run` so the daemon
stays on the optimized release build. The binary is already server-only in this MVP. `--headless`
is kept as a compatibility no-op for older command lines.

## How The Agent Loop Works

When Symphony RS detects a PR that needs attention, it:

1. resolves an existing local repository checkout for the PR
2. syncs the PR head branch into that checkout
3. fetches the latest base branch refs into that checkout without merging them
4. if a later retry sees the same unresolved merge-conflict workspace for the same PR head/base pair, it resumes from that workspace instead of rejecting it as a generic dirty checkout
5. asks the agent to merge the latest base branch, resolve conflicts if needed, and then continue with the CI or review fix
6. builds an execution prompt from the PR context and trigger reason
7. pipes that prompt to the configured agent command
8. updates the UI and persisted state with the result

The default prompt asks the agent to inspect GitHub feedback and CI, merge the latest base branch itself when needed, resolve conflicts before declaring success, fix code in-place, run targeted validation, and push back to the PR branch if it can. It also tells the agent to stop and ask for operator direction before making material or high-risk changes instead of changing code unilaterally.

Manual deep reviews use a separate read-only prompt: they inspect the diff, produce a concise review report, and the backend posts that report back to the PR as a comment when the run succeeds.

## Current Assumptions

- authored PRs are discovered via the GitHub Search API
- review feedback detection is based on reviews, inline review comments, and issue comments from people other than the PR author
- CI attention is triggered only when a failing status/check is newer than the last processed CI signal for that PR
- successful runs only consume the trigger they were started for; unchanged review and CI signals on the same PR stay independently actionable
- repository resolution prefers `workspace.repo_map` when present, then falls back to `<workspace.root>/<repo-name>`
- Symphony RS reuses existing local checkouts and refuses to operate on a tracked repo that has local tracked changes
- the current UI is local-only and intentionally minimal
- state persistence is still JSON-backed for MVP

## Validation

```bash
cargo +1.93.0 test
```

This includes a simulated integration test that boots the app in-process with a fake GitHub provider and fake runner, verifies one PR becomes `running`, then confirms the same unchanged signal does not trigger a duplicate run on the next poll.
