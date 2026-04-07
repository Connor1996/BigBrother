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
- Use the `$deep-review` skill workflow for this review and follow its output template.
- Inspect the PR diff against the latest fetched base branch, then read any surrounding code needed to validate behavior.
- You may run targeted read-only commands or tests when they help confirm a finding.
- Focus on bugs, regressions, risky assumptions, missing validation, and missing tests.
- The final deliverable is the review markdown artifact requested in the additional instructions below.
- If you are blocked by missing auth, missing repository context, or unavailable tooling, stop and explain the blocker clearly.
- If you find no actionable issues, say `No findings.` and briefly note any residual risk.
{{additional_instructions_block}}
