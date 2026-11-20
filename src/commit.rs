//! Commit and its usage scenarios.

use crate::Write;
use tempo_types::{DatasetId, Revision};

/// An application-owned atomic batch based on one consistent snapshot.
///
/// # When to use
///
/// After all pure domain decisions succeed, collect the required [`Write`] values
/// and call [`crate::Store::commit`]. Stamp the batch with the snapshot's store
/// revision and dataset identity. Both must still match; otherwise reload and
/// re-evaluate the command rather than blindly changing the expected revision.
///
/// The store revision guards concurrent changes to the whole snapshot. It is not
/// an attempt's response revision, a content version or a learning miss revision.
/// The in-file scenario below shows the entire answer-to-learning-to-receipt batch.
///
/// ```
/// use tempo_chess::Catalog;
/// use tempo_learning::{LearningPolicy, StandardLearning};
/// use tempo_store::{Commit, MemoryStore, Store, Write};
///
/// let policy: StandardLearning = StandardLearning::default();
/// let mut store = MemoryStore::new("dataset-a".into(), Catalog::default(), policy.settings())?;
/// let snapshot = store.snapshot()?;
/// let revision = store.commit(Commit {
///     expected_revision: snapshot.revision,
///     expected_dataset: snapshot.dataset,
///     writes: vec![Write::Active(None)],
/// })?;
/// assert!(revision > snapshot.revision);
/// # Ok::<(), tempo_store::StoreError>(())
/// ```
#[derive(Clone, Debug)]
pub struct Commit {
    pub expected_revision: Revision,
    pub expected_dataset: DatasetId,
    pub writes: Vec<Write>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AnswerResult, AssessmentKey, AssessmentRecord, MemoryStore, Receipt, ReceiptResult, Store,
        StoreError, StoredAttempt,
    };
    use tempo_chess::{ExactSequenceGrader, demo_catalog};
    use tempo_learning::{AssessmentInput, AssessmentSource, LearningPolicy, StandardLearning};
    use tempo_study::{Attempt, AttemptStatus, Grader};
    use tempo_types::{AttemptId, Grade, StudyDay, StudyTime, SubmissionId, TargetId};

    #[test]
    fn one_answer_commits_progress_assessment_learning_and_the_replayable_reply_together() {
        let policy: StandardLearning = StandardLearning::default();
        let mut store =
            MemoryStore::new("dataset-a".into(), demo_catalog(), policy.settings()).unwrap();
        let initial = store.snapshot().unwrap();
        let target = TargetId::from("r1:p1");
        let item = &initial.catalog.items[&target];
        let start = ExactSequenceGrader.start(&item.definition).unwrap();
        let issued = StoredAttempt {
            attempt: Attempt {
                id: "attempt-1".into(),
                dataset: initial.dataset.clone(),
                target: target.clone(),
                content_version: item.version,
                revision: Revision(0),
                state: start.state,
                prompt: Some(start.prompt),
                terminal: None,
                status: AttemptStatus::Active,
            },
            issued_at: StudyTime {
                day: StudyDay(10),
                unix_ms: 100,
            },
            issued_revision: Some(initial.revision),
            issued_miss_revision: initial.learning[&target].miss_revision,
            mode: crate::ActivityMode::Test,
            route: None,
            first_introduction: false,
            exercise_name: None,
            relevance_reason: None,
            lesson: None,
            opponent_lead_in: None,
            acceptance: None,
            revealed_decision: None,
        };
        store
            .commit(Commit {
                expected_revision: initial.revision,
                expected_dataset: initial.dataset,
                writes: vec![
                    Write::Issue(issued.clone()),
                    Write::Active(Some(issued.attempt.id.clone())),
                ],
            })
            .unwrap();

        // Read one consistent view before asking the pure domain components to act.
        let before = store.snapshot().unwrap();
        let stored = &before.attempts[&AttemptId::from("attempt-1")];
        let definition = &before.catalog.items[&target].definition;
        let progress = tempo_study::submit(
            &ExactSequenceGrader,
            definition,
            &stored.attempt,
            stored.attempt.revision,
            &"e2e4".into(),
        )
        .unwrap();
        let input = AssessmentInput {
            grade: progress
                .assessment
                .expect("this answer completes the exercise"),
            accepted_at: StudyTime {
                day: StudyDay(10),
                unix_ms: 110,
            },
            source: AssessmentSource::Human {
                attempt_id: stored.attempt.id.clone(),
                issued_miss_revision: stored.issued_miss_revision,
            },
        };
        let transition = policy.apply(&before.learning[&target], &input).unwrap();
        let record = AssessmentRecord {
            key: AssessmentKey::Human(stored.attempt.id.clone()),
            dataset: before.dataset.clone(),
            target: target.clone(),
            content_version: stored.attempt.content_version,
            input,
            transition: transition.clone(),
        };
        let reply = AnswerResult {
            attempt: progress.attempt.clone(),
            feedback: Some(progress.feedback),
            assessment: progress.assessment,
            learning_effect: Some(transition.effect),
        };

        // These writes form one application operation, not five independent saves.
        store
            .commit(Commit {
                expected_revision: before.revision,
                expected_dataset: before.dataset,
                writes: vec![
                    Write::Progress(StoredAttempt {
                        attempt: progress.attempt,
                        ..stored.clone()
                    }),
                    Write::Learning {
                        target: target.clone(),
                        state: transition.state,
                    },
                    Write::Assessment(record),
                    Write::Receipt(Receipt {
                        id: "submission-1".into(),
                        command: "canonical answer command fixture".into(),
                        result: ReceiptResult::Study(reply.clone()),
                    }),
                    Write::Active(None),
                ],
            })
            .unwrap();

        let after = store.snapshot().unwrap();
        assert_eq!(after.revision, Revision(2));
        assert_eq!(after.active_attempt, None);
        assert_eq!(
            after.attempts[&AttemptId::from("attempt-1")]
                .attempt
                .terminal,
            Some(Grade::Good)
        );
        assert_eq!(after.learning[&target].due, Some(StudyDay(13)));
        assert_eq!(after.assessments.len(), 1);
        assert_eq!(
            after.receipts[&SubmissionId::from("submission-1")].result,
            ReceiptResult::Study(reply)
        );
    }

    #[test]
    fn terminal_identity_and_expected_revision_prevent_duplicate_effects() {
        use crate::test_support::{issued_record, store, successful_answer_writes};

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
        let after = store.snapshot().unwrap();
        assert!(matches!(
            store.commit(Commit {
                expected_revision: issued.revision,
                expected_dataset: issued.dataset,
                writes: vec![Write::Active(None)],
            }),
            Err(StoreError::Conflict(_))
        ));
        let mut duplicate = after.assessments[0].clone();
        duplicate.transition.state.scheduled_reviews += 1;
        assert!(matches!(
            store.commit(Commit {
                expected_revision: after.revision,
                expected_dataset: after.dataset.clone(),
                writes: vec![Write::Assessment(duplicate)],
            }),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(store.snapshot().unwrap(), after);
    }
}
