# BigBrother: a local agent for keeping an eye on your GitHub PRs

End-to-end automation from requirement to implementation to testing to deployment is already becoming realistic in many agentic workflows. But in real TiDB and TiKV production work, many tasks still are not solved in one pass. We still need repeated back-and-forth with agents on design and implementation details, especially once review starts and the code needs another round of polishing.

During implementation, teams already have many ways to work with agents. You can use coding agents such as Codex or Claude Code directly, spin up subagents for parallel research and implementation, or organize work through agent-team setups such as slock.ai. BigBrother is not trying to standardize that open-ended part of the workflow.

It is built for a different problem: the long tail after a PR is already open. At that point, the cost is often not the code change itself, but the repeated work around review comments, failing CI, merge conflicts, and simply keeping track of which PR needs attention next. That problem fits a persistent operational surface better than a pure chat interface.

## What it does

![alt text](img_v3_0210g_df8c0026-a7fe-42b8-93af-fdb2174d5c3g.jpg)

BigBrother runs as a local daemon plus web dashboard. It keeps polling the open PRs you authored and the PRs that currently request your review. When it sees a signal that needs action, it uses its own managed worktree and hands the trigger plus PR context to a local coding agent such as Codex or Claude Code. The run output, result, and latest status are recorded in the dashboard. If Feishu is configured, important updates can also be sent through the local `lark-cli` bot flow.

In day-to-day use, its behavior is roughly this:

- `Review feedback`: when an authored PR gets new review feedback, inline comments, or top-level comments that have not been handled yet, BigBrother treats that as a new actionable signal. It can start an agent run, read the feedback, merge the latest base, attempt the change, push, and reply on the PR. If the agent decides the change should not be landed automatically, the PR moves to `needs decision`. Once you step in and make the call, you can mark it `Addressed` and let BigBrother continue.
![alt text](img_v3_0210h_f55fc9d4-0bbd-4d1e-b46a-c7a2adcef6ag.jpg)
![alt text](image-1.png)
- `CI failure`: when a new failing check or status appears, BigBrother treats that as another trigger. It can ask the agent to fix code directly, or decide to post `/retest` when the failure looks flaky or not worth a speculative code change.
![alt text](e6e045dc-529d-409f-a05e-2edb25319ff5.jpeg)
- `Merge conflict`: when a PR no longer merges cleanly with the latest base branch, BigBrother asks the agent to resolve the conflict inside its managed worktree instead of touching your everyday checkout.
- `Review Requests`: for PRs that currently request your review, BigBrother puts them in the `Review Requests` inbox and lets you manually trigger a read-only `Deep Review`.
It generates a structured review result and posts only the final cleaned-up review artifact back to the PR.
![alt text](image.png)
- `Track / Untrack`: if you do not want BigBrother to keep following a PR for a while, you can `Untrack` it. The current state stays visible in the dashboard, but automatic follow-up stops until you `Track` it again.

## Quick Start

### Requirements

- Rust toolchain and `git`
- `gh` installed and already authenticated
- `codex` or `claude` installed and already authenticated

### Steps

1. Copy the config template.

```bash
cp bigbrother.example.toml bigbrother.toml
```

2. Open `bigbrother.toml` and confirm these settings:

- `workspace.root`: the repositories you want BigBrother to manage should already exist locally and be discoverable from this root
- `workspace.repo_map`: optional manual overrides for repositories that are not checked out at the default path. For example, if `tikv/tikv` is not at `<workspace.root>/tikv`, add something like `workspace.repo_map = { "tikv/tikv" = "/Users/alice/src/tikv-dev" }`

3. If you want Feishu notifications, set up local `lark-cli` first:

```bash
npm install -g @larksuite/cli
lark-cli config init
```

BigBrother sends `lark_cli_bot` notifications through local `lark-cli`, and `config init` is required for that flow. Then set `notifications.feishu.receive_id` to your Feishu-bound email. If you keep the template value `"$FEISHU_NOTIFY_EMAIL"`, export:

```bash
export FEISHU_NOTIFY_EMAIL="you@example.com"
```

4. Start BigBrother:

```bash
cargo build --release
GITHUB_TOKEN="$(gh auth token)" target/release/bigbrother --config bigbrother.toml
```

5. Open [http://127.0.0.1:8787/](http://127.0.0.1:8787/).
