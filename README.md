# BigBrother: a local agent for keeping an eye on your GitHub PRs

BigBrother is a local Rust daemon and web dashboard for keeping GitHub pull requests moving after
they are already open.

It is not trying to standardize how teams do implementation. Instead, it focuses on the repetitive
follow-up work around review comments, failing CI, merge conflicts, and review requests by handing
actionable PR context to a local coding agent such as Codex or Claude Code.

## What It Does

![BigBrother dashboard](docs/img_v3_0210g_df8c0026-a7fe-42b8-93af-fdb2174d5c3g.jpg)

BigBrother polls the PRs you authored and the PRs that currently request your review. When it sees
a signal that needs attention, it resolves a local source repository, switches into a managed
worktree under `<workspace.root>/bigbrother-worktrees`, and runs a local coding agent there. The
dashboard records run output, result, and the latest PR state.

Common workflows:

- `Review feedback`: new review comments or issue comments can trigger an agent run that reads the
  feedback, merges the latest base, attempts the change, pushes, and replies on the PR
- `CI failure`: new failing checks can trigger either a code fix or a `/retest` when the failure
  looks flaky or not worth a speculative change
- `Merge conflict`: conflicts are resolved inside BigBrother's managed worktree instead of your
  everyday checkout
- `Review Requests`: PRs that request your review land in a separate inbox where you can trigger a
  read-only `Deep Review`
- `Track / Untrack`: you can pause automatic follow-up for a PR without losing its current dashboard
  state
- `needs decision`, `failed`, and `running`: the dashboard surfaces when the agent is blocked,
  failed, or still actively working

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

2. Open `bigbrother.toml` and confirm:

- `workspace.root` points at the local root where your repositories already live
- `workspace.repo_map` is set only for repositories that are not at
  `<workspace.root>/<repo-name>`
- `[agent].command` uses the local agent you want BigBrother to run
- `[agent].args` matches the permissions and model settings you want on this machine

3. Optional: if you want Feishu notifications, set up local `lark-cli` and export your target
   email when using the example config:

```bash
npm install -g @larksuite/cli
lark-cli config init
export FEISHU_NOTIFY_EMAIL="you@example.com"
```

4. Build and start BigBrother:

```bash
cargo build --release
GITHUB_TOKEN="$(gh auth token)" target/release/bigbrother --config bigbrother.toml
```

5. Open [http://127.0.0.1:8787/](http://127.0.0.1:8787/).

For a fuller setup walkthrough, see [`docs/bigbrother_getting_started.md`](docs/bigbrother_getting_started.md).

## How It Works

1. Resolve the PR's local source repository through `workspace.repo_map` or
   `<workspace.root>/<repo-name>`.
2. Create or reuse a BigBrother-managed worktree under
   `<workspace.root>/bigbrother-worktrees/<repo-name>-bigbrother`.
3. Sync the PR head and base refs, build a prompt from the PR context and trigger, and invoke the
   configured local agent.
4. Record terminal output and outcome in the dashboard. If the agent emits
   `BIGBROTHER_NEEDS_DECISION:`, BigBrother pauses automatic follow-up for that PR until you step
   in.

## More Docs

- [`docs/bigbrother_getting_started.md`](docs/bigbrother_getting_started.md): fuller onboarding and
  screenshots
- [`docs/github_pr_supervisor_spec.md`](docs/github_pr_supervisor_spec.md): detailed runtime
  behavior and operator-facing semantics
- [`docs/github_pr_supervisor_tasks.md`](docs/github_pr_supervisor_tasks.md): task breakdown and
  roadmap notes
- [`prompts/README.md`](prompts/README.md): prompt templates and placeholders

## Validation

```bash
cargo +1.93.0 test
```

## Releases

Pushing a tag that matches `v*` triggers
[`release.yml`](.github/workflows/release.yml), which runs tests, builds the release archives, and
uploads them to the matching GitHub Release.
