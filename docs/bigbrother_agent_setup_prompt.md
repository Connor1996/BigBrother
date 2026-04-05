# Agent-assisted setup prompt for BigBrother

If you want Codex to do the first-time setup for you, start Codex from the root of this repository and paste the prompt below as-is. The prompt is written against the current implementation, which means it uses the existing config file and daemon workflow rather than imaginary future commands such as `bigbrother doctor` or `bigbrother setup --agent`.

```text
You are setting up BigBrother for local use in the `bigbrother` repository on this machine.

Work from the repository root and use the current implementation only. Do not invent or document commands that do not exist yet, including `bigbrother doctor`, `bigbrother init`, or `bigbrother setup --agent`.

Your goal is to leave the operator with a working local BigBrother setup, a clear summary of what was discovered, and a dashboard they can open immediately if all required credentials are present.

Start by inspecting the machine. Confirm whether `gh` is installed and authenticated, whether `codex` is available, whether either `GITHUB_TOKEN` or `GH_TOKEN` is set, whether Git can push to the operator's normal remotes, and which likely source repositories exist under the operator's shared code directory such as `~/Coding`. If you need the GitHub login, prefer discovering it from GitHub itself with `gh api user -q .login` instead of guessing.

Then create or update `bigbrother.toml`. If the file does not exist, copy `bigbrother.example.toml`. If it already exists, preserve the operator's intent and patch it carefully instead of replacing it wholesale. Set up the config so it matches the current machine.

For the GitHub section, use an environment-variable reference for the token if one already exists. If `author = "$GITHUB_USER"` would remain unresolved, either replace it with the real GitHub login you discovered or remove the `author` field so BigBrother can resolve the viewer login dynamically at runtime. Do not leave a literal unresolved placeholder there.

For the workspace section, prefer `workspace.root = ".."` only when this repository actually sits next to the tracked repositories and that relative layout is correct. Otherwise, use an absolute path such as `/Users/alice/Coding`. Do not write `~` or `$HOME/Coding` into the TOML because the current config loader does not expand those forms. Add `workspace.repo_map` entries only for repositories that cannot be found as `<workspace.root>/<repo-name>`.

For the agent section, keep `command = "codex"` unless the machine clearly uses another agent command. Keep the default high reasoning effort unless the operator asked for something lighter. Leave `dangerously_bypass_approvals_and_sandbox` off unless the operator explicitly wants unsandboxed Codex access on this host.

If Feishu credentials are already available, offer to wire them in. If they are not available, leave Feishu disabled and explain exactly what values would be needed later.

Before launching anything, show the operator a short setup summary in plain language. The summary should say which repositories were discovered, which ones will be tracked through `workspace.root` versus explicit `workspace.repo_map` entries, where the managed worktrees will live, whether Feishu notifications are enabled, and whether any required credential is still missing.

If the required credentials are present, build the release binary with `cargo build --release`. Do not run multiple Cargo commands concurrently. Then launch `target/release/bigbrother --config bigbrother.toml` and verify that the local dashboard responds at `http://127.0.0.1:8787/` or that `/api/health` returns successfully. If you choose to keep the daemon running in the background, make the launch method durable enough for local use and tell the operator exactly how to stop it. If you cannot keep it running safely in the background, do a short foreground smoke test and explain how the operator should start it for normal use.

End with one concise operator-facing report. That report should include the dashboard URL, whether the daemon is currently running, the managed worktree root, the notification target if Feishu is enabled, and the exact next action only if something is still missing.
```
