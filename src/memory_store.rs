//! MemoryStore and its usage scenarios.

use crate::transaction::{initial_snapshot, stage_commit};
use crate::{Commit, Snapshot, Store, StoreError};
use tempo_chess::Catalog;
use tempo_learning::SchedulerSettings;
use tempo_types::{DatasetId, Revision};

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
    fail_next_commit: bool,
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
        })
    }
    /// Fault injection after staging, before publishing, for transaction contract tests.
    pub fn fail_next_commit(&mut self) {
        self.fail_next_commit = true;
    }
}

impl Store for MemoryStore {
    fn snapshot(&self) -> Result<Snapshot, StoreError> {
        Ok(self.state.clone())
    }
    fn commit(&mut self, commit: Commit) -> Result<Revision, StoreError> {
        let staged = stage_commit(&self.state, commit)?;
        if std::mem::take(&mut self.fail_next_commit) {
            return Err(StoreError::Unavailable("injected commit failure".into()));
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
