//! MemoryStore and its usage scenarios.

use crate::store_error::conflict;
use crate::transaction::{initial_snapshot, stage_commit};
use crate::{
    Commit, DatasetIdentity, ContentState, PortableGameEvidence, Snapshot, Store, StoreError, StudyOrderingWrite,
    StudyScope, StudyState,
};
use tempo_chess::Catalog;
use tempo_learning::{LearningEffect, SchedulerSettings};
use tempo_types::{AttemptId, ContentVersion, DatasetId, Grade, Revision, TargetId};

/// An atomic in-memory implementation of [`Store`] for wiring and contract tests.
///
/// # When to use
///
/// Use this adapter for a process-local demo or to test the Store contract.
/// `new` validates the catalog and creates default Learning state for each
/// target. Snapshot returns an owned clone.
///
/// Commit checks optimistic stamps, stages all writes in a clone, validates the
/// final snapshot and new outcomes, and publishes the clone only if everything
/// succeeds. Fault injection happens immediately before publication.
///
/// The staging logic is shared with [`crate::SqliteStore`].
pub struct MemoryStore {
    state: Snapshot,
    content_revision: Revision,
    fail_next_commit: bool,
    regular_review_owed: bool,
}
impl MemoryStore {
    pub fn new(
        dataset: DatasetId,
        catalog: Catalog,
        policy: SchedulerSettings,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            state: initial_snapshot(dataset, catalog, policy)?,
            fail_next_commit: false,
            regular_review_owed: false,
            content_revision: Revision(0),
        })
    }
    /// Fault injection after staging, before publishing, for transaction contract tests.
    pub fn fail_next_commit(&mut self) {
        self.fail_next_commit = true;
    }
}

impl DatasetIdentity for MemoryStore {
    fn dataset_id(&self) -> Result<DatasetId, StoreError> {
        Ok(self.state.dataset.clone())
    }
}

impl crate::AddressedResume for MemoryStore {
    fn resume_state(&self, id: &AttemptId) -> Result<crate::ResumeState, StoreError> {
        let record = self.state.attempts.get(id);
        let target = record
            .and_then(|record| self.state.catalog.items.get(&record.attempt.target))
            .map(|item| crate::ResumeTarget {
                version: item.version,
                status: item.status,
                excluded: false,
                suspended: false,
            });
        Ok(crate::ResumeState {
            dataset: self.state.dataset.clone(),
            active_attempt: self.state.active_attempt.clone(),
            record: record.cloned(),
            target,
        })
    }
}

impl Store for MemoryStore {
    fn latest_applied_test(
        &self,
        dataset: &DatasetId,
        target: &TargetId,
        version: ContentVersion,
    ) -> Result<Option<Grade>, StoreError> {
        Ok(self.state.assessments.iter().rev().find_map(|record| {
            let crate::AssessmentKey::Human(id) = &record.key else {
                return None;
            };
            (&record.dataset == dataset
                && &record.target == target
                && record.content_version == version
                && !matches!(
                    record.transition.effect,
                    LearningEffect::PracticeOnly | LearningEffect::StalePositiveIgnored
                )
                && self
                    .state
                    .attempts
                    .get(id)
                    .is_some_and(|attempt| attempt.mode == crate::ActivityMode::Test))
            .then_some(record.input.grade)
        }))
    }
    fn regular_review_owed(&self, dataset: &DatasetId) -> Result<bool, StoreError> {
        if *dataset != self.state.dataset {
            return Err(conflict("daily ordering dataset changed"));
        }
        Ok(self.regular_review_owed)
    }

    fn commit_daily_issuance(
        &mut self,
        commit: Commit,
        _scope: StudyScope<'_>,
        immediate_request: Option<u64>,
        ordering: StudyOrderingWrite,
    ) -> Result<Revision, StoreError> {
        if immediate_request.is_some() {
            return Err(StoreError::Unsupported(
                "bind immediate recall in MemoryStore",
            ));
        }
        commit.daily_issued_attempt()?;
        if self.state.active_attempt.is_some() {
            return Err(conflict("finish the active attempt before daily issuance"));
        }
        let revision = self.commit(commit)?;
        match ordering {
            StudyOrderingWrite::Preserve => {}
            StudyOrderingWrite::OweRegularReview => self.regular_review_owed = true,
            StudyOrderingWrite::SatisfyRegularReview => self.regular_review_owed = false,
        }
        Ok(revision)
    }

    fn has_pending_tactical_request_for_attempt(
        &self,
        dataset: &DatasetId,
        _attempt: &AttemptId,
    ) -> Result<bool, StoreError> {
        if *dataset != self.state.dataset {
            return Err(StoreError::Conflict(
                "tactical recall dataset changed".into(),
            ));
        }
        // This adapter cannot accept tactical analyses or bind their requests.
        Ok(false)
    }

    fn pending_tactical_requests(
        &self,
        dataset: &DatasetId,
    ) -> Result<Vec<crate::TacticalRequest>, StoreError> {
        if *dataset != self.state.dataset {
            return Err(StoreError::Conflict(
                "tactical recall dataset changed".into(),
            ));
        }
        // This adapter cannot accept tactical analyses, so it has no such requests.
        Ok(Vec::new())
    }

    fn snapshot(&self) -> Result<Snapshot, StoreError> {
        Ok(self.state.clone())
    }
    fn study_state(&self, scope: StudyScope<'_>) -> Result<StudyState, StoreError> {
        Ok(StudyState::from_snapshot(&self.state, scope))
    }
    fn content_state(&self) -> Result<ContentState, StoreError> {
        Ok(ContentState::from_snapshot(
            &self.state,
            self.content_revision,
        ))
    }
    fn repertoire_reset_at(
        &self,
        _dataset: &DatasetId,
        _repertoire: &str,
    ) -> Result<Option<u64>, StoreError> {
        // Standalone Study cannot accept reset records through any write.
        Ok(None)
    }
    fn portable_state(&self) -> Result<(Snapshot, PortableGameEvidence), StoreError> {
        // This adapter cannot accept game evidence, including through restore.
        Ok((self.state.clone(), PortableGameEvidence::default()))
    }
    fn commit(&mut self, commit: Commit) -> Result<Revision, StoreError> {
        let staged = stage_commit(&self.state, commit)?;
        if std::mem::take(&mut self.fail_next_commit) {
            return Err(StoreError::Unavailable("injected commit failure".into()));
        }
        if self.state.dataset != staged.dataset || self.state.catalog != staged.catalog {
            self.content_revision = Revision(
                self.content_revision
                    .0
                    .checked_add(1)
                    .ok_or_else(|| StoreError::Invalid("content revision exhausted".into()))?,
            );
        }
        if self.state.dataset != staged.dataset {
            self.regular_review_owed = false;
        }
        self.state = staged;
        Ok(self.state.revision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Write;
    use crate::test_support::{issued_record, store, successful_answer_writes};
    use tempo_chess::demo_catalog;
    use tempo_types::{AttemptId, Grade, StudyDay, TargetId};

    #[test]
    fn failed_commit_publishes_nothing_and_same_batch_can_retry() {
        let mut store = store(demo_catalog());
        let initial = store.snapshot().unwrap();
        let issued = issued_record(&initial);
        store
            .commit(Commit {
                expected_revision: initial.revision,
                expected_dataset: initial.dataset,
                writes: vec![
                    Write::Issue(issued),
                    Write::Active(Some("attempt-1".into())),
                ],
            })
            .unwrap();
        let before = store.snapshot().unwrap();
        let batch = Commit {
            expected_revision: before.revision,
            expected_dataset: before.dataset.clone(),
            writes: successful_answer_writes(&before),
        };
        store.fail_next_commit();
        assert!(matches!(
            store.commit(batch.clone()),
            Err(StoreError::Unavailable(_))
        ));
        assert_eq!(store.snapshot().unwrap(), before);
        // Failure happened before publication, so exactly the same batch is still valid.
        store.commit(batch).unwrap();
        let after = store.snapshot().unwrap();
        assert_eq!(after.assessments.len(), 1);
        assert_eq!(after.receipts.len(), 1);
        assert_eq!(
            after.learning[&TargetId::from("r1:p1")].due,
            Some(StudyDay(13))
        );
        assert_eq!(
            after.attempts[&AttemptId::from("attempt-1")]
                .attempt
                .terminal,
            Some(Grade::Good)
        );
    }
}
