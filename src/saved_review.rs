//! Attempt-linked references for the desktop review list.
use crate::{RevealedDecision, StoredAttempt};
use serde::Serialize;
use tempo_types::{AttemptId, ContentVersion, DatasetId, TargetId};

#[derive(Clone, Debug, Serialize)]
pub struct SavedReview {
    pub id: i64,
    pub exercise_name: Option<String>,
    pub decision_index: usize,
    pub saved_at_ms: u64,
}

pub struct ReviewPosition<'a> {
    pub attempt: &'a AttemptId,
    pub target: &'a TargetId,
    pub version: ContentVersion,
    pub decision_index: usize,
    pub saved_at_ms: u64,
}

pub enum ReviewWrite<'a> {
    Save(ReviewPosition<'a>),
    Remove(i64),
}

/// Disclosure inputs read together for an addressed mark.
pub struct SavedReviewContext {
    pub decision_index: usize,
    pub source: StoredAttempt,
    pub active: Option<StoredAttempt>,
    /// Whether a later attempt of this issued version completed.
    pub later_completed: bool,
    /// Highest explicit reveal among later abandoned attempts of this issued version.
    pub later_abandoned_reveal: Option<RevealedDecision>,
    /// Compatibility fallback for attempts written before explicit reveal tracking.
    pub legacy_source_feedback: bool,
}

pub struct ReviewCommandKey<'a> {
    pub dataset: &'a DatasetId,
    pub submission: &'a str,
    pub fingerprint: &'a str,
}
