use crate::model::{AttentionReason, PullRequest};

pub fn build_prompt(pr: &PullRequest, reason: AttentionReason, extra: Option<&str>) -> String {
    if reason == AttentionReason::DeepReview {
        return build_deep_review_prompt(pr, extra);
    }

    let reviewer_activity = pr
        .latest_reviewer_activity_at
        .map(|value| value.to_rfc3339())
        .unwrap_or_else(|| "none".to_owned());
    let body = pr
        .body
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("No PR body provided.");

    let extra = extra
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("\nAdditional operator instructions:\n{value}\n"))
        .unwrap_or_default();
    let trigger_specific_rules = trigger_specific_rules(reason);

    format!(
        "You are an autonomous agent helping maintain an existing GitHub pull request.\n\
         \n\
         Trigger: {trigger}\n\
         Repository: {repo}\n\
         PR: #{number}\n\
         URL: {url}\n\
         Title: {title}\n\
         Base branch: {base_ref}\n\
         Base SHA: {base_sha}\n\
         Head branch: {head_ref}\n\
         Head SHA: {head_sha}\n\
         CI status: {ci_status}\n\
         Review status: {review_status}\n\
         Mergeable state: {mergeable_state}\n\
         Has conflicts: {has_conflicts}\n\
         Review comments: {review_comments}\n\
         Issue comments: {issue_comments}\n\
         Latest reviewer activity: {reviewer_activity}\n\
         \n\
         PR body:\n\
         {body}\n\
         \n\
         Working rules:\n\
         - Work only inside the current repository checkout.\n\
         - Start by checking the current branch state, latest CI failures, and latest review comments for this PR.\n\
         - If the base and head branches differ, fetch and merge the latest base branch into the PR branch yourself before addressing the trigger-specific issue.\n\
         - If that merge produces conflicts, resolve them first, then continue addressing the original trigger.\n\
         - Before making code changes, assess whether the fix would be a material or high-risk change, such as a broad refactor, user-visible behavior change, API/schema change, or an important product tradeoff.\n\
         - If the required change is material or high-risk, stop before editing files and explain the decision or approval you need from the operator instead of changing code unilaterally.\n\
         {trigger_specific_rules}\
         - If code changes are needed, make them, run targeted validation, commit, and push back to the same PR branch.\n\
         - If reviewer feedback needs a textual response, leave a concise response on the PR thread when tooling is available.\n\
         - If you are blocked by missing auth, missing secrets, or ambiguous product decisions, stop and explain the blocker clearly.\n\
         - Final output should summarize whether you merged base, how conflicts were resolved if any, what changed, what was validated, and any remaining blocker.\n\
         {extra}",
        trigger = reason.label(),
        repo = pr.repo_full_name,
        number = pr.number,
        url = pr.url,
        title = pr.title,
        base_ref = pr.base_ref,
        base_sha = pr.base_sha,
        head_ref = pr.head_ref,
        head_sha = pr.head_sha,
        ci_status = pr.ci_status.label(),
        review_status = pr.review_decision.label(),
        mergeable_state = pr.mergeable_state.as_deref().unwrap_or("unknown"),
        has_conflicts = if pr.has_conflicts { "yes" } else { "no" },
        review_comments = pr.review_comment_count,
        issue_comments = pr.issue_comment_count,
        reviewer_activity = reviewer_activity,
        body = body,
        trigger_specific_rules = trigger_specific_rules,
        extra = extra
    )
}

fn build_deep_review_prompt(pr: &PullRequest, extra: Option<&str>) -> String {
    let body = pr
        .body
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("No PR body provided.");
    let extra = extra
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("\nAdditional operator instructions:\n{value}\n"))
        .unwrap_or_default();

    format!(
        "You are performing a deep code review for an existing GitHub pull request.\n\
         \n\
         Trigger: {trigger}\n\
         Repository: {repo}\n\
         PR: #{number}\n\
         URL: {url}\n\
         Title: {title}\n\
         Base branch: {base_ref}\n\
         Base SHA: {base_sha}\n\
         Head branch: {head_ref}\n\
         Head SHA: {head_sha}\n\
         CI status: {ci_status}\n\
         Review status: {review_status}\n\
         Mergeable state: {mergeable_state}\n\
         \n\
         PR body:\n\
         {body}\n\
         \n\
         Working rules:\n\
         - Work only inside the current repository checkout.\n\
         - Treat this as a read-only review pass: do not edit files, commit, push, or leave GitHub comments.\n\
         - Inspect the PR diff against the latest fetched base branch, then read any surrounding code needed to validate behavior.\n\
         - You may run targeted read-only commands or tests when they help confirm a finding.\n\
         - Focus on bugs, regressions, risky assumptions, missing validation, and missing tests.\n\
         - Final output must be a concise review report ordered by severity with file references when possible.\n\
         - If you find no actionable issues, say `No findings.` and briefly note any residual risk.\n\
         {extra}",
        trigger = AttentionReason::DeepReview.label(),
        repo = pr.repo_full_name,
        number = pr.number,
        url = pr.url,
        title = pr.title,
        base_ref = pr.base_ref,
        base_sha = pr.base_sha,
        head_ref = pr.head_ref,
        head_sha = pr.head_sha,
        ci_status = pr.ci_status.label(),
        review_status = pr.review_decision.label(),
        mergeable_state = pr.mergeable_state.as_deref().unwrap_or("unknown"),
        body = body,
        extra = extra
    )
}

fn trigger_specific_rules(reason: AttentionReason) -> &'static str {
    match reason {
        AttentionReason::CiFailed => {
            "- If the failing CI looks unrelated to this PR's changes (for example flaky infrastructure, unrelated suites, or transient external breakage), do not make speculative code changes.\n\
             - In that unrelated/flaky case, leave a concise PR comment containing exactly `/retest` when tooling and auth are available, then summarize why you chose a retest.\n\
             - If you cannot tell with reasonable confidence whether the failure is unrelated, stop and explain the uncertainty instead of guessing.\n"
        }
        AttentionReason::ReviewFeedback
        | AttentionReason::MergeConflict
        | AttentionReason::DeepReview => "",
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::build_prompt;
    use crate::model::{AttentionReason, CiStatus, PullRequest, ReviewDecision};

    fn sample_pr() -> PullRequest {
        PullRequest {
            key: "openai/symphony#7".to_owned(),
            repo_full_name: "openai/symphony".to_owned(),
            number: 7,
            title: "Prompt test".to_owned(),
            body: Some("Test body".to_owned()),
            url: "https://github.com/openai/symphony/pull/7".to_owned(),
            author_login: "connor".to_owned(),
            labels: vec![],
            created_at: Utc.with_ymd_and_hms(2026, 3, 31, 12, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 3, 31, 13, 0, 0).unwrap(),
            head_sha: "head123".to_owned(),
            head_ref: "feature/test".to_owned(),
            base_sha: "base456".to_owned(),
            base_ref: "main".to_owned(),
            clone_url: "https://github.com/openai/symphony.git".to_owned(),
            ssh_url: "git@github.com:openai/symphony.git".to_owned(),
            ci_status: CiStatus::Failure,
            ci_updated_at: None,
            review_decision: ReviewDecision::ChangesRequested,
            approval_count: 0,
            review_comment_count: 2,
            issue_comment_count: 1,
            latest_reviewer_activity_at: Some(Utc.with_ymd_and_hms(2026, 3, 31, 13, 5, 0).unwrap()),
            has_conflicts: true,
            mergeable_state: Some("dirty".to_owned()),
            is_draft: false,
            is_closed: false,
            is_merged: false,
        }
    }

    #[test]
    fn build_prompt_instructs_agent_to_merge_and_resolve_conflicts() {
        let prompt = build_prompt(
            &sample_pr(),
            AttentionReason::CiFailed,
            Some("- Extra operator note."),
        );

        assert!(prompt.contains("Trigger: CI failed"));
        assert!(prompt.contains("merge the latest base branch into the PR branch yourself"));
        assert!(prompt.contains("If that merge produces conflicts, resolve them first"));
        assert!(prompt.contains("If the required change is material or high-risk"));
        assert!(prompt.contains("leave a concise PR comment containing exactly `/retest`"));
        assert!(prompt.contains("whether you merged base"));
        assert!(prompt.contains("Additional operator instructions:\n- Extra operator note."));
    }

    #[test]
    fn build_prompt_limits_retest_instruction_to_ci_failures() {
        let prompt = build_prompt(&sample_pr(), AttentionReason::ReviewFeedback, None);

        assert!(!prompt.contains("`/retest`"));
        assert!(!prompt.contains("do not make speculative code changes"));
    }

    #[test]
    fn build_prompt_uses_read_only_review_instructions_for_manual_deep_review() {
        let prompt = build_prompt(&sample_pr(), AttentionReason::DeepReview, None);

        assert!(prompt.contains("You are performing a deep code review"));
        assert!(prompt.contains("do not edit files, commit, push, or leave GitHub comments"));
        assert!(prompt.contains("If you find no actionable issues, say `No findings.`"));
        assert!(!prompt.contains("merge the latest base branch into the PR branch yourself"));
    }
}
