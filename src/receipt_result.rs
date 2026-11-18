//! ReceiptResult and its usage scenarios.

use crate::AnswerResult;
use serde::{Deserialize, Serialize};
use tempo_learning::Transition;
use tempo_types::TargetId;

/// The exact reply saved for either a Study command or an external assessment.
///
/// # When to use
///
/// The application chooses `Study` for human attempt progression and `External`
/// for provider feedback. [`crate::Receipt`] stores either result under a submission
/// ID, allowing retries to replay the original reply.
///
/// The external variant has a target and Learning transition but no human attempt.
/// Game feedback should not fabricate an attempt merely to reuse the Study path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReceiptResult {
    Study(AnswerResult),
    TacticsAllowance {
        reply: String,
    },
    External {
        target: TargetId,
        transition: Transition,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{policy, store};
    use crate::{AssessmentKey, AssessmentRecord, Commit, Receipt, Store, Write};
    use tempo_chess::demo_catalog;
    use tempo_learning::{AssessmentInput, AssessmentSource, LearningPolicy};
    use tempo_types::{Grade, StudyDay, StudyTime, SubmissionId};

    #[test]
    fn external_feedback_has_a_replayable_result_without_creating_a_human_attempt() {
        let mut store = store(demo_catalog());
        let before = store.snapshot().unwrap();
        let target = TargetId::from("r1:p1");
        let input = AssessmentInput {
            grade: Grade::Again,
            accepted_at: StudyTime {
                day: StudyDay(10),
                unix_ms: 100,
            },
            source: AssessmentSource::External {
                provider: "game-provider".into(),
                event_id: "game-42-ply-1".into(),
                observed_at_ms: 50,
            },
        };
        let transition = policy().apply(&before.learning[&target], &input).unwrap();
        let record = AssessmentRecord {
            key: AssessmentKey::External {
                provider: "game-provider".into(),
                event_id: "game-42-ply-1".into(),
            },
            dataset: before.dataset.clone(),
            target: target.clone(),
            content_version: before.catalog.items[&target].version,
            input,
            transition: transition.clone(),
        };
        let result = ReceiptResult::External {
            target: target.clone(),
            transition: transition.clone(),
        };
        store
            .commit(Commit {
                expected_revision: before.revision,
                expected_dataset: before.dataset,
                writes: vec![
                    Write::Learning {
                        target: target.clone(),
                        state: transition.state,
                    },
                    Write::Assessment(record),
                    Write::Receipt(Receipt {
                        id: "external-command-1".into(),
                        command: "canonical external command".into(),
                        result: result.clone(),
                    }),
                ],
            })
            .unwrap();
        let after = store.snapshot().unwrap();
        assert!(after.attempts.is_empty());
        assert!(after.learning[&target].repeat_pending);
        assert_eq!(after.assessments.len(), 1);
        assert_eq!(
            after.receipts[&SubmissionId::from("external-command-1")].result,
            result
        );
    }
}
