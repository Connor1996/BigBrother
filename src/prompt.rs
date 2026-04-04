use crate::{
    config::AgentPromptTemplates,
    model::{AttentionReason, PullRequest},
};

pub fn build_prompt(
    pr: &PullRequest,
    reason: AttentionReason,
    templates: &AgentPromptTemplates,
    extra: Option<&str>,
) -> String {
    if reason == AttentionReason::DeepReview {
        return build_deep_review_prompt(pr, templates, extra);
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

    render_template(
        &templates.actionable,
        &[
            ("trigger", reason.label().to_owned()),
            ("repo", pr.repo_full_name.clone()),
            ("number", pr.number.to_string()),
            ("url", pr.url.clone()),
            ("title", pr.title.clone()),
            ("base_ref", pr.base_ref.clone()),
            ("base_sha", pr.base_sha.clone()),
            ("head_ref", pr.head_ref.clone()),
            ("head_sha", pr.head_sha.clone()),
            ("ci_status", pr.ci_status.label().to_owned()),
            ("review_status", pr.review_decision.label().to_owned()),
            (
                "mergeable_state",
                pr.mergeable_state
                    .clone()
                    .unwrap_or_else(|| "unknown".to_owned()),
            ),
            (
                "has_conflicts",
                if pr.has_conflicts { "yes" } else { "no" }.to_owned(),
            ),
            ("review_comments", pr.review_comment_count.to_string()),
            ("issue_comments", pr.issue_comment_count.to_string()),
            ("reviewer_activity", reviewer_activity),
            ("body", body.to_owned()),
            (
                "trigger_specific_rules",
                trigger_specific_rules_block(reason, templates),
            ),
            (
                "additional_instructions_block",
                additional_instructions_block(extra),
            ),
        ],
    )
}

fn build_deep_review_prompt(
    pr: &PullRequest,
    templates: &AgentPromptTemplates,
    extra: Option<&str>,
) -> String {
    let body = pr
        .body
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("No PR body provided.");

    render_template(
        &templates.deep_review,
        &[
            ("trigger", AttentionReason::DeepReview.label().to_owned()),
            ("repo", pr.repo_full_name.clone()),
            ("number", pr.number.to_string()),
            ("url", pr.url.clone()),
            ("title", pr.title.clone()),
            ("base_ref", pr.base_ref.clone()),
            ("base_sha", pr.base_sha.clone()),
            ("head_ref", pr.head_ref.clone()),
            ("head_sha", pr.head_sha.clone()),
            ("ci_status", pr.ci_status.label().to_owned()),
            ("review_status", pr.review_decision.label().to_owned()),
            (
                "mergeable_state",
                pr.mergeable_state
                    .clone()
                    .unwrap_or_else(|| "unknown".to_owned()),
            ),
            ("body", body.to_owned()),
            (
                "additional_instructions_block",
                additional_instructions_block(extra),
            ),
        ],
    )
}

pub fn render_template(template: &str, values: &[(&str, String)]) -> String {
    let mut rendered = template.to_owned();
    for (key, value) in values {
        rendered = rendered.replace(&format!("{{{{{key}}}}}"), value);
    }

    rendered
}

fn additional_instructions_block(extra: Option<&str>) -> String {
    extra
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("Additional operator instructions:\n{value}\n"))
        .unwrap_or_default()
}

fn trigger_specific_rules_block(
    reason: AttentionReason,
    templates: &AgentPromptTemplates,
) -> String {
    match reason {
        AttentionReason::CiFailed => ensure_trailing_newline(&templates.ci_failure_rules),
        AttentionReason::ReviewFeedback
        | AttentionReason::MergeConflict
        | AttentionReason::DeepReview => String::new(),
    }
}

fn ensure_trailing_newline(value: &str) -> String {
    if value.is_empty() || value.ends_with('\n') {
        value.to_owned()
    } else {
        format!("{value}\n")
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::build_prompt;
    use crate::{
        config::AgentPromptTemplates,
        model::{AttentionReason, CiStatus, PullRequest, ReviewDecision},
    };

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
        let templates = AgentPromptTemplates::default();
        let prompt = build_prompt(
            &sample_pr(),
            AttentionReason::CiFailed,
            &templates,
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
        let templates = AgentPromptTemplates::default();
        let prompt = build_prompt(
            &sample_pr(),
            AttentionReason::ReviewFeedback,
            &templates,
            None,
        );

        assert!(!prompt.contains("`/retest`"));
        assert!(!prompt.contains("do not make speculative code changes"));
    }

    #[test]
    fn build_prompt_uses_read_only_review_instructions_for_manual_deep_review() {
        let templates = AgentPromptTemplates::default();
        let prompt = build_prompt(&sample_pr(), AttentionReason::DeepReview, &templates, None);

        assert!(prompt.contains("You are performing a deep code review"));
        assert!(prompt.contains("do not edit files, commit, push, or leave GitHub comments"));
        assert!(prompt.contains("Use the `$deep-review` skill workflow"));
        assert!(prompt.contains("If you find no actionable issues, say `No findings.`"));
        assert!(!prompt.contains("merge the latest base branch into the PR branch yourself"));
    }

    #[test]
    fn build_prompt_uses_custom_actionable_template_override() {
        let mut templates = AgentPromptTemplates::default();
        templates.actionable =
            "Runbook for {{repo}} #{{number}}\n{{additional_instructions_block}}".to_owned();

        let prompt = build_prompt(
            &sample_pr(),
            AttentionReason::ReviewFeedback,
            &templates,
            Some("- Custom note."),
        );

        assert!(prompt.contains("Runbook for openai/symphony #7"));
        assert!(prompt.contains("Additional operator instructions:\n- Custom note."));
        assert!(!prompt.contains("Working rules:"));
    }
}
