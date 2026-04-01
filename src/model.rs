use std::collections::{BTreeMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const MAX_ACTIVITY_EVENTS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CiStatus {
    Success,
    Pending,
    Failure,
    Unknown,
}

impl CiStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Success => "passed",
            Self::Pending => "pending",
            Self::Failure => "failing",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewDecision {
    Clean,
    Commented,
    ChangesRequested,
    Approved,
}

impl ReviewDecision {
    pub fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Commented => "commented",
            Self::ChangesRequested => "changes requested",
            Self::Approved => "approved",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttentionReason {
    CiFailed,
    ReviewFeedback,
    MergeConflict,
    DeepReview,
}

impl AttentionReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::CiFailed => "CI failed",
            Self::ReviewFeedback => "new review feedback",
            Self::MergeConflict => "merge conflict with base branch",
            Self::DeepReview => "manual deep review",
        }
    }

    pub fn active_summary(self) -> &'static str {
        match self {
            Self::CiFailed => "investigating CI failure",
            Self::ReviewFeedback => "addressing review feedback",
            Self::MergeConflict => "resolving merge conflict",
            Self::DeepReview => "running deep review",
        }
    }

    pub fn success_summary(self) -> &'static str {
        match self {
            Self::CiFailed => "CI failure handling completed",
            Self::ReviewFeedback => "review feedback handling completed",
            Self::MergeConflict => "merge conflict handling completed",
            Self::DeepReview => "deep review completed",
        }
    }

    pub fn failure_summary(self) -> &'static str {
        match self {
            Self::CiFailed => "CI failure handling failed",
            Self::ReviewFeedback => "review feedback handling failed",
            Self::MergeConflict => "merge conflict handling failed",
            Self::DeepReview => "deep review failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackingStatus {
    Draft,
    Paused,
    Conflict,
    WaitingCi,
    WaitingReview,
    WaitingMerge,
    NeedsAttention,
    Running,
    RetryScheduled,
    Blocked,
    Closed,
    Merged,
}

impl TrackingStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Paused => "paused",
            Self::Conflict => "conflict",
            Self::WaitingCi => "waiting for CI",
            Self::WaitingReview => "waiting review",
            Self::WaitingMerge => "waiting merge",
            Self::NeedsAttention => "needs attention",
            Self::Running => "running",
            Self::RetryScheduled => "retrying",
            Self::Blocked => "blocked",
            Self::Closed => "closed",
            Self::Merged => "merged",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub key: String,
    pub repo_full_name: String,
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub url: String,
    pub author_login: String,
    pub labels: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub head_sha: String,
    pub head_ref: String,
    pub base_sha: String,
    pub base_ref: String,
    pub clone_url: String,
    pub ssh_url: String,
    pub ci_status: CiStatus,
    pub ci_updated_at: Option<DateTime<Utc>>,
    pub review_decision: ReviewDecision,
    pub approval_count: usize,
    pub review_comment_count: usize,
    pub issue_comment_count: usize,
    pub latest_reviewer_activity_at: Option<DateTime<Utc>>,
    pub has_conflicts: bool,
    pub mergeable_state: Option<String>,
    pub is_draft: bool,
    pub is_closed: bool,
    pub is_merged: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistentPrState {
    #[serde(default)]
    pub paused: bool,
    pub last_processed_review_comment_at: Option<DateTime<Utc>>,
    pub last_processed_ci_signal_at: Option<DateTime<Utc>>,
    pub last_processed_ci_head_sha: Option<String>,
    pub last_processed_conflict_head_sha: Option<String>,
    pub last_processed_conflict_base_sha: Option<String>,
    // Legacy generic markers retained so older state files can be read safely.
    pub last_processed_comment_at: Option<DateTime<Utc>>,
    pub last_processed_ci_at: Option<DateTime<Utc>>,
    pub last_processed_head_sha: Option<String>,
    pub last_processed_base_sha: Option<String>,
    pub last_run_started_at: Option<DateTime<Utc>>,
    pub last_run_finished_at: Option<DateTime<Utc>>,
    pub last_run_status: Option<String>,
    pub last_run_summary: Option<String>,
    pub last_run_output: Option<String>,
    pub last_run_terminal: Option<String>,
    pub last_terminal_output_at: Option<DateTime<Utc>>,
    pub last_run_trigger: Option<AttentionReason>,
    #[serde(default)]
    pub consecutive_failures: u32,
    pub retry_trigger: Option<AttentionReason>,
    pub retry_head_sha: Option<String>,
    pub retry_base_sha: Option<String>,
    pub retry_comment_at: Option<DateTime<Utc>>,
    pub retry_ci_at: Option<DateTime<Utc>>,
}

impl PersistentPrState {
    pub fn clear_retry_state(&mut self) {
        self.consecutive_failures = 0;
        self.retry_trigger = None;
        self.retry_head_sha = None;
        self.retry_base_sha = None;
        self.retry_comment_at = None;
        self.retry_ci_at = None;
    }

    pub fn processed_review_comment_at(&self) -> Option<DateTime<Utc>> {
        self.last_processed_review_comment_at.clone().or_else(|| {
            if self.legacy_marker_matches(AttentionReason::ReviewFeedback) {
                self.last_processed_comment_at.clone()
            } else {
                None
            }
        })
    }

    pub fn processed_ci_signal_at(&self) -> Option<DateTime<Utc>> {
        self.last_processed_ci_signal_at.clone().or_else(|| {
            if self.legacy_marker_matches(AttentionReason::CiFailed) {
                self.last_processed_ci_at.clone()
            } else {
                None
            }
        })
    }

    pub fn processed_ci_head_sha(&self) -> Option<&str> {
        self.last_processed_ci_head_sha.as_deref().or_else(|| {
            if self.legacy_marker_matches(AttentionReason::CiFailed) {
                self.last_processed_head_sha.as_deref()
            } else {
                None
            }
        })
    }

    pub fn processed_conflict_head_sha(&self) -> Option<&str> {
        self.last_processed_conflict_head_sha
            .as_deref()
            .or_else(|| {
                if self.legacy_marker_matches(AttentionReason::MergeConflict) {
                    self.last_processed_head_sha.as_deref()
                } else {
                    None
                }
            })
    }

    pub fn processed_conflict_base_sha(&self) -> Option<&str> {
        self.last_processed_conflict_base_sha
            .as_deref()
            .or_else(|| {
                if self.legacy_marker_matches(AttentionReason::MergeConflict) {
                    self.last_processed_base_sha.as_deref()
                } else {
                    None
                }
            })
    }

    fn legacy_marker_matches(&self, trigger: AttentionReason) -> bool {
        self.last_run_status.as_deref() == Some("success") && self.last_run_trigger == Some(trigger)
    }
}

#[derive(Debug, Clone)]
pub struct RunnerState {
    pub status: TrackingStatus,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub attempt: u32,
    pub trigger: AttentionReason,
    pub summary: String,
    pub live_output: Option<String>,
    pub live_terminal: Option<String>,
    pub last_terminal_output_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct TrackedPr {
    pub pull_request: PullRequest,
    pub status: TrackingStatus,
    pub attention_reason: Option<AttentionReason>,
    pub persisted: PersistentPrState,
    pub runner: Option<RunnerState>,
}

#[derive(Debug, Clone)]
pub struct ReviewRequestPr {
    pub pull_request: PullRequest,
    pub persisted: PersistentPrState,
    pub runner: Option<RunnerState>,
}

#[derive(Debug, Clone, Copy)]
pub enum EventLevel {
    Info,
    Error,
}

impl EventLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActivityEvent {
    pub timestamp: DateTime<Utc>,
    pub level: EventLevel,
    pub pr_key: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct DashboardState {
    pub tracked_prs: BTreeMap<String, TrackedPr>,
    pub review_requests: BTreeMap<String, ReviewRequestPr>,
    pub total_matching_prs: Option<usize>,
    pub activity: VecDeque<ActivityEvent>,
    pub last_poll_started_at: Option<DateTime<Utc>>,
    pub last_poll_finished_at: Option<DateTime<Utc>>,
    pub next_poll_due_at: Option<DateTime<Utc>>,
    pub last_poll_error: Option<String>,
}

impl DashboardState {
    pub fn push_event(
        &mut self,
        level: EventLevel,
        pr_key: Option<String>,
        message: impl Into<String>,
    ) {
        self.activity.push_front(ActivityEvent {
            timestamp: Utc::now(),
            level,
            pr_key,
            message: message.into(),
        });

        while self.activity.len() > MAX_ACTIVITY_EVENTS {
            self.activity.pop_back();
        }
    }
}
