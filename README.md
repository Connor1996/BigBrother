# BigBrother

`BigBrother` is the product name for the Rust-based GitHub PR supervisor implemented in the `symphony-rs` repository.

If you want a prose-first introduction for teammates, start with
[`docs/bigbrother_getting_started.md`](/Users/Connor/Coding/symphony-rs/docs/bigbrother_getting_started.md).
If you want Codex to perform most of the first-time setup, use the copy-paste prompt in
[`docs/bigbrother_agent_setup_prompt.md`](/Users/Connor/Coding/symphony-rs/docs/bigbrother_agent_setup_prompt.md).

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

For each open PR authored by you, BigBrother polls GitHub and computes a live status:

- `waiting review`: nothing actionable right now, still waiting on human review
- `waiting merge`: approved and CI passed, waiting to be merged
- `conflict`: the PR does not currently merge cleanly with the latest base branch
- `needs attention`: new reviewer feedback or a newly failing CI signal
- `needs decision`: the agent determined the required change is non-trivial and needs operator approval before editing
- `failed`: the latest automatic agent run failed and the same PR signal is still unresolved; the daemon leaves it idle until you click `Retry`
- `untracked`: the operator intentionally froze this PR so scheduled polls keep its last snapshot but no automatic run will start
- `running`: the local agent command is currently working on the PR
- `draft`, `closed`, `merged`: terminal or non-actionable states

The UI shows the authored PR list, a `Review Requests` tab for PRs that currently request your review, CI/review state, timestamps, top-right dashboard tabs for switching between the PR view, review inbox, and live daemon activity, and a dedicated run-details page that uses a browser-rendered terminal for both active Codex runs and saved completed-run terminal recordings, falling back to wrapped last-run text output only when no PTY terminal capture exists. Terminal recordings are kept as full PTY output rather than a short screen snapshot so completed runs can be replayed with their saved scrollback. It also keeps a visibly subdued row state when a PR is explicitly shown as `untracked`.

## Requirements

- Rust toolchain `1.93.0` or newer
- `git`
- a GitHub token in `GITHUB_TOKEN` or `GH_TOKEN`
- an agent command available on your machine
  - the example config assumes `codex`
  - BigBrother defaults Codex runs to `model_reasoning_effort = "xhigh"` and passes that
    explicitly via `codex -c model_reasoning_effort=... exec ...`
  - if you want Codex to run with unsandboxed full local access, set
    `agent.dangerously_bypass_approvals_and_sandbox = true`
- existing local checkouts for the repositories you want BigBrother to operate on
  - by default it looks under `workspace.root` for a directory named after the repo, such as `../tikv` for `tikv/tikv`
  - if auto-discovery is not enough, you can provide `workspace.repo_map` entries in the config
  - these paths are only used to discover the source repository; BigBrother runs edits in its own managed worktrees under `<workspace.root>/bigbrother-worktrees`
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

If you keep `author = "$GITHUB_USER"` in the copied config, also export `GITHUB_USER` to your
GitHub login. Otherwise replace `author` with your real login or remove the field so BigBrother
can resolve the viewer login directly from GitHub. If you customize `workspace.root`, prefer an
absolute path such as `/Users/alice/Coding`; the config loader does not currently expand `~` or
`$HOME/Coding`.

3. Build and launch the local daemon and dashboard server from the project root using the optimized release binary:

```bash
cargo build --release
target/release/symphony-rs --config symphony-rs.toml
```

The example config sets `workspace.root = ".."`, which means BigBrother will look for sibling
repositories next to `symphony-rs` before it tries any explicit `workspace.repo_map` overrides.
It also means BigBrother will place its centralized managed worktrees under
`../bigbrother-worktrees`, with one reusable detached-HEAD worktree per repository such as
`../bigbrother-worktrees/tikv-bigbrother`.
If you enable `agent.dangerously_bypass_approvals_and_sandbox`, BigBrother will invoke Codex with
full unsandboxed access, so only use that on a machine you already trust.
By default, BigBrother also injects `-c model_reasoning_effort="xhigh"` and `--color always` into
`codex exec` so the daemon does not depend on whatever ambient global Codex config happens to be
present on the host and can preserve ANSI-colored terminal output in the browser terminal.

4. Open the dashboard in your browser:

```bash
http://127.0.0.1:8787/
```

For long-running local use, prefer `target/release/symphony-rs` over `cargo run` so the daemon
stays on the optimized release build. The binary is already server-only in this MVP. `--headless`
is kept as a compatibility no-op for older command lines.

## Optional Feishu Notifications

The current Feishu integration is a one-way notification sink for daemon activity. It sends key
automatic-run and manual deep-review updates outward, but it does not yet accept Feishu commands
or replies.

```toml
[notifications.feishu]
app_id = "$FEISHU_APP_ID"
app_secret = "$FEISHU_APP_SECRET"
receive_id = "$FEISHU_NOTIFY_EMAIL"
receive_id_type = "email"
label = "connor-mbp"
timeout_secs = 10
```

Current Feishu notifications cover:

- automatic agent run start
- automatic agent run completion or failure
- `needs decision` escalation
- manual deep review start and completion
- daemon poll failures

`label` is included in each message so multiple daemon instances can identify themselves clearly in
private DMs. `receive_id_type` supports Feishu message targets such as `email`,
`open_id`, `user_id`, `union_id`, and `chat_id`; for a first private-DM setup, `email` is usually
the easiest option.

## Prompt Templates

The default agent prompts now live in Markdown files under
[`prompts/`](/Users/Connor/Coding/symphony-rs/prompts/README.md). That makes the built-in prompt
wording visible in the repo and lets each operator tweak the repository prompt files directly.

BigBrother ships these default prompt files:

- [`prompts/actionable.md`](/Users/Connor/Coding/symphony-rs/prompts/actionable.md) for CI failures, merge conflicts, and review-feedback runs
- [`prompts/deep_review.md`](/Users/Connor/Coding/symphony-rs/prompts/deep_review.md) for manual deep reviews
- [`prompts/ci_failure_rules.md`](/Users/Connor/Coding/symphony-rs/prompts/ci_failure_rules.md) for the CI-only `/retest` guidance block
- [`prompts/workspace_ready.md`](/Users/Connor/Coding/symphony-rs/prompts/workspace_ready.md) and [`prompts/resumed_conflict.md`](/Users/Connor/Coding/symphony-rs/prompts/resumed_conflict.md) for workspace-preparation notes
- [`prompts/deep_review_artifact.md`](/Users/Connor/Coding/symphony-rs/prompts/deep_review_artifact.md) for the deep-review artifact instructions

BigBrother reads those templates from the repository's own `prompts/*.md` files. To customize
prompts on a given machine, edit the repo files directly.

## How The Agent Loop Works

When BigBrother detects a PR that needs attention, it:

1. resolves an existing local source repository for the PR via `workspace.repo_map` or `<workspace.root>/<repo-name>`
2. creates or reuses a centralized BigBrother-managed worktree for that repo under `<workspace.root>/bigbrother-worktrees/<repo-name>-bigbrother`
3. syncs the PR head and latest base refs into that managed worktree and checks out the PR head in detached-HEAD mode
4. if the same PR later retries with the same unresolved merge state, it resumes from that managed worktree; if a different PR for the same repo needs the worktree first, BigBrother can rebuild it and take over
5. asks the agent to merge the latest base branch, resolve conflicts if needed, and then continue with the CI or review fix
6. builds an execution prompt from the PR context and trigger reason
7. for `codex exec`, passes that prompt as the initial prompt argument instead of stdin so the PTY session can preserve richer terminal-style output; other agent commands still read prompt text from stdin
8. updates the UI and persisted state with the result

When the configured agent command is `codex`, BigBrother treats reasoning effort as a first-class
agent setting. The `[agent] model_reasoning_effort` config defaults to `xhigh`, and the runner
always passes it explicitly to `codex exec` via `-c model_reasoning_effort="..."`. It also forces
`--color always` and passes the prompt as the initial `codex exec` prompt argument so the PTY
session looks closer to a native terminal run.

The default prompt templates ask the agent to inspect GitHub feedback and CI, merge the latest base branch itself when needed, resolve conflicts before declaring success, fix code in-place, run targeted validation, and push back to the PR branch if it can. Because the managed worktree uses detached HEAD, the prompt also tells the agent not to create or rely on a local branch and to publish explicitly with `git push "$SYMPHONY_PR_PUSH_REMOTE" HEAD:"$SYMPHONY_PR_HEAD_REF"`. They also tell the agent to stop and ask for operator direction before making material or high-risk changes instead of changing code unilaterally. When the agent decides a change is non-trivial, it emits a machine-readable `BIGBROTHER_NEEDS_DECISION:` marker; BigBrother then sets the PR to `needs decision`, auto-freezes future automatic runs for that PR under the hood, and stores the full operator-facing explanation in the PR details output until you explicitly clear it from the dashboard.

If a `needs decision` run came from review feedback and you address that feedback manually, clicking
`Addressed` marks the currently displayed review signal as handled before the immediate targeted
re-check. That prevents the daemon from re-running immediately on the same unchanged review
activity, while still allowing newer reviewer activity to become actionable later.

Manual deep reviews use the `$deep-review` skill in read-only mode: they write the final review markdown under `target/bigbrother-deep-review`, and the backend posts only that final review artifact back to the PR as a comment when the run succeeds.

## Current Assumptions

- authored PRs are discovered via the GitHub Search API
- review feedback detection is based on reviews, inline review comments, and issue comments from people other than the PR author
- CI attention is triggered only when a failing status/check is newer than the last processed CI signal for that PR
- successful runs only consume the trigger they were started for; unchanged review and CI signals on the same PR stay independently actionable
- repository discovery prefers `workspace.repo_map` when present, then falls back to `<workspace.root>/<repo-name>`
- BigBrother derives a centralized managed worktree root at `<workspace.root>/bigbrother-worktrees` and uses one reusable detached-HEAD worktree per repository
- the source discovery repository may be dirty because BigBrother no longer edits it directly; the managed worktree is the only execution workspace
- the current UI is local-only and intentionally minimal
- state persistence is still JSON-backed for MVP

## Validation

```bash
cargo +1.93.0 test
```

This includes a simulated integration test that boots the app in-process with a fake GitHub provider and fake runner, verifies one PR becomes `running`, then confirms the same unchanged signal does not trigger a duplicate run on the next poll.
