---
name: deep-review
description: High-risk, production-critical code review focused on deep understanding and negative-impact analysis. Use when asked to perform a deep review of a PR or diff and to produce a detailed markdown review file in a specified target folder.
---

# Deep Review

## Goal

Write a production-critical review that explains the problem, the solution mechanics, and the full cost/risk profile. Always write the review to a markdown file under the target folder.

## Inputs and Defaults

- Determine the repository root and the target output folder.
- If the target folder is not provided, use `./target`.
- If the filename is not provided, use `review-report` prefix, system date and summary changes in a few words with `.md` suffix. e.g. `review-report-20260205-<summary-changes-in-words>.md`; do not overwrite an existing file.

## Workflow

1. Confirm inputs
   - Identify the repository root and target folder.
   - Confirm the output filename or choose a safe default.

2. Collect the change set
   - Use the provided diff; otherwise run `git diff origin/HEAD...HEAD` from the repository root.
   - If the command fails or `origin/HEAD` is missing, stop and ask for guidance.

3. Understand the problem and intent
   - Read available PR description, commit messages, or surrounding context.
   - If intent is unclear, infer from code and state any assumptions explicitly.
   - State the user-visible or system-level problem the change is trying to solve.
   - Read unfamiliar code in detail. Do not guess based on names; resolve all uncertainties.

4. Comprehensively explain how the change solves the problem
   - Walk through each non-obvious logic change in the diff.
   - Explain control flow, data flow, and state changes in plain language.
   - Do not skip any non-trivial behavior; reference file paths and relevant symbols.

5. Identify costs and negative impacts
   - Evaluate and document impacts for:
     - correctness
     - security
     - robustness and failure modes
     - compatibility breaking changes or behavioral shifts
     - non-trivial extra CPU usage
     - non-trivial extra memory usage
     - non-trivial log volume or logging costs
     - cognitive load or maintainability burdens
   - Call out regressions, risks, and edge cases explicitly.

6. Check engineering rules
   - Verify alignment with the `Engineering Rules` in `AGENTS.md`.

7. Write the review output
   - Write a markdown file under the target folder.
   - Use the template below to ensure completeness.
   - Include file/line references for findings.
   - If no findings exist, state that explicitly and note residual risks or testing gaps.

## Review Output Template

```markdown
### Deep Review

#### Problem Summary
- [Explain the concrete problem the change targets]

#### Solution Walkthrough
- [Explain how the change solves the problem; cover all non-obvious logic]

#### Findings (ordered by severity)
- [Issue or risk with file/line references]

#### Costs and Negative Impacts
- Correctness:
- Security:
- Compatibility:
- Robustness:
- Cognitive Load:
- CPU:
- Memory:
- Log Volume:

#### Engineering Rules Check
- [List code that breaks the engineering rules, or "None"]

#### Questions and Assumptions
- [List unknowns or assumptions made]

#### Suggested Tests / Validation
- [Targeted tests or checks to validate behavior]
```
