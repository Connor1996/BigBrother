# BigBrother: a local agent for keeping an eye on your GitHub PRs

End-to-end automation from requirement to implementation to testing to deployment is already becoming realistic in many agentic workflows. But in real TiDB and TiKV production work, many tasks still are not solved in one pass. We still need repeated back-and-forth with agents on design and implementation details, especially once review starts and the code needs another round of polishing.

During implementation, teams already have many ways to work with agents. You can use coding agents such as Codex or Claude Code directly, spin up subagents for parallel research and implementation, or organize work through agent-team setups such as slock.ai. BigBrother is not trying to standardize that open-ended part of the workflow.

It is built for a different problem: the long tail after a PR is already open. At that point, the cost is often not the code change itself, but the repeated work around review comments, failing CI, merge conflicts, and simply keeping track of which PR needs attention next. That problem fits a persistent operational surface better than a pure chat interface.

The detailed roadmap and reference behavior still live in
[`docs/github_pr_supervisor_spec.md`](/Users/Connor/Coding/bigbrother/docs/github_pr_supervisor_spec.md)
and
[`docs/github_pr_supervisor_tasks.md`](/Users/Connor/Coding/bigbrother/docs/github_pr_supervisor_tasks.md).

## What It Does

BigBrother runs as a local Rust daemon plus web dashboard. It keeps polling the open PRs you authored and the PRs that currently request your review. When it sees a signal that needs action, it resolves a local source repository, switches into its own managed worktree under `<workspace.root>/bigbrother-worktrees`, and hands the trigger plus PR context to a local coding agent such as Codex or Claude Code. The run output, result, and latest status are recorded in the dashboard.

In practice, it handles a few recurring PR workflows:

- `Review feedback`: new review comments or issue comments can trigger an agent run that reads feedback, merges the latest base, attempts the change, pushes, and replies on the PR
- `CI failure`: new failing checks can trigger either a code fix or a `/retest` when the failure looks flaky or not worth a speculative change
- `Merge conflict`: conflicts are resolved inside BigBrother's managed worktree instead of your everyday checkout
- `Review Requests`: PRs that request your review land in a separate inbox where you can trigger a read-only `Deep Review`
- `Track / Untrack`: you can pause automatic follow-up for a PR without losing its current dashboard state
- `needs decision`, `failed`, and `running`: the dashboard surfaces when the agent is blocked, failed, or still actively working

The UI shows the authored PR list, a `Review Requests` tab, CI and review state, daemon activity, and a dedicated run-details page with terminal output for active and completed runs.

## Quick Start

### Requirements

- Rust toolchain `1.93.0` or newer
- `git`
- `gh` installed and already authenticated
- `codex` or `claude` installed and already authenticated
- local checkouts of the repositories you want BigBrother to operate on
- Git credentials that can push back to your PR branches

### Steps

1. Copy the example config:

```bash
cp bigbrother.example.toml bigbrother.toml
```

2. Open `bigbrother.toml` and confirm these settings:

- `workspace.root`: the repositories you want BigBrother to manage should already exist locally and be discoverable from this root
- `workspace.repo_map`: optional manual overrides for repositories that are not checked out at the default path. For example, if `tikv/tikv` is not at `<workspace.root>/tikv`, add something like `workspace.repo_map = { "tikv/tikv" = "/Users/alice/src/tikv-dev" }`
- `[agent].command`: use `codex` or `claude` depending on the local agent you want BigBrother to run

3. If you want Feishu notifications, set up local `lark-cli` first:

```bash
npm install -g @larksuite/cli
lark-cli config init
```

Then set `notifications.feishu.receive_id` to your Feishu-bound email. If you keep the template value `"$FEISHU_NOTIFY_EMAIL"`, export:

```bash
export FEISHU_NOTIFY_EMAIL="you@example.com"
```

4. Build and start BigBrother:

```bash
cargo build --release
GITHUB_TOKEN="$(gh auth token)" target/release/bigbrother --config bigbrother.toml
```

5. Open [http://127.0.0.1:8787/](http://127.0.0.1:8787/).

The example config sets `workspace.root = ".."`, which makes BigBrother look for sibling repositories next to `bigbrother` before it tries any explicit `workspace.repo_map` overrides. That also means managed worktrees will live under `../bigbrother-worktrees`.

The shipped example config defaults Codex to full local access via `--dangerously-bypass-approvals-and-sandbox`, so only use it as-is on a machine you already trust. If you do not want unsandboxed local access, remove that flag from `[agent].args`.

For long-running local use, prefer `target/release/bigbrother` over `cargo run` so the daemon stays on the optimized release build.

## Optional Feishu Notifications

The current Feishu integration is a one-way notification sink for daemon activity. It sends key
automatic-run and manual deep-review updates outward, but it does not yet accept Feishu commands
or replies.

For teams that already have `lark-cli` configured with a working bot identity, BigBrother can send
through that local CLI instead of holding the app secret itself:

```toml
[notifications.feishu]
transport = "lark_cli_bot"
receive_id = "$FEISHU_NOTIFY_EMAIL"
receive_id_type = "email"
label = "connor-mbp"
timeout_secs = 10
```

The legacy direct OpenAPI app-bot transport remains available:

```toml
[notifications.feishu]
transport = "app_bot"
app_id = "$FEISHU_APP_ID"
app_secret = "$FEISHU_APP_SECRET"
receive_id = "$FEISHU_NOTIFY_EMAIL"
receive_id_type = "email"
label = "connor-mbp"
timeout_secs = 10
```

Current Feishu notifications cover:

- automatic agent run completion or failure
- `needs decision` escalation
- manual deep review completion
- daemon poll failures

`label` is included in each message so multiple daemon instances can identify themselves clearly in
private DMs. `receive_id_type` supports Feishu message targets such as `email`,
`open_id`, `user_id`, `union_id`, and `chat_id`; for a first private-DM setup, `email` is usually
the easiest option. `transport = "lark_cli_bot"` uses the locally configured `lark-cli` bot via
`lark-cli api POST /open-apis/im/v1/messages --as bot`, while `transport = "app_bot"` preserves
the earlier direct OpenAPI flow.

## Prompt Templates

The default agent prompts now live in Markdown files under
[`prompts/`](/Users/Connor/Coding/bigbrother/prompts/README.md). That makes the built-in prompt
wording visible in the repo and lets each operator tweak the repository prompt files directly.

BigBrother ships these default prompt files:

- [`prompts/actionable.md`](/Users/Connor/Coding/bigbrother/prompts/actionable.md) for CI failures, merge conflicts, and review-feedback runs
- [`prompts/deep_review.md`](/Users/Connor/Coding/bigbrother/prompts/deep_review.md) for manual deep reviews
- [`prompts/ci_failure_rules.md`](/Users/Connor/Coding/bigbrother/prompts/ci_failure_rules.md) for the CI-only `/retest` guidance block
- [`prompts/workspace_ready.md`](/Users/Connor/Coding/bigbrother/prompts/workspace_ready.md) and [`prompts/resumed_conflict.md`](/Users/Connor/Coding/bigbrother/prompts/resumed_conflict.md) for workspace-preparation notes
- [`prompts/deep_review_artifact.md`](/Users/Connor/Coding/bigbrother/prompts/deep_review_artifact.md) for the deep-review artifact instructions

BigBrother reads those templates from the repository's own `prompts/*.md` files. To customize
prompts on a given machine, edit the repo files directly.

## How The Agent Loop Works

When BigBrother detects a PR that needs attention, it:

1. resolves an existing local source repository for the PR via `workspace.repo_map` or `<workspace.root>/<repo-name>`
2. creates or reuses a centralized BigBrother-managed worktree for that repo under `<workspace.root>/bigbrother-worktrees/<repo-name>-bigbrother`
3. syncs the PR head and latest base refs into that managed worktree and checks out the PR head in detached-HEAD mode
4. if the same PR later retries with the same unresolved merge state, it resumes from that managed worktree; a different PR from the same repo must wait until the active run finishes before BigBrother rebuilds that worktree and takes it over
5. asks the agent to merge the latest base branch, resolve conflicts if needed, and then continue with the CI or review fix
6. builds an execution prompt from the PR context and trigger reason
7. for `codex exec` and `claude -p`, passes that prompt as the initial prompt argument instead of stdin so the PTY session can preserve richer terminal-style output; other agent commands still read prompt text from stdin
8. updates the UI and persisted state with the result

When `[agent].command` resolves to `codex`, Codex-specific
flags should live directly in `args`, including any explicit reasoning-effort override such as
`-c model_reasoning_effort="xhigh"`. The runner still forces `--color always` when you have not
already specified a color override, and it passes the prompt as the initial `codex exec` prompt
argument so the PTY session looks closer to a native terminal run.

When `[agent].command` resolves to `claude`, BigBrother treats Claude Code print mode as the
supported non-interactive path. If your
args include `-p` or `--print`, the runner appends the assembled prompt as that print-mode
argument instead of piping it through stdin. Any full-access or other Claude-specific behavior
should be expressed directly in `args`, for example `--dangerously-skip-permissions`.

The default prompt templates ask the agent to inspect GitHub feedback and CI, merge the latest base branch itself when needed, resolve conflicts before declaring success, fix code in-place, run targeted validation, and push back to the PR branch if it can. Because the managed worktree uses detached HEAD, the prompt also tells the agent not to create or rely on a local branch and to publish explicitly with `git push "$BIGBROTHER_PR_PUSH_REMOTE" HEAD:"$BIGBROTHER_PR_HEAD_REF"`. They also tell the agent to stop and ask for operator direction before making material or high-risk changes instead of changing code unilaterally. When the agent decides a change is non-trivial, it emits a machine-readable `BIGBROTHER_NEEDS_DECISION:` marker; BigBrother then sets the PR to `needs decision`, auto-freezes future automatic runs for that PR under the hood, and stores the full operator-facing explanation in the PR details output until you explicitly clear it from the dashboard.

If a `needs decision` run came from review feedback and you address that feedback manually, clicking
`Addressed` marks the currently displayed review signal as handled before the immediate targeted
re-check. That prevents the daemon from re-running immediately on the same unchanged review
activity, while still allowing newer reviewer activity to become actionable later.

Manual deep reviews use the repo-vendored `$deep-review` skill in read-only mode: they write the final review markdown under `target/bigbrother-deep-review`, and the backend posts only that final review artifact back to the PR as a comment when the run succeeds.

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

## Releases

Pushing a version tag that matches `v*` now triggers the GitHub Actions release workflow in
[`/.github/workflows/release.yml`](/Users/Connor/Coding/bigbrother/.github/workflows/release.yml).
The workflow runs the test suite on Linux first, then builds release archives for:

- `x86_64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

Each release asset is uploaded to the matching GitHub Release as a `.tar.gz` archive plus a
`.sha256` checksum file. The archive contains the `bigbrother` binary and the repository
`README.md`.

Typical release flow:

```bash
git tag -a v0.1.0 -m "v0.1.0"
git push origin v0.1.0
```

After the tag push finishes, GitHub creates or updates the `v0.1.0` release and attaches the
platform-specific archives automatically.
