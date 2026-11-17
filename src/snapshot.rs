//! Snapshot and its usage scenarios.

use crate::{AssessmentRecord, ContentHistory, Receipt, StoredAttempt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use tempo_chess::Catalog;
use tempo_learning::{LearningState, SchedulerSettings};
use tempo_types::{AttemptId, DatasetId, Revision, SubmissionId, TargetId};

/// A consistent owned view of everything needed to compose an application command.
///
/// # When to use
///
/// [`crate::Store::snapshot`] produces this value. The application reads content,
/// attempts and learning from the same revision, asks domain functions for pure
/// transitions, then submits a [`crate::Commit`] stamped with this revision and
/// dataset.
///
/// Editing this owned value does not persist anything. A [`crate::Write`] requests
/// one change and a commit publishes a batch atomically. Another successful commit
/// makes this snapshot's optimistic revision stale.
///
/// The catalog, learning and policy are current. Attempts, assessments, receipts
/// and content history can include earlier content versions or datasets. Those
/// historical records remain available for interpretation and retry replay.
///
/// ```
/// use tempo_chess::demo_catalog;
/// use tempo_learning::{LearningPolicy, StandardLearning};
/// use tempo_store::{MemoryStore, Store};
///
/// let policy: StandardLearning = StandardLearning::default();
/// let store = MemoryStore::new("dataset-a".into(), demo_catalog(), policy.settings())?;
/// let mut owned_view = store.snapshot()?;
/// owned_view.catalog.items.clear(); // A local edit only.
/// assert_eq!(store.snapshot()?.catalog.items.len(), 3);
/// # Ok::<(), tempo_store::StoreError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision: Revision,
    pub dataset: DatasetId,
    /// Dataset identities are never reused, including those with empty catalogs.
    #[serde(default)]
    pub known_datasets: BTreeSet<DatasetId>,
    pub catalog: Catalog,
    pub learning: BTreeMap<TargetId, LearningState>,
    pub policy: SchedulerSettings,
    pub attempts: BTreeMap<AttemptId, StoredAttempt>,
    pub active_attempt: Option<AttemptId>,
    pub assessments: Vec<AssessmentRecord>,
    pub receipts: BTreeMap<SubmissionId, Receipt>,
    pub content_history: Vec<ContentHistory>,
}

#[cfg(test)]
mod tests {
    use crate::test_support::{issued_record, store};
    use crate::{Commit, Store, Write};
    use tempo_chess::demo_catalog;

    #[test]
    fn an_owned_snapshot_stays_consistent_when_later_commits_advance_the_store() {
        let mut store = store(demo_catalog());
        let before = store.snapshot().unwrap();
        let mut local_edit = before.clone();
        local_edit.catalog.items.clear();
        assert_eq!(store.snapshot().unwrap(), before);
        store
            .commit(Commit {
                expected_revision: before.revision,
                expected_dataset: before.dataset.clone(),
                writes: vec![
                    Write::Issue(issued_record(&before)),
                    Write::Active(Some("attempt-1".into())),
                ],
            })
            .unwrap();
        let after = store.snapshot().unwrap();
        assert!(before.attempts.is_empty());
        assert_eq!(after.attempts.len(), 1);
        assert!(after.revision > before.revision);
        assert_eq!(before.catalog, after.catalog);
    }
}
