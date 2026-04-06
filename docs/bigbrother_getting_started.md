# BigBrother：一个帮你盯 GitHub PR 的本地 Agent
# BigBrother: a local agent for keeping an eye on your GitHub PRs

## 中文

现在在所谓的 vibe coding 阶段里，从需求到实现、测试、部署，端到端自动化已经越来越可行了。而且在实现阶段，大家已经有很多和 agent 协作的方式可选：可以直接用 Codex、Claude Code 这样的 coding agent，可以拉起 subagents 去并行做研究、评审和实现，也可以用 slock.ai 这类 agent teams 形态去组织多人类、多 agent 的协作。

但在真正生产级别的软件开发里，这段工作流通常还很难完全自动化。实际做下来也很容易发现，很多事情不是一轮就能做对的，我们还是需要和 agent 多轮来回讨论设计、反复修改实现，才能把东西打磨到足够好。也正因为这样，BigBrother 并不是想把实现阶段这段开放式、探索式的工作流统一收口。

BigBrother 想解决的是另一段问题：PR 一旦提出来之后，那条很长、但流程其实相对确定的尾巴。真正拖时间的，往往不是“改代码”本身，而是反复确认 review、CI、mergeability、comment、failed run 和接下来该轮到哪个 PR。这个问题更适合一个持续可见的操作界面，而不是单纯的 chat 线程，所以它也不是 OpenClaw 那种界面形态最擅长承接的部分。

## 它能做什么

BigBrother 现在主要做这些事情：

- 跟踪你 authored 的 open PR
- 跟踪当前 request 你 review 的 PR
- 展示 `waiting review`、`waiting merge`、`conflict`、`failed`、`needs decision`、`running` 等状态
- 在 review feedback、CI failure 和 merge conflict 出现时判断是否要自动处理
- 对 review-request PR 发起只读的 `Deep Review`
- 在配置好时通过飞书把 run 和 escalation 通知发出来

它最有价值的地方，并不是“自动写了多少代码”，而是减少了很多低价值的 PR babysitting 动作：

- 少手动点开 PR 只为确认状态
- 少来回刷新 review 和 CI
- 少在 trivial fix 和重要决策之间频繁切换心智
- 把真正需要人工介入的点明确抬出来

## 它实际怎么工作

BigBrother 不是直接替你接管实现流程，而是接在 PR 已经存在之后的运营环节上。

它大致这样工作：

1. 先持续跟踪你 authored 的 PR 和 request 你 review 的 PR。
2. 如果 review feedback、CI 或 merge 状态发生变化，它会判断这是不是一个值得处理的新信号。
3. 如果这个问题看起来可以安全自动处理，它会进入自己的 managed worktree，而不是直接改你平时开发用的工作区。
4. 然后它调用配置好的 agent 去处理问题，必要时合并最新 base、修冲突、做验证、再把结果推回 PR。
5. 如果 agent 判断这不是 trivial follow-up，而是 non-trivial change，就不会继续硬改，而是把 PR 升成 `needs decision`，等你拍板。
6. 对于 request 你 review 的 PR，它还可以跑只读的 `Deep Review`，帮助你快速形成 review 意见。

这里最值得强调的一点是：BigBrother 会发现你的本地仓库，但不会直接在你日常使用的工作区里改代码；它只在自己的专用 managed worktree 里工作。

## 整体流程大概是什么样

从日常使用者的角度看，这个流程通常会长这样：

1. 你照常写代码、开 PR，真正的设计和实现打磨还是在你熟悉的 agent workflow 里完成。
2. PR 打开之后，BigBrother 开始接管那条长尾运营流程。
3. 大多数时候你只需要开着 dashboard，看哪些 PR 在 `waiting review`，哪些已经 `waiting merge`，哪些因为 comment、CI 或冲突变成了新的 action item。
4. 如果自动处理失败，PR 会进入 `failed`，你可以决定是否 `Retry`。
5. 如果 agent 判断改动不应该自动做，PR 会进入 `needs decision`，等你来拍板。
6. 如果你暂时不想让它继续处理某个 PR，可以 `Untrack`。
7. 如果你自己已经处理掉当前阻塞，可以用 `Addressed` 把状态往前推进。

这也是为什么 BigBrother 更像一个 PR operations console，而不是一个聊天产品。它的重点不是“再开一轮对话”，而是把一组持续变化的 PR 状态稳定地摆在你面前。

## 怎么开始

最省事的方式，不是手动从头填配置，而是先让 agent 帮你做 setup。

仓库里已经有一份可以直接复制使用的 prompt：

[`docs/bigbrother_agent_setup_prompt.md`](/Users/Connor/Coding/bigbrother/docs/bigbrother_agent_setup_prompt.md)

把它交给 Codex 之后，它会去检查本机环境、修 `bigbrother.toml`、避免保留没展开的占位符、在有凭据时接上飞书、启动 daemon，并确认 dashboard 能否正常响应。

这就是当前版本最接近 “agent-assisted setup” 的方式。现在还没有真正内建的 `doctor`、`init` 或 `setup --agent` 命令，所以最顺的上手方式就是先让 agent 帮你把第一轮 setup 走完。

如果你还是想手动来，流程也不复杂：

1. 先准备好 Rust、`git`、`GITHUB_TOKEN` 或 `GH_TOKEN`、可用的 `codex` 命令，以及能正常 push 回自己 PR branch 的 Git 凭据。
2. 复制配置模板。

```bash
cp bigbrother.example.toml bigbrother.toml
```

3. 准备 GitHub 凭据。

```bash
export GITHUB_TOKEN=...
export GITHUB_USER=...
```

如果你保留了 `author = "$GITHUB_USER"`，那 `GITHUB_USER` 需要是你的 GitHub login。要是不想额外带这个环境变量，也可以直接把 `author` 改成真实用户名，或者删掉，让 BigBrother 在运行时自己去 GitHub 查当前 viewer。

4. 打开 `bigbrother.toml`，确认这些关键项：

- `workspace.root` 对不对
- 只有在 `<workspace.root>/<repo-name>` 找不到仓库时，才加 `workspace.repo_map`
- 除非你的机器明确用的是别的 agent，否则保留 `command = "codex"`
- 除非你明确要让 Codex 在本机无沙箱全权限运行，否则先不要打开 `dangerously_bypass_approvals_and_sandbox`

5. 如果你需要飞书通知，再补 `notifications.feishu`。

6. 编译并启动 daemon。

```bash
cargo build --release
target/release/bigbrother --config bigbrother.toml
```

7. 打开 [http://127.0.0.1:8787/](http://127.0.0.1:8787/)。

如果你的 repo 都放在 `~/Coding` 下面，把 `bigbrother` 放在这些 repo 旁边通常会最顺手，因为默认的 `workspace.root = ".."` 就能自动发现很多 sibling repo。要是目录布局不一样，就把 `workspace.root` 改成绝对路径，比如 `/Users/alice/Coding`。当前配置解析不会自动展开 `~` 或 `$HOME/Coding`。

---

## English

In what people now call vibe coding, it is already becoming realistic to automate the whole path from requirement to implementation to testing to deployment. And during implementation, developers already have many ways to work with agents: coding agents such as Codex and Claude Code, subagent patterns for parallel research, review, and implementation, and agent-team setups such as slock.ai.

But in real production-grade software work, that workflow is still much harder to automate end to end. In practice, many tasks still need repeated back-and-forth with an agent, multiple design discussions, and several implementation passes before the result is actually good enough. That is exactly why BigBrother is not trying to standardize the open-ended, exploratory implementation phase.

It is built for a different part of the problem: the long tail that starts after a PR is already open. At that point, the expensive part is often not the code change itself. It is the repeated checking around review state, CI, mergeability, comments, failed runs, and which PR actually needs attention next. That problem is better served by a persistent operational surface than by a chat thread, which is also why it is not really the part of the workflow that an OpenClaw-style interface handles best.

## What it does

Today, BigBrother mainly does a few things:

- it tracks the open PRs you authored
- it tracks PRs that currently request your review
- it shows states such as `waiting review`, `waiting merge`, `conflict`, `failed`, `needs decision`, and `running`
- it decides whether new review feedback, CI failures, or merge conflicts should trigger automatic handling
- it can run a read-only `Deep Review` on review-request PRs
- it can send Feishu notifications for runs and escalations when configured

The real value is not that it writes more code. The value is that it removes a lot of low-value PR babysitting:

- less manually opening PRs just to confirm status
- less re-checking review threads and CI for small changes
- less context switching between trivial follow-up work and important human decisions
- more explicit visibility into the exact places where a human actually needs to step in

## How it behaves in practice

BigBrother does not try to take over the implementation phase. It picks up once the PR already exists and the long-tail operational loop begins.

In practice, it works roughly like this:

1. It continuously tracks the PRs you authored and the PRs that currently request your review.
2. When review feedback, CI, or merge state changes, it decides whether this is a new actionable signal.
3. If the issue looks safe to handle automatically, it enters its own managed worktree instead of editing your everyday working tree.
4. It then invokes the configured agent, merging the latest base, resolving conflicts if needed, validating the result, and pushing the outcome back to the PR when appropriate.
5. If the agent decides the change is not a trivial follow-up, it does not force the change through. It escalates the PR to `needs decision` and waits for a human call.
6. For PRs that request your review, it can also run a read-only `Deep Review` to help you form review feedback faster.

The most important sentence in the whole model is this one: BigBrother will find your local repositories, but it will not change code in the working tree you use every day; it only works inside its own managed worktree.

## What the overall flow looks like

From the operator's point of view, the lifecycle usually looks like this:

1. You still do the real design and implementation work in whatever agent workflow fits you best.
2. Once the PR is open, BigBrother takes over the long-tail operational loop around that PR.
3. Most of the time, you simply leave the dashboard open and see which PRs are in `waiting review`, which ones are already `waiting merge`, and which ones turned into new action items because of comments, CI, or conflicts.
4. If an automatic attempt fails, the PR lands in `failed`, and you decide whether to `Retry`.
5. If the agent decides the change should not be automated, the PR lands in `needs decision`, and you decide what happens next.
6. If you do not want BigBrother to keep working on a PR for now, you can `Untrack` it.
7. If you handled the blocker yourself, you can use `Addressed` to move the state forward again.

That is also why BigBrother feels more like a PR operations console than a chat product. Its job is not to start another conversation. Its job is to keep a changing set of PR states visible and operable.

## How to get started

The easiest way to start is not to hand-edit everything yourself. It is to let an agent do the first setup pass.

The repository already includes a copy-paste setup prompt here:

[`docs/bigbrother_agent_setup_prompt.md`](/Users/Connor/Coding/bigbrother/docs/bigbrother_agent_setup_prompt.md)

If you hand that prompt to Codex, it will inspect the machine, patch `bigbrother.toml`, avoid unresolved placeholders, wire Feishu when credentials are available, launch the daemon, and verify that the dashboard responds.

That is the current best approximation of agent-assisted setup. There is not yet a built-in `doctor`, `init`, or `setup --agent` command, so the smoothest first-run path today is to let the agent help you bootstrap the environment.

If you still want to set it up manually, the flow is straightforward:

1. Make sure the machine already has Rust, `git`, a GitHub token in `GITHUB_TOKEN` or `GH_TOKEN`, a working `codex` command, and Git credentials that can push back to your PR branches.
2. Copy the config template.

```bash
cp bigbrother.example.toml bigbrother.toml
```

3. Export the GitHub credentials BigBrother should use.

```bash
export GITHUB_TOKEN=...
export GITHUB_USER=...
```

If you keep `author = "$GITHUB_USER"` in the config, `GITHUB_USER` needs to be set to your GitHub login. If you do not want that extra environment variable, replace the `author` field with your real login or remove it entirely and let BigBrother resolve the viewer login at runtime.

4. Open `bigbrother.toml` and confirm the important settings:

- confirm `workspace.root`
- add `workspace.repo_map` only for repositories that are not located at `<workspace.root>/<repo-name>`
- keep `command = "codex"` unless your machine clearly uses another agent command
- leave `dangerously_bypass_approvals_and_sandbox` off unless you explicitly want unsandboxed local access on this host

5. If you want Feishu notifications, fill in `notifications.feishu`.

6. Build and start the daemon.

```bash
cargo build --release
target/release/bigbrother --config bigbrother.toml
```

7. Open [http://127.0.0.1:8787/](http://127.0.0.1:8787/).

If your repositories already live under `~/Coding`, putting `bigbrother` next to them is usually the smoothest layout, because the default `workspace.root = ".."` can then discover many sibling repositories automatically. If your layout is different, set `workspace.root` to an absolute path such as `/Users/alice/Coding`. The current config loader does not expand `~` or `$HOME/Coding`.
