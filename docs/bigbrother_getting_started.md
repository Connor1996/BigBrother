# BigBrother: a local PR agent that stays out of your working tree

Most of the time we lose on pull requests is not the time spent writing the fix itself. It is the time spent checking whether review feedback changed, whether CI is still red, whether a merge conflict just appeared, and whether the last agent run actually finished or silently got stuck somewhere. BigBrother exists for that part of the job. It runs locally, watches the pull requests you authored and the pull requests that currently request your review, and keeps a small dashboard open so you can see what matters without repeatedly context-switching back into GitHub.

The most important thing to understand before trying it is that BigBrother does not edit the checkout you use for normal development. It discovers your local repositories from `workspace.root` and `workspace.repo_map`, but when it needs to act it creates or reuses a dedicated managed worktree under `<workspace.root>/bigbrother-worktrees`. That means your everyday working tree does not get its branch changed underneath you, does not pick up surprise merge commits, and does not become the place where an autonomous agent experiments. When a change looks non-trivial, BigBrother does not try to be clever and push through anyway. It escalates the PR to `needs decision`, records the reason, and waits for you to decide what should happen next.

That safety model is also the easiest way to explain the product to somebody new: BigBrother will find your local repositories, but it will not change code in the working tree you use every day; it only works inside its own managed worktree.

Setup today is still explicit rather than magical. There is not a built-in `doctor`, `init`, or `setup --agent` command yet. What exists today is a configuration file, a local daemon, and a browser dashboard. In practice that is still only a few minutes of work, and the advantage is that you can see every path, token, and safety switch before the daemon starts.

Before you start, make sure the machine already has a recent Rust toolchain, `git`, a GitHub token in `GITHUB_TOKEN` or `GH_TOKEN`, a working `codex` command, and whatever credentials you normally use to push back to your PR branches. BigBrother does not need a separate cloud control plane, but it does assume the machine can already talk to GitHub and can already authenticate the same way you would.

The easiest layout is to keep `symphony-rs` next to the repositories you want BigBrother to discover. If your machine already keeps code under `~/Coding`, cloning `symphony-rs` there makes the default `workspace.root = ".."` work nicely because BigBrother will look for sibling repositories such as `../tikv`, `../tidb`, or `../tidb-cse`. If `symphony-rs` lives somewhere else, set `workspace.root` to an absolute path such as `/Users/alice/Coding`. Avoid `~` and `$HOME/Coding` inside the TOML file for now; the current config loader only understands absolute paths, plain relative paths, or a single environment-variable reference.

From the repository root, start by copying the example config:

```bash
cp symphony-rs.example.toml symphony-rs.toml
```

Then make sure BigBrother can authenticate to GitHub. Export either `GITHUB_TOKEN` or `GH_TOKEN` before launch. If you keep `author = "$GITHUB_USER"` in the config, also export `GITHUB_USER` to your GitHub login. If you would rather not carry that extra environment variable, replace the `author` field with your real login or remove the field entirely and let BigBrother ask GitHub who the token belongs to.

```bash
export GITHUB_TOKEN=...
export GITHUB_USER=...
```

Now open `symphony-rs.toml` and make the file describe your machine rather than the example host. In most cases that means confirming `workspace.root`, adding a `workspace.repo_map` entry only for repositories that do not live at `<workspace.root>/<repo-name>`, and deciding whether the agent should keep the safer default sandboxing or run with `dangerously_bypass_approvals_and_sandbox = true`. The default agent command is `codex`, and the default reasoning effort is `xhigh`, which is a sensible starting point for early pilot users because it reduces the chance of the daemon depending on whatever ambient Codex defaults happen to exist on the host.

If you want Feishu notifications, fill in the `notifications.feishu` section with your bot app credentials and a target such as your email, `open_id`, or a chat ID. The current integration is one-way: BigBrother can send run status outward, but it does not take commands back from Feishu yet. That makes it useful for visibility without turning it into another control surface you have to learn on day one.

Once the config looks right, build the release binary and start the daemon:

```bash
cargo build --release
target/release/symphony-rs --config symphony-rs.toml
```

The dashboard comes up at [http://127.0.0.1:8787/](http://127.0.0.1:8787/). If you are introducing BigBrother to somebody else, that is the moment where it usually becomes real. They can see their authored PRs, the `Review Requests` inbox, and the live `Activity` feed immediately, without having to memorize a long CLI surface. The dashboard is intentionally small, but it already carries the important controls. `Untrack` tells BigBrother to freeze a PR and stop starting automatic runs for it. `Retry` tells it to re-check a failed PR immediately. `Addressed` clears the current `needs decision` blocker after you handled the issue yourself. `Deep Review` is available from the review-request inbox and is read-only by design.

If something goes wrong, the recovery model is simple. To stop BigBrother entirely, stop the `symphony-rs` process just like any other local daemon. To inspect what happened, open the PR detail page from the dashboard; BigBrother stores the live terminal stream for running jobs and keeps the saved terminal recording or output summary for completed ones. To take over a PR yourself, click `Untrack` first so the daemon stands down, then continue the work in a checkout you control. If you are curious where BigBrother was operating, look under `<workspace.root>/bigbrother-worktrees`. Those managed worktrees belong to the daemon and may be rebuilt or reset later, so they are best treated as execution sandboxes, not as your long-lived personal branches.

That is the current BigBrother experience in one sentence: keep the daemon local, keep the dashboard visible, let the agent handle the boring loop when it is safe, and keep the important decisions in front of a human.
