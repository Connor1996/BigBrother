use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use futures::{future::BoxFuture, stream, StreamExt, TryStreamExt};
use reqwest::{
    header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT},
    Client,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{
    config::{build_review_request_query, build_search_query, ResolvedGitHubConfig},
    model::{CiStatus, PullRequest, ReviewDecision},
    service::{GitHubProvider, GitHubRequestStats, PollQueryState},
};

const SIGNAL_REFRESH_GRACE_PERIOD_SECS: i64 = 30 * 60;

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
        self.fetch_pull_requests_with_state_and_stats(PollQueryState::default())
            .await
            .map(|(prs, _)| prs)
    }

    pub async fn fetch_pull_requests_with_state(
        &self,
        poll_state: PollQueryState,
    ) -> Result<Vec<PullRequest>> {
        self.fetch_pull_requests_with_state_and_stats(poll_state)
            .await
            .map(|(prs, _)| prs)
    }

    pub async fn fetch_pull_requests_with_state_and_stats(
        &self,
        poll_state: PollQueryState,
    ) -> Result<(Vec<PullRequest>, GitHubRequestStats)> {
        let metrics = GitHubRequestMetrics::default();
        let author = match &self.config.author {
            Some(author) => author.clone(),
            None => self.fetch_viewer_login(&metrics).await?,
        };
        let previous_prs = poll_state
            .previous_prs
            .into_iter()
            .map(|pr| (pr.key.clone(), pr))
            .collect::<HashMap<_, _>>();
        let frozen_pr_keys = poll_state
            .frozen_pr_keys
            .into_iter()
            .collect::<HashSet<_>>();
        let previous_prs = Arc::new(previous_prs);
        let frozen_pr_keys = Arc::new(frozen_pr_keys);

        let query = build_search_query(&self.config, &author);
        self.fetch_pull_requests_for_query_with_stats(
            &author,
            query,
            previous_prs,
            frozen_pr_keys,
            &metrics,
        )
        .await
    }

    pub async fn fetch_review_requests_with_stats(
        &self,
    ) -> Result<(Vec<PullRequest>, GitHubRequestStats)> {
        let metrics = GitHubRequestMetrics::default();
        let reviewer = match &self.config.author {
            Some(author) => author.clone(),
            None => self.fetch_viewer_login(&metrics).await?,
        };
        self.fetch_review_requests_for_query_with_stats(
            build_review_request_query(&reviewer),
            &metrics,
        )
        .await
    }

    async fn fetch_review_requests_for_query_with_stats(
        &self,
        query: String,
        metrics: &GitHubRequestMetrics,
    ) -> Result<(Vec<PullRequest>, GitHubRequestStats)> {
        let search: SearchResponse = self
            .get_json(
                "search/issues",
                &[
                    ("q", query),
                    ("sort", "updated".to_owned()),
                    ("order", "desc".to_owned()),
                    ("per_page", self.config.max_prs.to_string()),
                ],
                metrics,
                RequestCategory::Search,
            )
            .await?;
        let total_matching_prs = search.total_count;

        let mut prs = Vec::with_capacity(search.items.len());
        for item in search.items {
            let repo = parse_repo_from_api_url(&item.repository_url)
                .with_context(|| format!("failed to parse repo from {}", item.repository_url))?;
            prs.push(build_pull_request_from_search_item(&repo, item));
            metrics.record_light_pr();
        }

        prs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        let mut stats = metrics.snapshot();
        stats.total_matching_prs = Some(total_matching_prs);
        Ok((prs, stats))
    }

    async fn fetch_pull_requests_for_query_with_stats(
        &self,
        author: &str,
        query: String,
        previous_prs: Arc<HashMap<String, PullRequest>>,
        frozen_pr_keys: Arc<HashSet<String>>,
        metrics: &GitHubRequestMetrics,
    ) -> Result<(Vec<PullRequest>, GitHubRequestStats)> {
        let search: SearchResponse = self
            .get_json(
                "search/issues",
                &[
                    ("q", query),
                    ("sort", "updated".to_owned()),
                    ("order", "desc".to_owned()),
                    ("per_page", self.config.max_prs.to_string()),
                ],
                metrics,
                RequestCategory::Search,
            )
            .await?;
        let total_matching_prs = search.total_count;

        let search_results = stream::iter(search.items.into_iter().map(|item| {
            let metrics = metrics.clone();
            let previous_prs = previous_prs.clone();
            let frozen_pr_keys = frozen_pr_keys.clone();
            async move {
                let repo = parse_repo_from_api_url(&item.repository_url).with_context(|| {
                    format!("failed to parse repo from {}", item.repository_url)
                })?;
                let pr_key = format!("{repo}#{}", item.number);

                if let Some(previous_pr) =
                    reuse_frozen_pull_request(&pr_key, &frozen_pr_keys, &previous_prs)
                {
                    metrics.record_light_pr();
                    metrics.record_reused_pr();
                    return Ok::<_, anyhow::Error>(SearchResultPr::Frozen(previous_pr));
                }

                let detail = self.fetch_pull_detail(&repo, item.number, &metrics).await?;
                let pr = build_pull_request_from_detail(&repo, item.number, detail)?;
                metrics.record_light_pr();
                Ok::<_, anyhow::Error>(SearchResultPr::Light { repo, pr })
            }
        }))
        .buffer_unordered(6)
        .try_collect::<Vec<_>>()
        .await?;

        let prs = stream::iter(search_results.into_iter().map(|search_result| {
            let author = author.to_owned();
            let previous_prs = previous_prs.clone();
            let metrics = metrics.clone();
            async move {
                match search_result {
                    SearchResultPr::Frozen(pr) => Ok::<PullRequest, anyhow::Error>(pr),
                    SearchResultPr::Light { repo, pr: light_pr } => {
                        let previous_pr = previous_prs.get(&light_pr.key).cloned();

                        if should_refresh_signal_details(&light_pr, previous_pr.as_ref()) {
                            let pr = self
                                .hydrate_pull_request_signals(&repo, &author, light_pr, &metrics)
                                .await?;
                            metrics.record_hydrated_pr();
                            Ok::<PullRequest, anyhow::Error>(pr)
                        } else if let Some(previous_pr) = previous_pr {
                            metrics.record_reused_pr();
                            Ok::<PullRequest, anyhow::Error>(reuse_cached_signal_details(
                                light_pr,
                                &previous_pr,
                            ))
                        } else {
                            let pr = self
                                .hydrate_pull_request_signals(&repo, &author, light_pr, &metrics)
                                .await?;
                            metrics.record_hydrated_pr();
                            Ok::<PullRequest, anyhow::Error>(pr)
                        }
                    }
                }
            }
        }))
        .buffer_unordered(6)
        .try_collect::<Vec<_>>()
        .await?;

        let mut prs = prs;
        prs.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        let mut stats = metrics.snapshot();
        stats.total_matching_prs = Some(total_matching_prs);
        Ok((prs, stats))
    }

    pub async fn fetch_pull_request_by_key(&self, pr_key: &str) -> Result<Option<PullRequest>> {
        self.fetch_pull_request_by_key_with_stats(pr_key)
            .await
            .map(|(pr, _)| pr)
    }

    pub async fn fetch_pull_request_by_key_with_stats(
        &self,
        pr_key: &str,
    ) -> Result<(Option<PullRequest>, GitHubRequestStats)> {
        let metrics = GitHubRequestMetrics::default();
        let (repo, number) = parse_pr_key(pr_key)?;
        let author = match &self.config.author {
            Some(author) => author.clone(),
            None => self.fetch_viewer_login(&metrics).await?,
        };

        let detail = self.fetch_pull_detail(&repo, number, &metrics).await?;
        let pr = build_pull_request_from_detail(&repo, number, detail)?;
        metrics.record_light_pr();

        let pr = self
            .hydrate_pull_request_signals(&repo, &author, pr, &metrics)
            .await
            .map(Some)?;
        metrics.record_hydrated_pr();

        Ok((pr, metrics.snapshot()))
    }

    async fn fetch_viewer_login(&self, metrics: &GitHubRequestMetrics) -> Result<String> {
        let user: ViewerResponse = self
            .get_json("user", &[], metrics, RequestCategory::Viewer)
            .await?;
        Ok(user.login)
    }

    async fn fetch_pull_detail(
        &self,
        repo: &str,
        number: u64,
        metrics: &GitHubRequestMetrics,
    ) -> Result<PullDetail> {
        self.get_json(
            &format!("repos/{repo}/pulls/{number}"),
            &[],
            metrics,
            RequestCategory::PullDetail,
        )
        .await
    }

    async fn hydrate_pull_request_signals(
        &self,
        repo: &str,
        author: &str,
        mut pr: PullRequest,
        metrics: &GitHubRequestMetrics,
    ) -> Result<PullRequest> {
        let per_page = vec![("per_page", "100".to_owned())];
        let reviews_path = format!("repos/{repo}/pulls/{}/reviews", pr.number);
        let review_comments_path = format!("repos/{repo}/pulls/{}/comments", pr.number);
        let issue_comments_path = format!("repos/{repo}/issues/{}/comments", pr.number);
        let check_runs_path = format!("repos/{repo}/commits/{}/check-runs", pr.head_sha);

        let (reviews, review_comments, issue_comments, check_runs) = tokio::try_join!(
            self.get_json::<Vec<Review>>(
                &reviews_path,
                &per_page,
                metrics,
                RequestCategory::Reviews
            ),
            self.get_json::<Vec<ReviewComment>>(
                &review_comments_path,
                &per_page,
                metrics,
                RequestCategory::ReviewComments
            ),
            self.get_json::<Vec<IssueComment>>(
                &issue_comments_path,
                &per_page,
                metrics,
                RequestCategory::IssueComments
            ),
            self.get_json::<CheckRunsResponse>(
                &check_runs_path,
                &per_page,
                metrics,
                RequestCategory::CheckRuns
            ),
        )?;

        let review_summary = summarize_reviews(author, &reviews, &review_comments, &issue_comments);
        let ci_summary = summarize_ci(&check_runs.check_runs);
        apply_signal_summary(&mut pr, review_summary, ci_summary);
        Ok(pr)
    }

    async fn get_json<T>(
        &self,
        path: &str,
        query: &[(&str, String)],
        metrics: &GitHubRequestMetrics,
        category: RequestCategory,
    ) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let url = format!("{}/{}", self.api_base_url, path.trim_start_matches('/'));
        metrics.record_request(category);
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

    async fn post_json<B>(&self, path: &str, body: &B) -> Result<()>
    where
        B: Serialize + ?Sized,
    {
        let url = format!("{}/{}", self.api_base_url, path.trim_start_matches('/'));
        let response = self
            .http
            .post(url.clone())
            .json(body)
            .send()
            .await
            .with_context(|| format!("GitHub request failed for {url}"))?;

        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<body unavailable>".to_owned());
            Err(anyhow!("GitHub request {url} failed with {status}: {body}"))
        }
    }
}

impl GitHubProvider for GitHubClient {
    fn fetch_pull_requests(&self) -> BoxFuture<'_, Result<Vec<PullRequest>>> {
        Box::pin(async move { GitHubClient::fetch_pull_requests(self).await })
    }

    fn fetch_pull_requests_with_state(
        &self,
        poll_state: PollQueryState,
    ) -> BoxFuture<'_, Result<Vec<PullRequest>>> {
        Box::pin(
            async move { GitHubClient::fetch_pull_requests_with_state(self, poll_state).await },
        )
    }

    fn fetch_pull_requests_with_state_and_stats(
        &self,
        poll_state: PollQueryState,
    ) -> BoxFuture<'_, Result<(Vec<PullRequest>, GitHubRequestStats)>> {
        Box::pin(async move {
            GitHubClient::fetch_pull_requests_with_state_and_stats(self, poll_state).await
        })
    }

    fn fetch_review_requests_with_stats(
        &self,
    ) -> BoxFuture<'_, Result<(Vec<PullRequest>, GitHubRequestStats)>> {
        Box::pin(async move { GitHubClient::fetch_review_requests_with_stats(self).await })
    }

    fn fetch_pull_request(&self, pr_key: String) -> BoxFuture<'_, Result<Option<PullRequest>>> {
        Box::pin(async move { self.fetch_pull_request_by_key(&pr_key).await })
    }

    fn fetch_pull_request_with_stats(
        &self,
        pr_key: String,
    ) -> BoxFuture<'_, Result<(Option<PullRequest>, GitHubRequestStats)>> {
        Box::pin(async move { self.fetch_pull_request_by_key_with_stats(&pr_key).await })
    }

    fn post_issue_comment(&self, pr_key: String, body: String) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let (repo, number) = parse_pr_key(&pr_key)?;
            self.post_json(
                &format!("repos/{repo}/issues/{number}/comments"),
                &IssueCommentRequest { body },
            )
            .await
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum RequestCategory {
    Viewer,
    Search,
    PullDetail,
    Reviews,
    ReviewComments,
    IssueComments,
    CheckRuns,
}

#[derive(Debug, Clone, Default)]
struct GitHubRequestMetrics {
    inner: Arc<GitHubRequestMetricsInner>,
}

#[derive(Debug, Default)]
struct GitHubRequestMetricsInner {
    viewer_requests: AtomicUsize,
    search_requests: AtomicUsize,
    pull_detail_requests: AtomicUsize,
    review_requests: AtomicUsize,
    review_comment_requests: AtomicUsize,
    issue_comment_requests: AtomicUsize,
    check_run_requests: AtomicUsize,
    light_prs: AtomicUsize,
    hydrated_prs: AtomicUsize,
    reused_prs: AtomicUsize,
}

impl GitHubRequestMetrics {
    fn record_request(&self, category: RequestCategory) {
        let counter = match category {
            RequestCategory::Viewer => &self.inner.viewer_requests,
            RequestCategory::Search => &self.inner.search_requests,
            RequestCategory::PullDetail => &self.inner.pull_detail_requests,
            RequestCategory::Reviews => &self.inner.review_requests,
            RequestCategory::ReviewComments => &self.inner.review_comment_requests,
            RequestCategory::IssueComments => &self.inner.issue_comment_requests,
            RequestCategory::CheckRuns => &self.inner.check_run_requests,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn record_light_pr(&self) {
        self.inner.light_prs.fetch_add(1, Ordering::Relaxed);
    }

    fn record_hydrated_pr(&self) {
        self.inner.hydrated_prs.fetch_add(1, Ordering::Relaxed);
    }

    fn record_reused_pr(&self) {
        self.inner.reused_prs.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> GitHubRequestStats {
        GitHubRequestStats {
            total_matching_prs: None,
            viewer_requests: self.inner.viewer_requests.load(Ordering::Relaxed),
            search_requests: self.inner.search_requests.load(Ordering::Relaxed),
            pull_detail_requests: self.inner.pull_detail_requests.load(Ordering::Relaxed),
            review_requests: self.inner.review_requests.load(Ordering::Relaxed),
            review_comment_requests: self.inner.review_comment_requests.load(Ordering::Relaxed),
            issue_comment_requests: self.inner.issue_comment_requests.load(Ordering::Relaxed),
            check_run_requests: self.inner.check_run_requests.load(Ordering::Relaxed),
            light_prs: self.inner.light_prs.load(Ordering::Relaxed),
            hydrated_prs: self.inner.hydrated_prs.load(Ordering::Relaxed),
            reused_prs: self.inner.reused_prs.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ViewerResponse {
    login: String,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    total_count: usize,
    items: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
struct SearchItem {
    number: u64,
    repository_url: String,
    html_url: String,
    title: String,
    body: Option<String>,
    user: GitHubUser,
    #[serde(default)]
    labels: Vec<Label>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
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
    mergeable: Option<bool>,
    mergeable_state: Option<String>,
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

#[derive(Debug, Deserialize, Clone)]
struct GitHubUser {
    login: String,
}

#[derive(Debug, Deserialize, Clone)]
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

#[derive(Debug, Serialize)]
struct IssueCommentRequest {
    body: String,
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

enum SearchResultPr {
    Frozen(PullRequest),
    Light { repo: String, pr: PullRequest },
}

fn build_pull_request_from_detail(
    repo: &str,
    number: u64,
    detail: PullDetail,
) -> Result<PullRequest> {
    let repo_full_name = detail
        .base
        .repo
        .as_ref()
        .map(|repo| repo.full_name.clone())
        .unwrap_or_else(|| repo.to_owned());
    let head_repo = detail
        .head
        .repo
        .clone()
        .or_else(|| detail.base.repo.clone())
        .ok_or_else(|| anyhow!("pull request {} is missing repository metadata", number))?;

    Ok(PullRequest {
        key: format!("{repo_full_name}#{number}"),
        repo_full_name,
        number,
        title: detail.title,
        body: detail.body,
        url: detail.html_url,
        author_login: detail.user.login,
        labels: detail.labels.into_iter().map(|label| label.name).collect(),
        created_at: detail.created_at,
        updated_at: detail.updated_at,
        head_sha: detail.head.sha,
        head_ref: detail.head.reference,
        base_sha: detail.base.sha,
        base_ref: detail.base.reference,
        clone_url: head_repo.clone_url,
        ssh_url: head_repo.ssh_url,
        ci_status: CiStatus::Unknown,
        ci_updated_at: None,
        review_decision: ReviewDecision::Clean,
        approval_count: 0,
        review_comment_count: 0,
        issue_comment_count: 0,
        latest_reviewer_activity_at: None,
        has_conflicts: detail.mergeable == Some(false)
            || detail
                .mergeable_state
                .as_deref()
                .map(|value| value.eq_ignore_ascii_case("dirty"))
                .unwrap_or(false),
        mergeable_state: detail.mergeable_state,
        is_draft: detail.draft,
        is_closed: detail.state.eq_ignore_ascii_case("closed") && detail.merged_at.is_none(),
        is_merged: detail.merged_at.is_some(),
    })
}

fn build_pull_request_from_search_item(repo: &str, item: SearchItem) -> PullRequest {
    PullRequest {
        key: format!("{repo}#{}", item.number),
        repo_full_name: repo.to_owned(),
        number: item.number,
        title: item.title,
        body: item.body,
        url: item.html_url,
        author_login: item.user.login,
        labels: item.labels.into_iter().map(|label| label.name).collect(),
        created_at: item.created_at,
        updated_at: item.updated_at,
        head_sha: String::new(),
        head_ref: String::new(),
        base_sha: String::new(),
        base_ref: String::new(),
        clone_url: String::new(),
        ssh_url: String::new(),
        ci_status: CiStatus::Unknown,
        ci_updated_at: None,
        review_decision: ReviewDecision::Clean,
        approval_count: 0,
        review_comment_count: 0,
        issue_comment_count: 0,
        latest_reviewer_activity_at: None,
        has_conflicts: false,
        mergeable_state: None,
        is_draft: false,
        is_closed: false,
        is_merged: false,
    }
}

fn apply_signal_summary(
    pr: &mut PullRequest,
    review_summary: ReviewSummary,
    ci_summary: CiSummary,
) {
    pr.ci_status = ci_summary.status;
    pr.ci_updated_at = ci_summary.updated_at;
    pr.review_decision = review_summary.decision;
    pr.approval_count = review_summary.approval_count;
    pr.review_comment_count = review_summary.review_comment_count;
    pr.issue_comment_count = review_summary.issue_comment_count;
    pr.latest_reviewer_activity_at = review_summary.latest_activity_at;
}

fn reuse_frozen_pull_request(
    pr_key: &str,
    frozen_pr_keys: &HashSet<String>,
    previous_prs: &HashMap<String, PullRequest>,
) -> Option<PullRequest> {
    frozen_pr_keys
        .contains(pr_key)
        .then(|| previous_prs.get(pr_key).cloned())
        .flatten()
}

fn reuse_cached_signal_details(mut pr: PullRequest, previous: &PullRequest) -> PullRequest {
    pr.ci_status = previous.ci_status;
    pr.ci_updated_at = previous.ci_updated_at;
    pr.review_decision = previous.review_decision;
    pr.approval_count = previous.approval_count;
    pr.review_comment_count = previous.review_comment_count;
    pr.issue_comment_count = previous.issue_comment_count;
    pr.latest_reviewer_activity_at = previous.latest_reviewer_activity_at;
    pr
}

fn should_refresh_signal_details(current: &PullRequest, previous: Option<&PullRequest>) -> bool {
    let Some(previous) = previous else {
        return true;
    };

    if previous.updated_at != current.updated_at
        || previous.head_sha != current.head_sha
        || previous.head_ref != current.head_ref
        || previous.base_sha != current.base_sha
        || previous.base_ref != current.base_ref
        || previous.has_conflicts != current.has_conflicts
        || previous.mergeable_state != current.mergeable_state
        || previous.is_draft != current.is_draft
        || previous.is_closed != current.is_closed
        || previous.is_merged != current.is_merged
    {
        return true;
    }

    if matches!(
        previous.ci_status,
        CiStatus::Pending | CiStatus::Failure | CiStatus::Unknown
    ) {
        return true;
    }

    previous
        .ci_updated_at
        .map(|updated_at| {
            (Utc::now() - updated_at).num_seconds() <= SIGNAL_REFRESH_GRACE_PERIOD_SECS
        })
        .unwrap_or(false)
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

fn summarize_ci(check_runs: &[CheckRun]) -> CiSummary {
    let (status, updated_at) = collapse_check_runs(check_runs);
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

fn parse_pr_key(pr_key: &str) -> Result<(String, u64)> {
    let (repo, number) = pr_key
        .rsplit_once('#')
        .ok_or_else(|| anyhow!("unsupported PR key format: {pr_key}"))?;
    let number = number
        .parse::<u64>()
        .with_context(|| format!("failed to parse PR number from {pr_key}"))?;

    if repo.trim().is_empty() {
        return Err(anyhow!("unsupported PR key format: {pr_key}"));
    }

    Ok((repo.to_owned(), number))
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use chrono::{Duration, TimeZone, Utc};

    use super::{
        build_pull_request_from_search_item, reuse_cached_signal_details,
        reuse_frozen_pull_request, should_refresh_signal_details, summarize_ci, CheckRun,
        GitHubUser, Label, SearchItem,
    };
    use crate::model::{CiStatus, PullRequest, ReviewDecision};

    fn sample_pull_request() -> PullRequest {
        PullRequest {
            key: "openai/symphony#7".to_owned(),
            repo_full_name: "openai/symphony".to_owned(),
            number: 7,
            title: "Improve polling".to_owned(),
            body: Some("Reduce GitHub load".to_owned()),
            url: "https://github.com/openai/symphony/pull/7".to_owned(),
            author_login: "author".to_owned(),
            labels: vec!["automation".to_owned()],
            created_at: chrono::Utc.with_ymd_and_hms(2026, 3, 31, 18, 0, 0).unwrap(),
            updated_at: chrono::Utc
                .with_ymd_and_hms(2026, 3, 31, 18, 30, 0)
                .unwrap(),
            head_sha: "abc123".to_owned(),
            head_ref: "feature/polling".to_owned(),
            base_sha: "def456".to_owned(),
            base_ref: "main".to_owned(),
            clone_url: "https://github.com/openai/symphony.git".to_owned(),
            ssh_url: "git@github.com:openai/symphony.git".to_owned(),
            ci_status: CiStatus::Success,
            ci_updated_at: Some(
                chrono::Utc
                    .with_ymd_and_hms(2026, 3, 31, 18, 25, 0)
                    .unwrap(),
            ),
            review_decision: ReviewDecision::Approved,
            approval_count: 1,
            review_comment_count: 0,
            issue_comment_count: 0,
            latest_reviewer_activity_at: Some(
                chrono::Utc
                    .with_ymd_and_hms(2026, 3, 31, 18, 20, 0)
                    .unwrap(),
            ),
            has_conflicts: false,
            mergeable_state: Some("clean".to_owned()),
            is_draft: false,
            is_closed: false,
            is_merged: false,
        }
    }

    #[test]
    fn unchanged_stable_pr_reuses_cached_signal_details() {
        let now = chrono::Utc::now();
        let previous = PullRequest {
            ci_updated_at: Some(now - Duration::hours(2)),
            ..sample_pull_request()
        };
        let current = previous.clone();

        assert!(
            !should_refresh_signal_details(&current, Some(&previous)),
            "unchanged PRs with stale-enough passed CI should reuse cached signal details",
        );
    }

    #[test]
    fn updated_pr_forces_signal_refresh() {
        let previous = sample_pull_request();
        let mut current = previous.clone();
        current.updated_at = previous.updated_at + Duration::minutes(5);

        assert!(
            should_refresh_signal_details(&current, Some(&previous)),
            "updated PR metadata should force a fresh review/CI hydration",
        );
    }

    #[test]
    fn unstable_ci_keeps_refreshing_even_without_pr_metadata_changes() {
        let previous = PullRequest {
            ci_status: CiStatus::Pending,
            ..sample_pull_request()
        };
        let current = previous.clone();

        assert!(
            should_refresh_signal_details(&current, Some(&previous)),
            "pending CI should remain a candidate until it settles",
        );
    }

    #[test]
    fn reusing_cached_signal_details_preserves_lightweight_pr_fields() {
        let previous = sample_pull_request();
        let mut current = sample_pull_request();
        current.title = "New title from light detail".to_owned();
        current.body = Some("Light detail body".to_owned());
        current.review_decision = ReviewDecision::Clean;
        current.approval_count = 0;
        current.ci_status = CiStatus::Unknown;
        current.ci_updated_at = None;
        current.latest_reviewer_activity_at = None;

        let merged = reuse_cached_signal_details(current, &previous);

        assert_eq!(merged.title, "New title from light detail");
        assert_eq!(merged.body.as_deref(), Some("Light detail body"));
        assert_eq!(merged.review_decision, previous.review_decision);
        assert_eq!(merged.approval_count, previous.approval_count);
        assert_eq!(merged.ci_status, previous.ci_status);
        assert_eq!(merged.ci_updated_at, previous.ci_updated_at);
        assert_eq!(
            merged.latest_reviewer_activity_at,
            previous.latest_reviewer_activity_at
        );
    }

    #[test]
    fn frozen_pr_reuses_the_previous_snapshot() {
        let previous = sample_pull_request();
        let previous_prs = HashMap::from([(previous.key.clone(), previous.clone())]);
        let frozen_pr_keys = HashSet::from([previous.key.clone()]);

        let reused = reuse_frozen_pull_request(&previous.key, &frozen_pr_keys, &previous_prs)
            .expect("frozen PR should reuse its previous snapshot");

        assert_eq!(reused.title, previous.title);
        assert_eq!(reused.ci_status, previous.ci_status);
        assert_eq!(reused.review_decision, previous.review_decision);
    }

    #[test]
    fn summarize_ci_uses_check_runs_as_the_source_of_truth() {
        let check_runs = vec![
            CheckRun {
                status: "completed".to_owned(),
                conclusion: Some("success".to_owned()),
                started_at: Some(Utc.with_ymd_and_hms(2026, 4, 1, 6, 9, 10).unwrap()),
                completed_at: Some(Utc.with_ymd_and_hms(2026, 4, 1, 6, 10, 11).unwrap()),
            },
            CheckRun {
                status: "completed".to_owned(),
                conclusion: Some("success".to_owned()),
                started_at: Some(Utc.with_ymd_and_hms(2026, 4, 1, 6, 9, 10).unwrap()),
                completed_at: Some(Utc.with_ymd_and_hms(2026, 4, 1, 6, 15, 58).unwrap()),
            },
        ];

        let summary = summarize_ci(&check_runs);

        assert_eq!(summary.status, CiStatus::Success);
        assert_eq!(
            summary.updated_at,
            Some(Utc.with_ymd_and_hms(2026, 4, 1, 6, 15, 58).unwrap())
        );
    }

    #[test]
    fn build_pull_request_from_search_item_keeps_review_requests_lightweight() {
        let pr = build_pull_request_from_search_item(
            "openai/symphony",
            SearchItem {
                number: 18,
                repository_url: "https://api.github.com/repos/openai/symphony".to_owned(),
                html_url: "https://github.com/openai/symphony/pull/18".to_owned(),
                title: "Review me".to_owned(),
                body: Some("Please review".to_owned()),
                user: GitHubUser {
                    login: "reviewer".to_owned(),
                },
                labels: vec![Label {
                    name: "needs-review".to_owned(),
                }],
                created_at: Utc.with_ymd_and_hms(2026, 4, 1, 1, 2, 3).unwrap(),
                updated_at: Utc.with_ymd_and_hms(2026, 4, 1, 4, 5, 6).unwrap(),
            },
        );

        assert_eq!(pr.key, "openai/symphony#18");
        assert_eq!(pr.repo_full_name, "openai/symphony");
        assert_eq!(pr.title, "Review me");
        assert_eq!(pr.ci_status, CiStatus::Unknown);
        assert_eq!(pr.review_decision, ReviewDecision::Clean);
        assert!(pr.head_sha.is_empty());
        assert!(pr.clone_url.is_empty());
    }
}
