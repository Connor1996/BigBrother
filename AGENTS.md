# BigBrother Instructions

## Global Repository Rules

- Always add signoff for every git commit. Use `git commit -s` (or include `Signed-off-by:` manually).
- After each completed repository change, stage the relevant files and create a git commit in the same turn unless the user explicitly asks not to commit yet.
- Do not run multiple `cargo` commands concurrently; they contend on package cache locks and can block with `Blocking waiting for file lock on package cache`.
- Do not override `CARGO_HOME` when running `cargo` commands.

## Spec Sync

- When a change materially affects the implementation's architecture, runtime behavior, workflows, API or UI operator controls, or documented constraints, update `docs/github_pr_supervisor_spec.md` in the same change.
- Do not leave required spec updates as follow-up work when the new behavior is being introduced now.
