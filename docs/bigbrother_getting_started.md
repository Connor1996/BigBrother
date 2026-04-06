# BigBrother：一个帮你盯 GitHub PR 的本地 Agent
# BigBrother: a local agent for keeping an eye on your GitHub PRs

## 中文

现在在 web coding 这类场景里，从需求、实现、测试到部署，端到端自动化已经越来越可行了。但在真正生产级别的软件开发里，我们的工作流通常还很难完全自动化。在实际个人实践里也能明显感觉到，很多事情不是一轮就能做对的，我们往往还是需要和 agent 多轮来回讨论设计、反复修改实现，才能把东西打磨到足够好。

也正因为这样，实现阶段今天本来就会有很多不同做法。有人会用 Slack AI，有人会用 agent team，有人会用 Codex 配 cloud subagents，也有人会用自己更顺手的别的组合。BigBrother 并不是想把这一段开放式、探索式的工作流统一收口。

BigBrother 想解决的是另一段问题：PR 一旦提出来之后，那条很长、但流程其实相对确定的尾巴。真正拖时间的，往往不是“改代码”本身，而是围着代码变化不断确认上下文。

- review 有没有新反馈
- CI 是不是终于跑完了
- base branch 又有没有引入新的冲突
- 上一次 agent 到底跑到了哪一步，为什么停了
- 现在最该处理的是哪一个 PR

BigBrother 就是为这层工作做的。它在本地运行，持续跟踪你 authored 的 PR，以及当前 request 你 review 的 PR，再用一个很轻的 dashboard 把状态集中展示出来。这样你不用反复在 GitHub tab 之间来回切，也不用靠记忆维持“哪些 PR 还在等我”。

它也不是那种泛泛的“AI coding tool”。更贴切的说法是：BigBrother 是一个本地的 PR 运营 agent。它看 review feedback、CI failure 和 merge conflict；能安全自动处理的，就让 agent 去做；碰到 non-trivial change，就明确停下来，升级成 `needs decision`，把决定权交还给人。

## 为什么它比较容易让人敢用

这里最重要的一点，不是“它会不会改代码”，而是“它在哪改代码”。

BigBrother 会发现你的本地仓库，但不会直接改你平时开发用的工作区。`workspace.root` 和 `workspace.repo_map` 只负责帮它找到来源仓库；真正执行 agent 修改时，它会进入 `<workspace.root>/bigbrother-worktrees` 下面的专用 managed worktree。

这意味着：

- 它不会在你正在使用的 checkout 里偷偷切 branch
- 它不会在你的日常工作区里突然 merge 一些东西
- 它不会把你平时开发用的 repo 当成 agent 的实验场

而且它还有第二层安全边界。只要 agent 判断这次改动不属于 trivial follow-up，而是涉及 API 行为、较大重构、重要产品取舍，或者任何应该由人拍板的变化，BigBrother 就不会继续“硬做下去”。它会把 PR 升成 `needs decision`，把理由写清楚，等你决定下一步。

如果要用一句最适合对外解释的话，那就是：

`BigBrother 会发现你的本地仓库，但不会直接在你日常使用的工作区里改代码；它只在自己的专用 worktree 里工作。`

## 为什么不直接用 OpenClaw

这点也值得讲清楚，因为它不是“谁更智能”的问题，而是“谁更适合做 PR 运营界面”的问题。

OpenClaw 或一般 chat 形态，更适合一次性的对话式协作：你带着一个问题进去，聊完一个问题，再进入下一个问题。这个交互对于写代码、问问题、做一次性分析很自然，但它不是一个特别直观的 PR 管理界面。

PR 运营这件事需要的其实不是连续聊天，而是持续可见的状态面板。你需要同时看到多个 PR 的当前状态，需要一眼分清哪些在 `waiting review`、哪些在 `failed`、哪些在 `needs decision`，还需要很快做 `Retry`、`Untrack`、`Addressed` 这种操作。把这些动作都塞进 chat 线程里，会出现几个问题：

- 它不够直观，多个 PR 放在聊天上下文里不容易扫一眼看清
- 它不够高效，很多时候你只是想确认状态，不是真的想再开一轮对话
- 它不够灵活，像 `Track / Untrack`、失败重试、review inbox、activity feed 这种东西，本质上更像控制台，而不是聊天消息

所以 BigBrother 不是在和 OpenClaw 做“谁替代谁”的竞争。更准确地说，OpenClaw 是 chat 界面，BigBrother 是操作界面。前者适合对话，后者适合持续管理一个 PR 队列。对于 PR 这种需要长时间跟踪、反复回看、快速操作的工作，dashboard 会比 chat 更自然。

## 它今天已经能做什么

BigBrother 已经覆盖了你平时最常碰到的那一圈事情：

- 跟踪你 authored 的 open PR
- 跟踪 request 你 review 的 PR
- 展示 `waiting review`、`waiting merge`、`conflict`、`failed`、`needs decision`、`running` 这些状态
- 在 review feedback、CI failure 和 merge conflict 出现时决定是否要自动处理
- 对 review-request PR 发起只读的 `Deep Review`
- 在配置好时通过飞书把 run 和 escalation 通知发出来

它真正的价值，并不是“自动写了多少代码”，而是把很多低价值的 PR babysitting 动作拿掉：

- 少手动点开 PR 只为确认状态
- 少来回刷新 review 和 CI
- 少在 trivial fix 和重要决策之间频繁切换心智
- 把真正需要人工介入的点明确抬出来

如果你平时经常同时挂着几个 PR，这种收益会很明显。

## 日常使用会是什么感觉

大多数时候，dashboard 本身就是产品。

你让 BigBrother 在本地跑着，打开 [http://127.0.0.1:8787/](http://127.0.0.1:8787/)，把它当成 PR 状态汇总面板来用。PR 没事的时候，它就安静地停在当前状态；有事的时候，你会比“偶然想起来再去 GitHub 看一眼”更早看到它。

操作也刻意做得很直接：

- `Untrack`：暂时冻结一个 PR，不让它继续自动跑
- `Retry`：对失败项立刻再检查一次
- `Addressed`：你已经自己处理了 `needs decision` 里的事情，可以清掉当前阻塞
- `Deep Review`：对 request 你 review 的 PR 跑一次只读审查

这才是 BigBrother 适合存在的地方。它应该帮你减少切换和背景焦虑，而不是再制造一个你得持续维护的新聊天流。

## 怎么开始

现在的 setup 还不是“一条命令全自动”那种形态。还没有内建的 `doctor`、`init` 或 `setup --agent`。当前真实存在的是：配置文件、本地 daemon、浏览器 dashboard。好处是第一轮 setup 很透明，你能清楚看到用的是哪个 token、哪些 repo、哪些安全开关。

开始前，你的机器上最好已经有：

- 较新的 Rust toolchain
- `git`
- `GITHUB_TOKEN` 或 `GH_TOKEN`
- 可用的 `codex` 命令
- 能正常 push 回自己 PR branch 的 Git 凭据

最顺手的目录布局，是把 `bigbrother` 放在你常用 repo 的旁边。如果你的代码都在 `~/Coding` 下，把这个 repo clone 到那里时，默认的 `workspace.root = ".."` 通常就够用了，因为它会自动去找像 `../tikv`、`../tidb`、`../tidb-cse` 这样的 sibling repo。要是你的目录布局不是这样，就把 `workspace.root` 改成绝对路径，比如 `/Users/alice/Coding`。目前不要在 TOML 里写 `~` 或 `$HOME/Coding`，当前的配置解析不会自动展开它们。

第一次启动的步骤是：

1. 复制配置模板。

```bash
cp bigbrother.example.toml bigbrother.toml
```

2. 准备 GitHub 凭据。

```bash
export GITHUB_TOKEN=...
export GITHUB_USER=...
```

如果你保留了 `author = "$GITHUB_USER"` 这一行，那 `GITHUB_USER` 需要是你的 GitHub login。要是不想额外带这个环境变量，也可以直接把 `author` 改成真实用户名，或者干脆删掉，让 BigBrother 在运行时自己去 GitHub 查当前 viewer。

3. 打开 `bigbrother.toml`，把它改成符合你机器环境的配置。

通常你需要确认的是：

- `workspace.root` 对不对
- 只有在 `<workspace.root>/<repo-name>` 找不到仓库时，才加 `workspace.repo_map`
- 除非你的机器明确用的是别的 agent，否则保留 `command = "codex"`
- 除非你明确要让 Codex 在本机无沙箱全权限运行，否则先不要打开 `dangerously_bypass_approvals_and_sandbox`

4. 如果你需要飞书通知，再补 `notifications.feishu`。

当前飞书集成是单向的，只负责把 run 和 escalation 发出去，还不能从飞书反向控制 BigBrother。

5. 编译并启动 daemon。

```bash
cargo build --release
target/release/bigbrother --config bigbrother.toml
```

然后打开 [http://127.0.0.1:8787/](http://127.0.0.1:8787/)。

## 如果你想让 agent 帮你完成 setup

虽然现在还没有真正的“一键 setup 命令”，但已经可以把大部分 setup 工作交给 Codex。仓库里已经有一份可以直接复制使用的 prompt：

[`docs/bigbrother_agent_setup_prompt.md`](/Users/Connor/Coding/bigbrother/docs/bigbrother_agent_setup_prompt.md)

它会引导 Codex 去检查本机环境、修 `bigbrother.toml`、避免保留没展开的占位符、在有凭据时接上飞书、启动 daemon，并确认 dashboard 能否正常响应。

这就是当前版本最接近 “agent-assisted setup” 的方式，而且不会把未来还没实现的命令硬写成现在已经存在。

## 如果出问题了怎么办

恢复模型尽量保持简单：

- 要停掉 BigBrother，就像停普通本地 daemon 一样停掉 `bigbrother` 进程
- 要看刚才发生了什么，就去 dashboard 的 PR detail 页看保存下来的输出和 terminal recording
- 要自己接管，就先点 `Untrack`，让 daemon 先站到一边
- 要看 BigBrother 刚才到底在哪个目录里做事，就看 `<workspace.root>/bigbrother-worktrees`

这些 managed worktree 是 daemon 的执行沙箱，不适合拿来当长期个人分支。它们后面可能会被重建、重置，最好把它们理解成 BigBrother 的运行空间，而不是你的开发空间。

如果要用一句话总结 BigBrother 的定位，那就是：让 daemon 留在本地，让 dashboard 保持可见，让 agent 处理那些安全又机械的事情，把真正重要的决定继续留给人。

---

## English

In some web-coding workflows today, it is already becoming realistic to automate the whole path from requirement to implementation to testing to deployment. But in real production-grade software work, our workflow is usually much harder to automate end to end. In practice, many tasks still need repeated back-and-forth with an agent, multiple design discussions, and several implementation passes before the result is actually good enough.

That is also why the implementation phase is still naturally diverse. Some people use Slack AI, some use agent teams, some use Codex with cloud subagents, and others use whatever combination fits their habits best. BigBrother is not trying to standardize that open-ended, exploratory part of the workflow.

It is built for a different part of the problem: the long tail that starts after a PR is already open. At that point, the expensive part is often not the code change itself. It is the repeated checking that surrounds the code change.

- Did review feedback change again?
- Did CI finally finish?
- Did the PR pick up a new merge conflict with the base branch?
- Did the last agent run actually finish, or did it fail halfway through?
- Which PR needs me right now, and which one is just waiting?

BigBrother is built for that layer of work. It runs locally, tracks the pull requests you authored and the pull requests that currently request your review, and gives you one lightweight dashboard where that state is visible without constant tab-hopping.

It is also not best described as a generic AI coding tool. A better description is: BigBrother is a local PR operations agent. It watches review feedback, CI failures, and merge conflicts; lets the agent handle the safe, mechanical follow-up work; and stops to ask for human judgment when the change is important enough to deserve it.

## Why it is easier to trust

The key question is not just whether it changes code, but where it changes code.

BigBrother can discover your local repositories, but it does not edit the checkout you use for day-to-day development. `workspace.root` and `workspace.repo_map` are only used to locate the source repository. When BigBrother actually needs to act, it creates or reuses a dedicated managed worktree under `<workspace.root>/bigbrother-worktrees` and works there instead.

That means:

- it does not silently switch branches in the checkout you are actively using
- it does not merge into your normal working tree behind your back
- it does not turn your everyday clone into the agent's scratch space

There is also a second safety boundary on top of that. If the agent decides the change is non-trivial, BigBrother does not try to force the change through anyway. It escalates the PR to `needs decision`, records the reason, and waits for you to decide what should happen next.

If you only remember one sentence from this document, make it this one:

`BigBrother will find your local repositories, but it will not change code in the working tree you use every day; it only works inside its own managed worktree.`

## Why not just use OpenClaw

This is worth stating directly, because the difference is not really about who is more intelligent. It is about which interface is better suited to PR operations.

OpenClaw, or any chat-shaped interface, is strong at one-off conversational collaboration. You bring one problem into a thread, work through it, and then move to the next problem. That interaction is natural for coding help, analysis, and question answering. It is much less natural as an ongoing PR management surface.

PR operations needs a persistent control surface more than it needs a continuous chat. You need to see multiple PRs at once, distinguish `waiting review` from `failed` from `needs decision` at a glance, and perform actions such as `Retry`, `Untrack`, and `Addressed` quickly. Once all of that is pushed into a chat thread, a few things get worse:

- it is less intuitive, because multiple PRs do not scan well inside a linear conversation
- it is less efficient, because many checks are status checks rather than conversations
- it is less flexible, because things like `Track / Untrack`, retries, review inboxes, and activity feeds behave more like an operations console than a chat log

So BigBrother is not really trying to replace OpenClaw. OpenClaw is a chat interface. BigBrother is an operational interface. The former is good for dialogue; the latter is good for continuously managing a queue of PRs. For work that needs long-lived visibility, repeated revisits, and fast control actions, a dashboard is simply a more natural shape than chat.

## What it already does today

BigBrother already covers the loop most people care about:

- it tracks the open PRs you authored
- it tracks PRs that currently request your review
- it shows states such as `waiting review`, `waiting merge`, `conflict`, `failed`, `needs decision`, and `running`
- it can react to review feedback, CI failures, and merge conflicts
- it can run a read-only `Deep Review` on review-request PRs
- it can send Feishu notifications for runs and escalations when configured

The real value is not that it writes more code. The value is that it removes a lot of low-value PR babysitting:

- less manually opening PRs just to confirm status
- less re-checking review threads and CI for small changes
- less context switching between trivial follow-up work and important human decisions
- more explicit visibility into the exact places where a human actually needs to step in

If you usually have several PRs in flight at the same time, this is where it starts to pay off.

## What everyday use looks like

Most of the time, the dashboard is the product.

You leave BigBrother running locally, open [http://127.0.0.1:8787/](http://127.0.0.1:8787/), and use it as the place where PR state is summarized back to you. When a PR is fine, it simply sits in its current state. When something changes, you see it earlier and more directly than if you relied on occasionally checking GitHub.

The controls are intentionally simple:

- `Untrack` freezes a PR and stops automatic runs for it
- `Retry` immediately re-checks a failed PR
- `Addressed` clears a current `needs decision` blocker after you handled it yourself
- `Deep Review` runs a read-only review pass for a review-request PR

That is the intended interaction model. BigBrother should reduce tab-hopping and background anxiety, not create a second giant workflow that you now have to maintain.

## How to get started

Setup today is still explicit rather than magical. There is not a built-in `doctor`, `init`, or `setup --agent` command yet. What exists today is a configuration file, a local daemon, and a browser dashboard. The upside is that the first run is transparent: you can see exactly which token, path, and safety switches are in play before anything starts.

Before you begin, make sure the machine already has:

- a recent Rust toolchain
- `git`
- a GitHub token in `GITHUB_TOKEN` or `GH_TOKEN`
- a working `codex` command
- whatever Git credentials you normally use to push back to your PR branches

The smoothest layout is to keep `bigbrother` next to the repositories you want BigBrother to discover. If your machine already keeps code under `~/Coding`, cloning `bigbrother` there makes the default `workspace.root = ".."` work nicely because BigBrother will look for sibling repositories such as `../tikv`, `../tidb`, or `../tidb-cse`. If `bigbrother` lives somewhere else, set `workspace.root` to an absolute path such as `/Users/alice/Coding`. Avoid `~` and `$HOME/Coding` in the TOML file for now; the current config loader does not expand those forms.

The first-start flow is:

1. Copy the config template.

```bash
cp bigbrother.example.toml bigbrother.toml
```

2. Export the GitHub credentials BigBrother should use.

```bash
export GITHUB_TOKEN=...
export GITHUB_USER=...
```

If you keep `author = "$GITHUB_USER"` in the config, `GITHUB_USER` needs to be set to your GitHub login. If you do not want that extra environment variable, replace the `author` field with your real login or remove it entirely and let BigBrother resolve the viewer login at runtime.

3. Open `bigbrother.toml` and make it match your machine.

What you usually need to check:

- confirm `workspace.root`
- add `workspace.repo_map` only for repositories that are not located at `<workspace.root>/<repo-name>`
- keep `command = "codex"` unless your machine clearly uses another agent command
- leave `dangerously_bypass_approvals_and_sandbox` off unless you explicitly want unsandboxed local access on this host

4. If you want Feishu notifications, fill in `notifications.feishu`.

The current Feishu integration is outbound only. It can send run updates and escalations there, but it does not accept control commands back from Feishu yet.

5. Build and start the daemon.

```bash
cargo build --release
target/release/bigbrother --config bigbrother.toml
```

Then open [http://127.0.0.1:8787/](http://127.0.0.1:8787/).

## If you want an agent to handle setup

We are not at the point of a true one-command setup flow yet, but you can already hand most of the work to Codex. The repository includes a copy-paste setup prompt here:

[`docs/bigbrother_agent_setup_prompt.md`](/Users/Connor/Coding/bigbrother/docs/bigbrother_agent_setup_prompt.md)

It tells Codex to inspect the machine, patch `bigbrother.toml`, avoid unresolved placeholders, wire Feishu only when credentials are available, launch the daemon, and verify that the dashboard responds.

That is the current best approximation of agent-assisted setup without pretending a built-in setup wizard already exists.

## If something goes wrong

The recovery model is intentionally simple:

- To stop BigBrother, stop the `bigbrother` process like any other local daemon.
- To inspect what happened, open the PR detail page from the dashboard and read the saved run output or terminal recording.
- To take over manually, click `Untrack` first so the daemon stands down.
- To see where BigBrother was operating, look under `<workspace.root>/bigbrother-worktrees`.

Those managed worktrees belong to the daemon and may be rebuilt or reset later, so they are best treated as execution sandboxes rather than long-lived personal branches.

In one sentence, BigBrother is trying to do this: keep the daemon local, keep the dashboard visible, let the agent handle the safe mechanical work, and keep the real decisions in front of a human.
