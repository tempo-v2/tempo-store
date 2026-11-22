//! Receipt and its usage scenarios.

use crate::ReceiptResult;
use serde::{Deserialize, Serialize};
use tempo_types::{DatasetId, EventId, SubmissionId};

/// A submission identity, command equality token, and its exact committed reply.
///
/// # When to use
///
/// The application checks receipts before re-evaluating a retry against current
/// content. An identical command can return its saved result; reuse of the same
/// submission ID for a different command is an application conflict. Store rejects
/// duplicate receipt IDs during commit.
///
/// A receipt is about reliable command replay, while [`crate::AssessmentRecord`]
/// is about learning history. Many intermediate/correction replies have a receipt
/// but no new assessment. Neither the command token nor the reply is executable.
///
/// Historical replies retain earlier attempt revisions and dataset identities.
/// They are not rewritten when content changes or a new dataset is restored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub id: SubmissionId,
    /// Canonical serialization of the typed command, excluding acceptance time.
    /// This is an equality token, never an executable/domain message.
    pub command: String,
    pub result: ReceiptResult,
}

/// Read an accepted command before loading any new-work facts.
pub trait StudyReceiptRead {
    fn study_receipt(
        &self,
        dataset: &DatasetId,
        id: &SubmissionId,
    ) -> Result<StudyReceiptLookup, crate::StoreError>;
}

pub enum StudyReceiptLookup {
    WrongDataset,
    Missing,
    Found(Box<Receipt>),
}

impl Receipt {
    /// Identifies the exact delivery receipt owned by an external assessment key.
    pub fn external_id(dataset: &DatasetId, provider: &str, event: &EventId) -> SubmissionId {
        let identity = serde_json::json!([dataset, provider, event]);
        SubmissionId(format!("external:{identity}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{issued_record, policy, store, successful_answer_writes};
    use crate::{AnswerResult, Commit, Store, StoreError, Write};
    use std::collections::BTreeMap;
    use tempo_chess::{Catalog, demo_catalog};
    use tempo_learning::LearningPolicy;
    use tempo_types::Revision;

    #[test]
    fn receipt_cannot_publish_a_different_or_incomplete_outcome() {
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
        for omit_assessment in [false, true] {
            let mut writes = successful_answer_writes(&before);
            for write in &mut writes {
                if let Write::Receipt(Receipt {
                    result: ReceiptResult::Study(result),
                    ..
                }) = write
                {
                    if omit_assessment {
                        result.assessment = None;
                        result.learning_effect = None;
                    } else {
                        result.attempt.revision = Revision(99);
                    }
                }
            }
            assert!(matches!(
                store.commit(Commit {
                    expected_revision: before.revision,
                    expected_dataset: before.dataset.clone(),
                    writes,
                }),
                Err(StoreError::Invalid(_))
            ));
            assert_eq!(store.snapshot().unwrap(), before);
        }
    }

    #[test]
    fn exact_historical_result_may_have_an_alias_receipt_after_restore() {
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
        let issued = store.snapshot().unwrap();
        store
            .commit(Commit {
                expected_revision: issued.revision,
                expected_dataset: issued.dataset.clone(),
                writes: successful_answer_writes(&issued),
            })
            .unwrap();
        let answered = store.snapshot().unwrap();
        let mut alias = answered.receipts[&SubmissionId::from("submission-1")].clone();
        alias.id = "alias-receipt".into();
        store
            .commit(Commit {
                expected_revision: answered.revision,
                expected_dataset: answered.dataset,
                writes: vec![Write::Restore {
                    dataset: "dataset-b".into(),
                    catalog: Catalog::default(),
                    learning: BTreeMap::new(),
                    policy: policy().settings(),
                }],
            })
            .unwrap();
        let restored = store.snapshot().unwrap();
        store
            .commit(Commit {
                expected_revision: restored.revision,
                expected_dataset: restored.dataset,
                writes: vec![Write::Receipt(alias.clone())],
            })
            .unwrap();
        let after = store.snapshot().unwrap();
        assert_eq!(after.receipts[&alias.id], alias);
        assert!(
            matches!(&alias.result, ReceiptResult::Study(AnswerResult { attempt, .. }) if attempt.dataset.0 == "dataset-a")
        );
        assert!(after.learning.is_empty());
        assert_eq!(after.assessments.len(), 1);
    }
}
