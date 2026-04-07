# BigBrother：一个帮你盯 GitHub PR 的本地 Agent
# BigBrother: a local agent for keeping an eye on your GitHub PRs

## 中文

现在的 vibe coding 已经越来越接近这样一种体验：从需求到实现、测试、部署，很多事情都可以一路自动往前推。但 BigBrother 不是想替代这段实现流程；它默认你在需求、设计、实现这些环节里，还是会继续和 Codex、Claude Code、subagents、slock.ai 这类 agent 反复来回，把方案和代码一点点打磨出来。

在真正的生产开发里，这段实现工作流通常还是很难完全自动化。很多事情不是一轮就能做对的，往往要经过多轮讨论设计、修改实现、再看结果，才能落到一个足够稳的版本。也正因为这样，BigBrother 并不打算把这段开放式、探索式协作硬收口。

它想接住的是 PR 提出来之后那条更确定、也更容易耗人的长尾流程：反复确认 review、CI、mergeability、comment、failed run，以及“下一个到底该处理哪个 PR”。这类问题更适合一个持续可见的操作界面，而不是单纯的 chat 线程，所以它也不是 OpenClaw 那种界面形态最擅长承接的部分。

## 它能做什么

BigBrother 现在主要做这些事情：


BigBrother 以一个本地 daemon + web dashboard 的形态常驻运行。它会持续轮询你 authored 的 open PR，以及当前 request 你 review 的 PR；一旦发现有需要处理的信号，就先定位本地 source repo，再在 `<workspace.root>/bigbrother-worktrees/<repo-name>-bigbrother` 里创建或复用自己的 managed worktree，把最新的 PR head 和 base branch sync 进去，用 detached HEAD 准备好执行环境，然后才把当前触发原因和 PR 上下文交给 agent。整个 run 的 terminal 输出、结果和最后状态都会回到 dashboard 里；如果配了飞书，关键 completion、failure 和 escalation 也会发出来。

具体到日常场景，它的行为大致是这样的：

- `Review feedback`：当你 authored 的 PR 出现新的 review、inline comment 或普通 comment，而且这些反馈还没被处理过时，BigBrother 会把它当成一个新的 actionable signal。它会拉起一次 agent run 去读反馈、合并最新 base、尝试改动并 push；如果 agent 认为这不是该自动拍板的改动，就会把 PR 升成 `needs decision`。
- `CI failure`：当新的 failing check 或 status 出现时，BigBrother 会把它当成另一类触发信号。它可以让 agent 直接修代码，也可以在判断更像 flaky 或不值得 speculative fix 时选择发 `/retest`，而不是硬改代码。
- `Merge conflict`：当 PR 跟最新 base branch 冲突时，BigBrother 会在自己的 managed worktree 里让 agent 先做合并和冲突解决，而不是碰你的日常工作区。如果同一个冲突状态后面再次重试，它还可以从同一个 worktree 继续接着处理。
- `Review requests`：对当前 request 你 review 的 PR，BigBrother 不会直接帮你改代码；它会把它们放进 `Review Requests` inbox，并允许你手动触发只读的 `Deep Review`。
- `Deep Review`：Deep Review 跑的是只读审查流程。它会生成结构化 review 结果，并在成功后只把最终整理好的 review artifact comment 回 PR，而不是把原始 terminal 噪音直接贴上去。
- `Track / Untrack`：如果你暂时不想让 BigBrother 继续跟某个 PR，可以先 `Untrack`。这样它会把当前状态留在 dashboard 上，但先停止自动跟进和自动 run；等你准备好了，再 `Track` 回来，它就会重新进入正常轮询和处理流程。
- `Failed` 和 `needs decision`：如果 run 失败，PR 会进入 `failed`，后续 scheduled poll 不会无休止重试，要等你点 `Retry`。如果 agent 明确判断这是 non-trivial change，它会进入 `needs decision`，自动 run 会先冻结，等待你在 dashboard 里拍板或点 `Addressed` 继续。

平时你看到的 `waiting review`、`waiting merge`、`conflict`、`failed`、`needs decision`、`running`，本质上就是 BigBrother 把这些不同阶段的 PR 长尾动作摊平成一个持续可见的操作面板。

## 建议贴图位置

### 贴图 1：主页 dashboard

建议放一张首页截图，最好同时带上：

- authored PR 列表
- `Review Requests` 和 `Activity` tab
- 几个不同状态的 PR，比如 `waiting review`、`failed`、`needs decision`

这里最适合说明的点是：BigBrother 把“我现在到底该看哪个 PR”这件事变成一个可以一眼扫过的问题。

> Screenshot placeholder: Home dashboard with PR list, Review Requests, Activity tabs, and mixed PR states.

### 贴图 2：PR details 里的 terminal 输出

建议放一张 details 页面截图，重点展示：

- terminal 输出
- 最近一次 run 的结果
- 这是一个可以直接看到 agent 做了什么、跑到哪里、为什么停下来的页面

这张图最适合承接 “不是只看状态，还能往下看执行细节”。

> Screenshot placeholder: PR details page with saved or live terminal output.

### 贴图 3：agent 自己判断后去 `/retest`

建议放一张能同时说明“它不是无脑改代码”的截图。理想情况是：

- CI fail 了
- agent 看完之后判断这是 flaky 或不值得直接改代码
- 最终自动发出 `/retest` comment

这张图适合说明 BigBrother 不是所有问题都直接改代码，它会先判断什么动作最合理。

> Screenshot placeholder: PR timeline or comment thread showing automatic `/retest` after agent inspection.

### 贴图 4：Deep Review 和自动回复 comment

建议放一张 review-request PR 的截图，展示：

- `Deep Review` 是怎么触发的
- 它最后会把整理好的 review 结果贴回 PR comment
- comment 不是原始 terminal 噪音，而是整理过的 review 结果

这张图最适合说明 BigBrother 不只是盯 authored PR，也能帮助你处理 review-request PR。

> Screenshot placeholder: Deep Review flow with resulting PR comment posted automatically.

## 怎么开始

第一次上手前，先确认这些东西已经有：

- 机器上已经有可用的 Rust toolchain 和 `git`
- 已经安装并登录 `gh`
- 本机能直接调用 `codex` 命令
- Git 凭据可以 push 回你自己的 PR branch
- 你想让 BigBrother 管理的仓库已经在本地，默认放在 `workspace.root` 下能被发现
- 如果你想接飞书通知，可选安装并登录 `lark-cli`

推荐先让 agent 帮你做第一轮 setup：

1. 打开这份 prompt：

[`docs/bigbrother_agent_setup_prompt.md`](/Users/Connor/Coding/bigbrother/docs/bigbrother_agent_setup_prompt.md)

2. 把它交给 Codex。
3. 它会检查环境、补 `bigbrother.toml`、按需接上飞书、启动 daemon，并确认 dashboard 可以打开。

这就是现在最接近 `agent-assisted setup` 的方式。

手动 setup 也很直接：

1. 复制配置模板。

```bash
cp bigbrother.example.toml bigbrother.toml
```

2. 打开 `bigbrother.toml`，确认这两项：

- `workspace.root` 对不对
- 只有在 `<workspace.root>/<repo-name>` 找不到仓库时，才加 `workspace.repo_map`

3. 如果你需要飞书通知，可选安装并登录 `lark-cli`，再补 `notifications.feishu`。

4. 启动：

```bash
cargo build --release
GITHUB_TOKEN="$(gh auth token)" target/release/bigbrother --config bigbrother.toml
```

如果你已经自己管理好了 `GITHUB_TOKEN` 或 `GH_TOKEN`，也可以继续沿用现有环境变量。

5. 打开 [http://127.0.0.1:8787/](http://127.0.0.1:8787/)。

---

## English

Vibe coding is getting closer and closer to an experience where a lot of the path from requirement to implementation to testing to deployment can move forward automatically. BigBrother is not trying to replace that implementation loop. It assumes that during requirements, design, and implementation, people will still keep iterating with tools such as Codex, Claude Code, subagents, or agent-team setups like slock.ai until the design and code are actually good enough.

In real production software work, that implementation loop is still hard to automate cleanly from end to end. Many tasks are not solved in one pass; they need multiple design discussions, several implementation revisions, and repeated back-and-forth with an agent before the result is solid. That is why BigBrother is not trying to force the open-ended, exploratory phase into one standardized workflow.

It is built for the more predictable long tail that starts after a PR is already open: checking review state, CI, mergeability, comments, failed runs, and simply knowing which PR needs attention next. That problem is better served by a persistent operational surface than by a chat thread, which is also why it is not really the part of the workflow that an OpenClaw-style interface handles best.

## What it does

BigBrother runs as a local daemon plus web dashboard. It keeps polling the open PRs you authored as well as the PRs that currently request your review; when it sees a signal that needs action, it first resolves the local source repository, then creates or reuses its own managed worktree under `<workspace.root>/bigbrother-worktrees/<repo-name>-bigbrother`, syncs the latest PR head and base branch into that detached-HEAD workspace, and only then hands the current trigger plus PR context to the agent. The terminal output, run result, and latest status all flow back into the dashboard, and if Feishu is configured, the important completions, failures, and escalations can be pushed there too.

In day-to-day use, its behavior is roughly this:

- `Review feedback`: when an authored PR gets a new review, inline comment, or top-level comment that has not been handled yet, BigBrother treats that as a new actionable signal. It starts an agent run to read the feedback, merge the latest base, attempt the change, and push if it can; if the agent decides the change is not something it should land on its own, the PR moves to `needs decision`.
- `CI failure`: when a new failing check or status appears, BigBrother treats that as a separate trigger. It can ask the agent to fix code directly, but it can also choose to post `/retest` when the failure looks flaky or not worth a speculative code change.
- `Merge conflict`: when the PR no longer merges cleanly with the latest base branch, BigBrother asks the agent to merge and resolve conflicts inside its managed worktree instead of touching your everyday checkout. If you retry the same unresolved conflict later, it can resume from that same workspace.
- `Review requests`: for PRs that currently request your review, BigBrother does not try to edit code on your behalf. Instead, it keeps them in the `Review Requests` inbox and lets you manually trigger a read-only `Deep Review`.
- `Deep Review`: Deep Review is a read-only review pass. It produces a structured review result and, on success, posts only the final cleaned-up review artifact back to the PR instead of dumping raw terminal output into a comment.
- `Track / Untrack`: if you do not want BigBrother actively handling a PR for a while, you can `Untrack` it. The current dashboard snapshot stays visible, but automatic follow-up and new runs stop; when you `Track` it again, it goes back into the normal polling and handling flow.
- `Failed` and `needs decision`: if a run fails, the PR moves to `failed`, and scheduled polls do not keep retrying forever; it waits for you to click `Retry`. If the agent explicitly decides the change is non-trivial, the PR moves to `needs decision`, automatic handling freezes, and the dashboard waits for you to make the call or mark it `Addressed`.

The statuses you see in practice, such as `waiting review`, `waiting merge`, `conflict`, `failed`, `needs decision`, and `running`, are basically BigBrother flattening all of those long-tail PR situations into one persistent operational surface.

## Suggested screenshot slots

### Screenshot 1: home dashboard

This is the best place for a homepage screenshot that includes:

- the authored PR list
- the `Review Requests` and `Activity` tabs
- a few PRs in different states, such as `waiting review`, `failed`, and `needs decision`

This screenshot should support the core idea that BigBrother turns “which PR actually needs me right now?” into something you can scan at a glance.

> Screenshot placeholder: Home dashboard with PR list, Review Requests, Activity tabs, and mixed PR states.

### Screenshot 2: PR details with terminal output

This is the place for a detail-page screenshot that shows:

- terminal output
- the latest run result
- a concrete view of what the agent actually did, where it stopped, and why

This screenshot supports the idea that BigBrother is not only a status board; it also lets you inspect execution details when you need them.

> Screenshot placeholder: PR details page with saved or live terminal output.

### Screenshot 3: agent deciding to `/retest`

This is the best place for a screenshot that shows BigBrother is not blindly editing code. Ideally it captures:

- a failed CI run
- the agent deciding that the failure looks flaky or not worth changing code for directly
- an automatic `/retest` comment posted back to the PR

This screenshot supports the idea that BigBrother first chooses the right action, instead of assuming every problem should become a code change.

> Screenshot placeholder: PR timeline or comment thread showing automatic `/retest` after agent inspection.

### Screenshot 4: Deep Review and automatic comment reply

This is the best place for a review-request PR screenshot that shows:

- how `Deep Review` is triggered
- that the final review result is posted back as a PR comment
- that the comment is a cleaned-up review artifact rather than raw terminal noise

This screenshot supports the idea that BigBrother is useful not only for authored PRs, but also for PRs where you are the reviewer.

> Screenshot placeholder: Deep Review flow with resulting PR comment posted automatically.

## How to get started

Before the first run, make sure you already have:

- a working Rust toolchain and `git`
- `gh` is installed and already authenticated
- a working `codex` command on the machine
- Git credentials that can push back to your PR branches
- local checkouts of the repositories you want BigBrother to manage, discoverable from `workspace.root`
- optional `lark-cli` installation and login if you want Feishu notifications

The recommended path is to let an agent do the first setup pass:

1. Open this prompt:

[`docs/bigbrother_agent_setup_prompt.md`](/Users/Connor/Coding/bigbrother/docs/bigbrother_agent_setup_prompt.md)

2. Hand it to Codex.
3. It will inspect the machine, patch `bigbrother.toml`, wire Feishu if needed, start the daemon, and verify that the dashboard responds.

That is the current best approximation of `agent-assisted setup`.

Manual setup is straightforward:

1. Copy the config template.

```bash
cp bigbrother.example.toml bigbrother.toml
```

2. Open `bigbrother.toml` and confirm the important settings:

- confirm `workspace.root`
- add `workspace.repo_map` only for repositories that are not located at `<workspace.root>/<repo-name>`
- keep `command = "codex"` unless your machine clearly uses another agent command
- if the template still contains `author = "$GITHUB_USER"`, replace it with your real GitHub login or remove the field entirely
- leave `dangerously_bypass_approvals_and_sandbox` off unless you explicitly want unsandboxed local access on this host

3. If you want Feishu notifications, optionally install and log into `lark-cli`, then fill in `notifications.feishu`.

4. Build and start the daemon.

```bash
cargo build --release
GITHUB_TOKEN="$(gh auth token)" target/release/bigbrother --config bigbrother.toml
```

If you already manage `GITHUB_TOKEN` or `GH_TOKEN` yourself, you can keep using that existing environment variable setup.

5. Open [http://127.0.0.1:8787/](http://127.0.0.1:8787/).

If your repositories already live under `~/Coding`, putting `bigbrother` next to them is usually the simplest layout. Otherwise, set `workspace.root` to an absolute path such as `/Users/alice/Coding`. The current config loader does not expand `~` or `$HOME/Coding`.
