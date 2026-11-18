//! Write and its usage scenarios.

use crate::{AssessmentRecord, Receipt, StoredAttempt};
use std::collections::BTreeMap;
use tempo_chess::Catalog;
use tempo_learning::{LearningState, SchedulerSettings};
use tempo_types::{AttemptId, DatasetId, TargetId};

/// One typed persistence request within an atomic application batch.
///
/// # When to use
///
/// The application translates domain results into these values and places them in
/// [`crate::Commit::writes`]. Domain functions return their own values; they never
/// construct SQL or receive a Store handle.
///
/// Progress, a newly emitted assessment, its Learning state and the submission's
/// receipt belong in the same batch. A single write is not necessarily valid by
/// itself: Store checks the final staged snapshot before publishing anything.
///
/// `Catalog` is an ordinary import/update and cannot silently delete content.
/// `Restore` installs a new dataset identity and replaces current content, learning
/// and policy together while keeping history. The application separately validates
/// restore eligibility and policy compatibility.
#[derive(Clone, Debug)]
pub enum Write {
    /// Insert a newly issued attempt with immutable issue facts.
    Issue(StoredAttempt),
    /// Replace progress for an existing active attempt, advancing its revision once.
    Progress(StoredAttempt),
    /// Save the resulting Learning state, including opaque scheduler memory.
    Learning {
        target: TargetId,
        state: LearningState,
    },
    /// Append one unique assessment and its proposed Learning transition.
    Assessment(AssessmentRecord),
    /// Preserve the exact command reply for idempotent submission replay.
    Receipt(Receipt),
    /// Replace current catalog after ordinary import; retain version history.
    Catalog(Catalog),
    /// Select the open attempt, or clear the active pointer after closing it.
    Active(Option<AttemptId>),
    /// Replace current data under a never-used dataset ID, retaining old history.
    Restore {
        dataset: DatasetId,
        catalog: Catalog,
        learning: BTreeMap<TargetId, LearningState>,
        policy: SchedulerSettings,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{policy, store};
    use crate::{Commit, Store, StoreError};
    use tempo_learning::LearningPolicy;

    #[test]
    fn empty_dataset_identities_remain_retired_after_restore() {
        let mut store = store(Catalog::default());
        let initial = store.snapshot().unwrap();
        store
            .commit(Commit {
                expected_revision: initial.revision,
                expected_dataset: initial.dataset,
                writes: vec![Write::Restore {
                    dataset: "dataset-b".into(),
                    catalog: Catalog::default(),
                    learning: BTreeMap::new(),
                    policy: policy().settings(),
                }],
            })
            .unwrap();
        let before = store.snapshot().unwrap();
        assert!(before.content_history.is_empty());
        assert_eq!(before.known_datasets.len(), 2);
        assert!(matches!(
            store.commit(Commit {
                expected_revision: before.revision,
                expected_dataset: before.dataset.clone(),
                writes: vec![Write::Restore {
                    dataset: "dataset-a".into(),
                    catalog: Catalog::default(),
                    learning: BTreeMap::new(),
                    policy: policy().settings(),
                }],
            }),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(store.snapshot().unwrap(), before);
    }
}
