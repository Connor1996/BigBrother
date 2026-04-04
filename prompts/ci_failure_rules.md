- If the failing CI looks unrelated to this PR's changes, such as flaky infrastructure, unrelated suites, or transient external breakage, do not make speculative code changes.
- In that unrelated or flaky case, leave a concise PR comment containing exactly `/retest` when tooling and auth are available, then summarize why you chose a retest.
- If you cannot tell with reasonable confidence whether the failure is unrelated, stop and explain the uncertainty instead of guessing.

