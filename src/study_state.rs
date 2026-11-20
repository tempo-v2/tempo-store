//! Consistent Study command reads with explicit current-target coverage.
use crate::{
    AssessmentRecord, Receipt, Snapshot, StoreError, StoredAttempt, StudyScope,
    store_error::invalid,
};
use std::collections::{BTreeMap, BTreeSet};
use tempo_chess::{Catalog, ContentItem};
use tempo_learning::{LearningState, SchedulerSettings};
use tempo_types::{AttemptId, DatasetId, Revision, SubmissionId, TargetId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetCoverage {
    Complete,
    Selected(BTreeSet<TargetId>),
}

/// Current state and only the attempts, assessments, and receipt addressed by the read.
/// The active attempt and its target are included even when omitted from the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudyState {
    pub dataset: DatasetId,
    pub revision: Revision,
    pub policy: SchedulerSettings,
    pub suspended_repertoires: BTreeSet<String>,
    pub active_attempt: Option<AttemptId>,
    pub attempts: BTreeMap<AttemptId, StoredAttempt>,
    pub assessments: Vec<AssessmentRecord>,
    pub receipts: BTreeMap<SubmissionId, Receipt>,
    pub(crate) coverage: TargetCoverage,
    pub(crate) catalog: Catalog,
    pub(crate) learning: BTreeMap<TargetId, LearningState>,
}

impl StudyState {
    pub fn target_coverage(&self) -> &TargetCoverage {
        &self.coverage
    }

    /// A selected read returns an error for an unrequested target and None for
    /// a requested target that is absent from the current catalog.
    pub fn target(
        &self,
        id: &TargetId,
    ) -> Result<Option<(&ContentItem, &LearningState)>, StoreError> {
        if matches!(&self.coverage, TargetCoverage::Selected(ids) if !ids.contains(id)) {
            return Err(invalid("target was not requested in this Study read"));
        }
        match (self.catalog.items.get(id), self.learning.get(id)) {
            (Some(item), Some(learning)) => Ok(Some((item, learning))),
            (None, None) => Ok(None),
            _ => Err(invalid("current target and Learning state disagree")),
        }
    }

    /// Read complete current content and Learning for selection.
    pub fn complete_catalog(
        &self,
    ) -> Result<(&Catalog, &BTreeMap<TargetId, LearningState>), StoreError> {
        if self.coverage != TargetCoverage::Complete {
            return Err(invalid(
                "complete catalog was not requested in this Study read",
            ));
        }
        Ok((&self.catalog, &self.learning))
    }

    /// Project from an adapter's already consistent, complete snapshot.
    pub fn from_snapshot(snapshot: &Snapshot, scope: StudyScope<'_>) -> Self {
        let attempts: BTreeMap<_, _> = snapshot
            .attempts
            .iter()
            .filter(|(id, _)| {
                snapshot.active_attempt.as_ref() == Some(*id) || scope.attempt == Some(*id)
            })
            .map(|(id, attempt)| (id.clone(), attempt.clone()))
            .collect();
        let coverage = if scope.catalog {
            TargetCoverage::Complete
        } else {
            TargetCoverage::Selected(
                attempts
                    .values()
                    .map(|row| row.attempt.target.clone())
                    .chain(scope.target.cloned())
                    .collect(),
            )
        };
        let requested = |id: &TargetId| match &coverage {
            TargetCoverage::Complete => true,
            TargetCoverage::Selected(ids) => ids.contains(id),
        };
        let catalog = Catalog {
            items: snapshot
                .catalog
                .items
                .iter()
                .filter(|(id, _)| requested(id))
                .map(|(id, item)| (id.clone(), item.clone()))
                .collect(),
            lines: if scope.catalog {
                snapshot.catalog.lines.clone()
            } else {
                Vec::new()
            },
        };
        let learning = snapshot
            .learning
            .iter()
            .filter(|(id, _)| requested(id))
            .map(|(id, state)| (id.clone(), state.clone()))
            .collect();
        let assessments = snapshot.assessments.iter().filter(|record| {
            matches!(&record.key, crate::AssessmentKey::Human(id) if attempts.contains_key(id))
        }).cloned().collect();
        let receipts = scope
            .submission
            .and_then(|id| snapshot.receipts.get_key_value(id))
            .map(|(id, receipt)| (id.clone(), receipt.clone()))
            .into_iter()
            .collect();
        Self {
            dataset: snapshot.dataset.clone(),
            revision: snapshot.revision,
            policy: snapshot.policy.clone(),
            suspended_repertoires: BTreeSet::new(),
            active_attempt: snapshot.active_attempt.clone(),
            attempts,
            assessments,
            receipts,
            coverage,
            catalog,
            learning,
        }
    }
}
