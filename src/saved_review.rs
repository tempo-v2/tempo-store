//! Attempt-linked references for the desktop review list.
use crate::{ChessAttempt, RevealedDecision, StoredAttempt};
use serde::Serialize;
use tempo_types::{AttemptId, ContentVersion, DatasetId, Revision, TargetId};

#[derive(Clone, Debug, Serialize)]
pub struct SavedReview {
    pub id: i64,
    pub exercise_name: Option<String>,
    pub decision_index: usize,
    pub saved_at_ms: u64,
    pub can_remove_from_study: bool,
    pub removed_from_study: bool,
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
    /// Apply the prepared exclusion and, if this tactic is active, end its attempt.
    ExcludeTactic {
        id: i64,
        expected_revision: Revision,
        abandoned: Option<Box<ChessAttempt>>,
    },
}

/// A settled retry or the addressed state Tempo needs to prepare the transition.
pub enum TacticExclusionRead {
    Accepted(i64),
    Pending {
        revision: Revision,
        active: Option<Box<ChessAttempt>>,
    },
}

/// Disclosure inputs read together for an addressed mark.
pub struct SavedReviewContext {
    pub decision_index: usize,
    pub source: StoredAttempt,
    pub active: Option<StoredAttempt>,
    pub excluded_from_study: bool,
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
