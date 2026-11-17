//! MemoryStore and its usage scenarios.

use crate::store_error::{conflict, invalid};
use crate::{Commit, ContentHistory, Snapshot, Store, StoreError, Write};
use std::collections::{BTreeMap, BTreeSet};
use tempo_chess::Catalog;
use tempo_learning::{LearningState, SchedulerSettings};
use tempo_types::{DatasetId, Revision};

mod apply_write;
mod validate_outcomes;
mod validate_snapshot;

use apply_write::apply_write;
use validate_outcomes::validate_new_outcomes;
use validate_snapshot::validate_snapshot;

/// An atomic in-memory implementation of [`Store`] for wiring and contract tests.
///
/// # When to use
///
/// Use this adapter to run the application before a persistent database adapter is
/// available. `new` validates the initial catalog and creates default Learning
/// state for each target. Snapshot returns an owned clone.
///
/// Commit checks optimistic stamps, stages all writes in a clone, validates the
/// final snapshot and new outcomes, and publishes the clone only if everything
/// succeeds. Fault injection happens immediately before publication.
///
/// The store is process-local and has no disk durability. Its staging algorithm is
/// a readable reference for transaction semantics, not an implementation of SQL
/// constraints, migrations or crash recovery.
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
        catalog
            .validate()
            .map_err(|e| StoreError::Invalid(e.to_string()))?;
        let learning = catalog
            .items
            .keys()
            .map(|id| (id.clone(), LearningState::default()))
            .collect();
        let content_history = catalog
            .items
            .values()
            .cloned()
            .map(|item| ContentHistory {
                dataset: dataset.clone(),
                item,
            })
            .collect();
        let known_datasets = BTreeSet::from([dataset.clone()]);
        Ok(Self {
            state: Snapshot {
                revision: Revision(0),
                dataset,
                known_datasets,
                catalog,
                learning,
                policy,
                attempts: BTreeMap::new(),
                active_attempt: None,
                assessments: vec![],
                receipts: BTreeMap::new(),
                content_history,
            },
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
        if commit.expected_revision != self.state.revision
            || commit.expected_dataset != self.state.dataset
        {
            return Err(conflict("snapshot changed; reload before retrying"));
        }
        let mut staged = self.state.clone();
        let assessments = commit
            .writes
            .iter()
            .filter_map(|write| match write {
                Write::Assessment(record) => Some(record.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let receipts = commit
            .writes
            .iter()
            .filter_map(|write| match write {
                Write::Receipt(receipt) => Some(receipt.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        for write in commit.writes {
            apply_write(&mut staged, write)?;
        }
        validate_snapshot(&staged)?;
        validate_new_outcomes(&self.state, &staged, &assessments, &receipts)?;
        staged.revision = Revision(
            staged
                .revision
                .0
                .checked_add(1)
                .ok_or_else(|| invalid("store revision exhausted"))?,
        );
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
