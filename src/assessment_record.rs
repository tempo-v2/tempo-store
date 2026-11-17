//! AssessmentRecord and its usage scenarios.

use crate::AssessmentKey;
use serde::{Deserialize, Serialize};
use tempo_learning::{AssessmentInput, Transition};
use tempo_types::{ContentVersion, DatasetId, TargetId};

/// The durable link from an assessment input to its Learning transition.
///
/// # When to use
///
/// After Study emits a new grade, or a provider observation becomes an external
/// assessment, the application asks Learning for a transition and constructs this
/// record. It commits the record with the transition's resulting learning state.
/// Store verifies identity/source agreement and the matching final learning state.
///
/// This record is learning history. A [`crate::Receipt`] instead retains the exact
/// reply to a submission, including commands that emit no new assessment. Old
/// records keep their dataset, content version and transition even when current
/// content or learning changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentRecord {
    pub key: AssessmentKey,
    pub dataset: DatasetId,
    pub target: TargetId,
    pub content_version: ContentVersion,
    pub input: AssessmentInput,
    pub transition: Transition,
}

#[cfg(test)]
mod tests {
    use crate::test_support::{issued_record, store, successful_answer_writes};
    use crate::{Commit, Store, StoreError, Write};
    use tempo_chess::demo_catalog;

    #[test]
    fn terminal_assessment_cannot_commit_without_its_learning_transition() {
        let mut store = store(demo_catalog());
        let initial = store.snapshot().unwrap();
        store
            .commit(Commit {
                expected_revision: initial.revision,
                expected_dataset: initial.dataset.clone(),
                writes: vec![
                    Write::Issue(issued_record(&initial)),
                    Write::Active(Some("attempt-1".into())),
                ],
            })
            .unwrap();
        let before = store.snapshot().unwrap();
        let incomplete_writes = successful_answer_writes(&before)
            .into_iter()
            .filter(|write| !matches!(write, Write::Learning { .. }))
            .collect();
        assert!(matches!(
            store.commit(Commit {
                expected_revision: before.revision,
                expected_dataset: before.dataset.clone(),
                writes: incomplete_writes,
            }),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(store.snapshot().unwrap(), before);
        store
            .commit(Commit {
                expected_revision: before.revision,
                expected_dataset: before.dataset.clone(),
                writes: successful_answer_writes(&before),
            })
            .unwrap();
        let after = store.snapshot().unwrap();
        let record = &after.assessments[0];
        assert_eq!(after.learning[&record.target], record.transition.state);
    }
}
