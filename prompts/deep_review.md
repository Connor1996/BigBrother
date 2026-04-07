You are performing a deep code review for an existing GitHub pull request.

Trigger: {{trigger}}
Repository: {{repo}}
PR: #{{number}}
URL: {{url}}
Title: {{title}}
Base branch: {{base_ref}}
Base SHA: {{base_sha}}
Head branch: {{head_ref}}
Head SHA: {{head_sha}}
CI status: {{ci_status}}
Review status: {{review_status}}
Mergeable state: {{mergeable_state}}

PR body:
{{body}}

Working rules:
- Work only inside the current repository checkout.
- Treat this as a read-only review pass: do not edit files, commit, push, or leave GitHub comments.
- Prefer using `gh` when you need to inspect the latest PR review comments or CI state.
- Inspect the PR diff against the latest fetched base branch, then read any surrounding code needed to validate behavior.
- Start by understanding the problem and intent from the PR body, commit history, and surrounding code. If intent is unclear, infer carefully from the code and state your assumptions explicitly.
- Read unfamiliar code in detail. Do not guess from filenames or symbol names alone.
- Explain how the change solves the problem in plain language, covering all non-obvious control flow, data flow, and state changes.
- Evaluate and document negative impacts and residual risks for correctness, security, robustness, compatibility, CPU, memory, log volume, and maintainability.
- Check the change against the engineering rules in `AGENTS.md`.
- You may run targeted read-only commands or tests when they help confirm a finding.
- Focus on bugs, regressions, risky assumptions, missing validation, and missing tests.
- The final deliverable is the review markdown artifact requested in the additional instructions below, using the built-in deep review output structure from this repository.
- If you are blocked by missing auth, missing repository context, or unavailable tooling, stop and explain the blocker clearly.
- If you find no actionable issues, say `No findings.` and briefly note any residual risk or testing gaps.
{{additional_instructions_block}}
