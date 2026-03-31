use std::collections::HashMap;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use futures::{future::BoxFuture, stream, StreamExt, TryStreamExt};
use reqwest::{
    header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT},
    Client,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::{
    config::{build_search_query, ResolvedGitHubConfig},
    model::{CiStatus, PullRequest, ReviewDecision},
    service::GitHubProvider,
};

pub struct GitHubClient {
    http: Client,
    api_base_url: String,
    config: ResolvedGitHubConfig,
}

impl GitHubClient {
    pub fn new(config: ResolvedGitHubConfig) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static("symphony-rs/0.1"));

        let auth_value = format!("Bearer {}", config.api_token);
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth_value).context("failed to encode auth header")?,
        );

        let http = Client::builder()
            .default_headers(headers)
            .build()
            .context("failed to build GitHub HTTP client")?;

        Ok(Self {
            http,
            api_base_url: config.api_base_url.trim_end_matches('/').to_owned(),
            config,
        })
    }

    pub async fn fetch_pull_requests(&self) -> Result<Vec<PullRequest>> {
        let author = match &self.config.author {
            Some(author) => author.clone(),
            None => self.fetch_viewer_login().await?,
        };

        let query = build_search_query(&self.config, &author);
        let search: SearchResponse = self
            .get_json(
                "search/issues",
                &[
                    ("q", query),
                    ("sort", "updated".to_owned()),
                    ("order", "desc".to_owned()),
                    ("per_page", self.config.max_prs.to_string()),
                ],
            )
            .await?;

        let prs = stream::iter(search.items.into_iter().map(|item| {
            let author = author.clone();
            async move { self.enrich_pull_request(item, &author).await }
        }))
        .buffer_unordered(6)
        .try_collect::<Vec<_>>()
        .await?;

        let mut prs = prs;
        prs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(prs)
    }

    async fn fetch_viewer_login(&self) -> Result<String> {
        let user: ViewerResponse = self.get_json("user", &[]).await?;
        Ok(user.login)
    }

    async fn enrich_pull_request(&self, item: SearchItem, author: &str) -> Result<PullRequest> {
        let repo = parse_repo_from_api_url(&item.repository_url)
            .with_context(|| format!("failed to parse repo from {}", item.repository_url))?;

        let detail: PullDetail = self
            .get_json(&format!("repos/{repo}/pulls/{}", item.number), &[])
            .await?;

        let reviews_path = format!("repos/{repo}/pulls/{}/reviews", item.number);
        let review_comments_path = format!("repos/{repo}/pulls/{}/comments", item.number);
        let issue_comments_path = format!("repos/{repo}/issues/{}/comments", item.number);
        let check_runs_path = format!("repos/{repo}/commits/{}/check-runs", detail.head.sha);
        let combined_status_path = format!("repos/{repo}/commits/{}/status", detail.head.sha);
        let per_page = vec![("per_page", "100".to_owned())];

        let (reviews, review_comments, issue_comments, check_runs, combined_status) = tokio::try_join!(
            self.get_json::<Vec<Review>>(&reviews_path, &per_page,),
            self.get_json::<Vec<ReviewComment>>(&review_comments_path, &per_page,),
            self.get_json::<Vec<IssueComment>>(&issue_comments_path, &per_page,),
            self.get_json::<CheckRunsResponse>(&check_runs_path, &per_page,),
            self.get_json::<CombinedStatus>(&combined_status_path, &[]),
        )?;

        let review_summary = summarize_reviews(author, &reviews, &review_comments, &issue_comments);
        let ci_summary = summarize_ci(
            &check_runs.check_runs,
            &combined_status.statuses,
            &combined_status.state,
        );
        let repo_full_name = detail
            .base
            .repo
            .as_ref()
            .map(|repo| repo.full_name.clone())
            .unwrap_or_else(|| repo.clone());
        let head_repo = detail
            .head
            .repo
            .clone()
            .or_else(|| detail.base.repo.clone())
            .ok_or_else(|| {
                anyhow!(
                    "pull request {} is missing repository metadata",
                    item.number
                )
            })?;

        Ok(PullRequest {
            key: format!("{repo_full_name}#{}", item.number),
            repo_full_name,
            number: item.number,
            title: detail.title,
            body: detail.body,
            url: detail.html_url,
            author_login: detail.user.login,
            labels: detail.labels.into_iter().map(|label| label.name).collect(),
            created_at: detail.created_at,
            updated_at: detail.updated_at,
            head_sha: detail.head.sha,
            head_ref: detail.head.reference,
            base_ref: detail.base.reference,
            clone_url: head_repo.clone_url,
            ssh_url: head_repo.ssh_url,
            ci_status: ci_summary.status,
            ci_updated_at: ci_summary.updated_at,
            review_decision: review_summary.decision,
            approval_count: review_summary.approval_count,
            review_comment_count: review_summary.review_comment_count,
            issue_comment_count: review_summary.issue_comment_count,
            latest_reviewer_activity_at: review_summary.latest_activity_at,
            is_draft: detail.draft,
            is_closed: detail.state.eq_ignore_ascii_case("closed") && detail.merged_at.is_none(),
            is_merged: detail.merged_at.is_some(),
        })
    }

    async fn get_json<T>(&self, path: &str, query: &[(&str, String)]) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let url = format!("{}/{}", self.api_base_url, path.trim_start_matches('/'));
        let response = self
            .http
            .get(url.clone())
            .query(query)
            .send()
            .await
            .with_context(|| format!("GitHub request failed for {url}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<body unavailable>".to_owned());
            return Err(anyhow!("GitHub request {url} failed with {status}: {body}"));
        }

        response
            .json::<T>()
            .await
            .with_context(|| format!("failed to decode GitHub response from {url}"))
    }
}

impl GitHubProvider for GitHubClient {
    fn fetch_pull_requests(&self) -> BoxFuture<'_, Result<Vec<PullRequest>>> {
        Box::pin(async move { GitHubClient::fetch_pull_requests(self).await })
    }
}

#[derive(Debug, Deserialize)]
struct ViewerResponse {
    login: String,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    items: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
struct SearchItem {
    number: u64,
    repository_url: String,
}

#[derive(Debug, Deserialize)]
struct PullDetail {
    title: String,
    body: Option<String>,
    html_url: String,
    user: GitHubUser,
    labels: Vec<Label>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    state: String,
    draft: bool,
    merged_at: Option<DateTime<Utc>>,
    head: GitReference,
    base: GitReference,
}

#[derive(Debug, Deserialize)]
struct GitReference {
    #[serde(rename = "ref")]
    reference: String,
    sha: String,
    repo: Option<RepoRef>,
}

#[derive(Debug, Deserialize, Clone)]
struct RepoRef {
    full_name: String,
    clone_url: String,
    ssh_url: String,
}

#[derive(Debug, Deserialize)]
struct GitHubUser {
    login: String,
}

#[derive(Debug, Deserialize)]
struct Label {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Review {
    state: String,
    submitted_at: Option<DateTime<Utc>>,
    user: Option<GitHubUser>,
}

#[derive(Debug, Deserialize)]
struct ReviewComment {
    updated_at: DateTime<Utc>,
    user: Option<GitHubUser>,
}

#[derive(Debug, Deserialize)]
struct IssueComment {
    updated_at: DateTime<Utc>,
    user: Option<GitHubUser>,
}

#[derive(Debug, Deserialize)]
struct CheckRunsResponse {
    #[serde(default)]
    check_runs: Vec<CheckRun>,
}

#[derive(Debug, Deserialize)]
struct CheckRun {
    status: String,
    conclusion: Option<String>,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct CombinedStatus {
    state: String,
    #[serde(default)]
    statuses: Vec<StatusContext>,
}

#[derive(Debug, Deserialize)]
struct StatusContext {
    state: String,
    updated_at: DateTime<Utc>,
}

struct ReviewSummary {
    decision: ReviewDecision,
    approval_count: usize,
    review_comment_count: usize,
    issue_comment_count: usize,
    latest_activity_at: Option<DateTime<Utc>>,
}

struct CiSummary {
    status: CiStatus,
    updated_at: Option<DateTime<Utc>>,
}

fn summarize_reviews(
    author: &str,
    reviews: &[Review],
    review_comments: &[ReviewComment],
    issue_comments: &[IssueComment],
) -> ReviewSummary {
    let non_author_reviews = reviews
        .iter()
        .filter(|review| review.user.as_ref().map(|user| user.login.as_str()) != Some(author))
        .collect::<Vec<_>>();
    let mut latest_review_states = HashMap::new();

    for (index, review) in non_author_reviews.iter().enumerate() {
        let Some(user) = review.user.as_ref() else {
            continue;
        };

        let candidate_order = (review.submitted_at, index);
        let replace = latest_review_states
            .get(user.login.as_str())
            .map(|existing: &(Option<DateTime<Utc>>, usize, String)| {
                candidate_order >= (existing.0, existing.1)
            })
            .unwrap_or(true);

        if replace {
            latest_review_states.insert(
                user.login.as_str(),
                (review.submitted_at, index, review.state.clone()),
            );
        }
    }

    let review_comment_count = review_comments
        .iter()
        .filter(|comment| comment.user.as_ref().map(|user| user.login.as_str()) != Some(author))
        .count();
    let issue_comment_count = issue_comments
        .iter()
        .filter(|comment| comment.user.as_ref().map(|user| user.login.as_str()) != Some(author))
        .count();
    let approval_count = latest_review_states
        .values()
        .filter(|(_, _, state)| state.eq_ignore_ascii_case("APPROVED"))
        .count();

    let latest_activity_at = non_author_reviews
        .iter()
        .filter_map(|review| review.submitted_at)
        .chain(review_comments.iter().filter_map(|comment| {
            (comment.user.as_ref().map(|user| user.login.as_str()) != Some(author))
                .then_some(comment.updated_at)
        }))
        .chain(issue_comments.iter().filter_map(|comment| {
            (comment.user.as_ref().map(|user| user.login.as_str()) != Some(author))
                .then_some(comment.updated_at)
        }))
        .max();

    let decision = if latest_review_states
        .values()
        .any(|(_, _, state)| state.eq_ignore_ascii_case("CHANGES_REQUESTED"))
    {
        ReviewDecision::ChangesRequested
    } else if approval_count > 0 {
        ReviewDecision::Approved
    } else if review_comment_count > 0
        || issue_comment_count > 0
        || latest_review_states
            .values()
            .any(|(_, _, state)| state.eq_ignore_ascii_case("COMMENTED"))
    {
        ReviewDecision::Commented
    } else {
        ReviewDecision::Clean
    };

    ReviewSummary {
        decision,
        approval_count,
        review_comment_count,
        issue_comment_count,
        latest_activity_at,
    }
}

fn summarize_ci(
    check_runs: &[CheckRun],
    statuses: &[StatusContext],
    combined_state: &str,
) -> CiSummary {
    let checks_status = collapse_check_runs(check_runs);
    let status_context_status = collapse_status_contexts(statuses, combined_state);

    let status = match (checks_status.0, status_context_status.0) {
        (CiStatus::Failure, _) | (_, CiStatus::Failure) => CiStatus::Failure,
        (CiStatus::Pending, _) | (_, CiStatus::Pending) => CiStatus::Pending,
        (CiStatus::Success, _) | (_, CiStatus::Success) => CiStatus::Success,
        _ => CiStatus::Unknown,
    };

    let updated_at = checks_status
        .1
        .into_iter()
        .chain(status_context_status.1)
        .max();

    CiSummary { status, updated_at }
}

fn collapse_check_runs(check_runs: &[CheckRun]) -> (CiStatus, Option<DateTime<Utc>>) {
    let mut status = CiStatus::Unknown;
    let mut updated_at = None;

    for check in check_runs {
        updated_at = updated_at
            .into_iter()
            .chain(check.started_at)
            .chain(check.completed_at)
            .max();

        let current = match (
            check.status.to_ascii_lowercase().as_str(),
            check.conclusion.as_deref(),
        ) {
            ("queued", _) | ("in_progress", _) | ("waiting", _) => CiStatus::Pending,
            ("completed", Some("success"))
            | ("completed", Some("neutral"))
            | ("completed", Some("skipped")) => CiStatus::Success,
            ("completed", Some(_)) => CiStatus::Failure,
            _ => CiStatus::Unknown,
        };

        status = merge_ci_status(status, current);
    }

    (status, updated_at)
}

fn collapse_status_contexts(
    statuses: &[StatusContext],
    combined_state: &str,
) -> (CiStatus, Option<DateTime<Utc>>) {
    let mut status = match combined_state.to_ascii_lowercase().as_str() {
        "failure" | "error" => CiStatus::Failure,
        "pending" => CiStatus::Pending,
        "success" => CiStatus::Success,
        _ => CiStatus::Unknown,
    };
    let mut updated_at = statuses.iter().map(|status| status.updated_at).max();

    for context in statuses {
        let current = match context.state.to_ascii_lowercase().as_str() {
            "failure" | "error" => CiStatus::Failure,
            "pending" => CiStatus::Pending,
            "success" => CiStatus::Success,
            _ => CiStatus::Unknown,
        };
        status = merge_ci_status(status, current);
        updated_at = updated_at.into_iter().chain(Some(context.updated_at)).max();
    }

    (status, updated_at)
}

fn merge_ci_status(existing: CiStatus, incoming: CiStatus) -> CiStatus {
    match (existing, incoming) {
        (CiStatus::Failure, _) | (_, CiStatus::Failure) => CiStatus::Failure,
        (CiStatus::Pending, _) | (_, CiStatus::Pending) => CiStatus::Pending,
        (CiStatus::Success, _) | (_, CiStatus::Success) => CiStatus::Success,
        _ => CiStatus::Unknown,
    }
}

fn parse_repo_from_api_url(url: &str) -> Result<String> {
    let parts = url
        .trim_end_matches('/')
        .split('/')
        .rev()
        .take(2)
        .collect::<Vec<_>>();

    if parts.len() != 2 {
        return Err(anyhow!("unsupported GitHub repository URL: {url}"));
    }

    Ok(format!("{}/{}", parts[1], parts[0]))
}
