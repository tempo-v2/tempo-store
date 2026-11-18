//! AssessmentKey and its usage scenarios.

use serde::{Deserialize, Serialize};
use tempo_types::{AttemptId, EventId};

/// The identity that makes one learning assessment unique within a dataset.
///
/// # When to use
///
/// The application gives each [`crate::AssessmentRecord`] a key matching its
/// [`tempo_learning::AssessmentSource`]. A human attempt has at most one terminal
/// assessment; an external observation is identified by provider and stable event
/// ID. Store rejects a second assessment with the same key in the same dataset.
///
/// A submission ID answers a different question: has this exact application
/// command already received a reply? That identity belongs to [`crate::Receipt`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssessmentKey {
    Human(AttemptId),
    External { provider: String, event_id: EventId },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{policy, store};
    use crate::{AssessmentRecord, Commit, Store, StoreError, Write};
    use tempo_chess::demo_catalog;
    use tempo_learning::{AssessmentInput, AssessmentSource, LearningPolicy};
    use tempo_types::{ContentVersion, Grade, StudyDay, StudyTime, TargetId};

    #[test]
    fn external_assessment_key_and_source_must_agree() {
        let mut store = store(demo_catalog());
        let before = store.snapshot().unwrap();
        let target: TargetId = "r1:p1".into();
        let input = AssessmentInput {
            grade: Grade::Again,
            accepted_at: StudyTime {
                day: StudyDay(10),
                unix_ms: 100,
            },
            source: AssessmentSource::External {
                provider: "provider".into(),
                event_id: "event-a".into(),
                observed_at_ms: 1,
            },
        };
        let transition = policy().apply(&before.learning[&target], &input).unwrap();
        let record = AssessmentRecord {
            key: AssessmentKey::External {
                provider: "provider".into(),
                event_id: "event-b".into(),
            },
            dataset: before.dataset.clone(),
            target: target.clone(),
            content_version: ContentVersion(1),
            input,
            transition: transition.clone(),
        };
        assert!(matches!(
            store.commit(Commit {
                expected_revision: before.revision,
                expected_dataset: before.dataset.clone(),
                writes: vec![
                    Write::Learning {
                        target: target.clone(),
                        state: transition.state.clone()
                    },
                    Write::Assessment(record.clone()),
                ],
            }),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(store.snapshot().unwrap(), before);
        // The stable provider event ID must be the same in provenance and uniqueness key.
        let corrected = AssessmentRecord {
            key: AssessmentKey::External {
                provider: "provider".into(),
                event_id: "event-a".into(),
            },
            ..record
        };
        store
            .commit(Commit {
                expected_revision: before.revision,
                expected_dataset: before.dataset,
                writes: vec![
                    Write::Learning {
                        target,
                        state: transition.state,
                    },
                    Write::Assessment(corrected),
                ],
            })
            .unwrap();
        assert_eq!(store.snapshot().unwrap().assessments.len(), 1);
    }
}
