use crate::model::{AttentionReason, PullRequest};

pub fn build_prompt(pr: &PullRequest, reason: AttentionReason, extra: Option<&str>) -> String {
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

    format!(
        "You are an autonomous agent helping maintain an existing GitHub pull request.\n\
         \n\
         Trigger: {trigger}\n\
         Repository: {repo}\n\
         PR: #{number}\n\
         URL: {url}\n\
         Title: {title}\n\
         Base branch: {base_ref}\n\
         Head branch: {head_ref}\n\
         Head SHA: {head_sha}\n\
         CI status: {ci_status}\n\
         Review status: {review_status}\n\
         Review comments: {review_comments}\n\
         Issue comments: {issue_comments}\n\
         Latest reviewer activity: {reviewer_activity}\n\
         \n\
         PR body:\n\
         {body}\n\
         \n\
         Working rules:\n\
         - Work only inside the current repository checkout.\n\
         - Start by checking the latest CI failures and review comments for this PR.\n\
         - If code changes are needed, make them, run targeted validation, commit, and push back to the same PR branch.\n\
         - If reviewer feedback needs a textual response, leave a concise response on the PR thread when tooling is available.\n\
         - If you are blocked by missing auth, missing secrets, or ambiguous product decisions, stop and explain the blocker clearly.\n\
         - Final output should summarize what changed, what was validated, and any remaining blocker.\n\
         {extra}",
        trigger = reason.label(),
        repo = pr.repo_full_name,
        number = pr.number,
        url = pr.url,
        title = pr.title,
        base_ref = pr.base_ref,
        head_ref = pr.head_ref,
        head_sha = pr.head_sha,
        ci_status = pr.ci_status.label(),
        review_status = pr.review_decision.label(),
        review_comments = pr.review_comment_count,
        issue_comments = pr.issue_comment_count,
        reviewer_activity = reviewer_activity,
        body = body,
        extra = extra
    )
}
