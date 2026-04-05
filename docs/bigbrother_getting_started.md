# BigBrother: a local agent for keeping an eye on your GitHub PRs

When we are working through pull requests, the expensive part is often not the code change itself. It is the repeated checking around the code change.

- Did review feedback change again?
- Did CI finally finish?
- Did the PR pick up a new merge conflict with the base branch?
- Did the last agent run actually finish, or did it fail halfway through?
- Which PR needs me right now, and which one is just waiting?

BigBrother is meant for that layer of work. It runs locally, keeps track of the pull requests you authored and the pull requests that currently request your review, and gives you one small dashboard where you can see what changed without constantly bouncing between GitHub tabs.

It is not an “AI coding tool” in the generic sense, and it is not trying to take over your repository. The better way to think about it is: BigBrother is a local PR operations agent. It watches for review feedback, CI failures, and merge conflicts; it lets the agent handle the safe, mechanical follow-up work; and it stops to ask you when the change is important enough that a human should decide.

## The part that matters most: it stays out of your normal working tree

This is the design choice that makes BigBrother much easier to trust.

BigBrother can discover your local repositories, but it does not edit the checkout you use for day-to-day development. It uses `workspace.root` and `workspace.repo_map` only to find the source repository. When it actually needs to act, it creates or reuses a dedicated managed worktree under `<workspace.root>/bigbrother-worktrees` and runs there instead.

That means:

- it does not silently switch branches in the checkout you are actively using
- it does not merge into your normal working tree behind your back
- it does not treat your everyday repo clone as its scratch space

And there is a second safety boundary on top of that. If the agent decides the change is non-trivial, BigBrother does not try to push through anyway. It escalates the PR to `needs decision`, records the reason, and waits for you to decide what should happen next.

If you only remember one sentence from this document, make it this one: BigBrother will find your local repositories, but it will not change code in the working tree you use every day; it only works inside its own managed worktree.

## What it does today

BigBrother already covers the core loop you usually care about:

- it tracks the open PRs you authored
- it tracks PRs that currently request your review
- it shows states such as `waiting review`, `waiting merge`, `conflict`, `failed`, `needs decision`, and `running`
- it can react to review feedback, CI failures, and merge conflicts
- it can run a read-only `Deep Review` on review-request PRs
- it can send Feishu notifications for runs and escalations when configured

The value is not that it “writes more code.” The value is that it removes a lot of low-value PR babysitting:

- less manually opening PRs just to check status
- less re-checking CI and review threads for tiny changes
- less switching between trivial follow-up work and real product or architecture decisions
- more explicit visibility into the places where a human actually needs to step in

If you usually have several PRs in flight at once, this is where it starts to pay for itself.

## What everyday use looks like

Most of the time, the dashboard is the product.

You leave BigBrother running locally, open [http://127.0.0.1:8787/](http://127.0.0.1:8787/), and use it as the place where PR status gets summarized back to you. When a PR is fine, it simply sits in its current state. When something fails, you can see that quickly instead of discovering it later by accident.

The controls are intentionally small and direct:

- `Untrack` freezes a PR and stops automatic runs for it
- `Retry` re-checks a failed PR immediately
- `Addressed` clears a current `needs decision` blocker after you handled it yourself
- `Deep Review` runs a read-only review pass for a review-request PR

That is the intended interaction model. BigBrother should reduce tab-hopping and background worry, not create a second giant workflow you now have to maintain.

## How to get started today

Setup today is still explicit rather than magical. There is not a built-in `doctor`, `init`, or `setup --agent` command yet. What exists today is a configuration file, a local daemon, and a browser dashboard. The upside is that the first run is transparent: you can see exactly which token, path, and safety switches are in play before anything starts.

Before you begin, make sure the machine already has:

- a recent Rust toolchain
- `git`
- a GitHub token in `GITHUB_TOKEN` or `GH_TOKEN`
- a working `codex` command
- whatever Git credentials you normally use to push back to your PR branches

The smoothest layout is to keep `bigbrother` next to the repositories you want BigBrother to discover. If your machine already keeps code under `~/Coding`, cloning `bigbrother` there makes the default `workspace.root = ".."` work nicely because BigBrother will look for sibling repositories such as `../tikv`, `../tidb`, or `../tidb-cse`. If `bigbrother` lives somewhere else, set `workspace.root` to an absolute path such as `/Users/alice/Coding`. Avoid `~` and `$HOME/Coding` in the TOML file for now; the current config loader does not expand those forms.

From the repository root:

1. Copy the example config.

```bash
cp bigbrother.example.toml bigbrother.toml
```

2. Export the GitHub credentials BigBrother will use.

```bash
export GITHUB_TOKEN=...
export GITHUB_USER=...
```

If you keep `author = "$GITHUB_USER"` in the config, `GITHUB_USER` needs to be set to your GitHub login. If you do not want that extra environment variable, replace the `author` field with your real login or remove the field entirely and let BigBrother resolve the viewer login from GitHub at runtime.

3. Open `bigbrother.toml` and make it describe your machine.

What you usually need to check:

- confirm `workspace.root`
- add `workspace.repo_map` only for repositories that are not located at `<workspace.root>/<repo-name>`
- keep `command = "codex"` unless your machine uses a different agent command
- leave `dangerously_bypass_approvals_and_sandbox` off unless you explicitly want unsandboxed local access on this host

4. If you want Feishu notifications, fill in `notifications.feishu`.

The current Feishu integration is outbound only. BigBrother can send run updates and escalations there, but it does not accept commands back from Feishu yet.

5. Build and start the daemon.

```bash
cargo build --release
target/release/bigbrother --config bigbrother.toml
```

The dashboard should now be available at [http://127.0.0.1:8787/](http://127.0.0.1:8787/).

## If you want an agent to do the setup

We are not at the point of a true one-command setup flow yet, but you can already hand most of the work to Codex. The repo includes a copy-paste setup prompt at [`docs/bigbrother_agent_setup_prompt.md`](/Users/Connor/Coding/bigbrother/docs/bigbrother_agent_setup_prompt.md). It tells Codex to inspect the machine, patch `bigbrother.toml`, avoid unresolved placeholders, wire Feishu only when credentials are available, launch the daemon, and verify that the dashboard responds.

That is the current best approximation of “agent-assisted setup” without pretending the product already has a built-in setup wizard.

## If something goes wrong

The recovery model is intentionally simple.

- To stop BigBrother, stop the `bigbrother` process like any other local daemon.
- To inspect what happened, open the PR detail page from the dashboard and read the saved run output or terminal recording.
- To take over manually, click `Untrack` first so the daemon stands down, then continue the work in a checkout you control.
- To see where BigBrother was operating, look under `<workspace.root>/bigbrother-worktrees`.

Those managed worktrees belong to the daemon and may be rebuilt or reset later, so they are best treated as execution sandboxes, not as long-lived personal branches.

That is the BigBrother pitch in practical terms: keep the daemon local, keep the dashboard open, let the agent handle the boring parts when it is safe, and keep the real decisions in front of a human.
