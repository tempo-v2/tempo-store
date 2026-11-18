//! StoredAttempt and its usage scenarios.

use crate::ChessAttempt;
use serde::{Deserialize, Serialize};
use tempo_types::{MissRevision, StudyTime};

/// Fixed when an activity is durably issued.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityMode {
    Teach,
    #[default]
    Test,
    Practice,
}

/// Accepted activity evidence, including the reason for the first Again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityAcceptance {
    pub at: StudyTime,
    pub kind: AcceptanceKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AcceptanceKind {
    Teaching,
    Recall,
    WrongMove { uci: String, decision_index: usize },
    Revealed { decision_index: usize },
}

/// A Study attempt plus the learning context fixed when it was issued.
///
/// # When to use
///
/// The application wraps a newly started [`ChessAttempt`] with an explicit issue
/// time and the current target's miss revision, then requests [`crate::Write::Issue`].
/// For later answers it replaces only `attempt` and requests
/// [`crate::Write::Progress`]. Store preserves the original issue facts.
///
/// `issued_miss_revision` lets Learning recognize a success from an attempt that
/// started before a newer miss. It is different from the attempt's response
/// revision, which detects stale answer commands. Neither value is a content
/// version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredAttempt {
    pub attempt: ChessAttempt,
    pub issued_at: StudyTime,
    pub issued_miss_revision: MissRevision,
    #[serde(default)]
    pub mode: ActivityMode,
    #[serde(default)]
    pub route: Option<String>,
    #[serde(default)]
    pub first_introduction: bool,
    #[serde(default)]
    pub repertoire_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lesson: Option<tempo_chess::RepertoireLesson>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<ActivityAcceptance>,
}

#[cfg(test)]
mod tests {
    use crate::test_support::{issued_record, store};
    use crate::{Commit, Store, StoreError, Write};
    use tempo_chess::demo_catalog;
    use tempo_types::{AttemptId, MissRevision};

    #[test]
    fn issuance_requires_current_miss_revision_and_consistent_presentation() {
        let mut store = store(demo_catalog());
        let before = store.snapshot().unwrap();
        let mut stale = issued_record(&before);
        stale.issued_miss_revision = MissRevision(1);
        assert!(matches!(
            store.commit(Commit {
                expected_revision: before.revision,
                expected_dataset: before.dataset.clone(),
                writes: vec![Write::Issue(stale)],
            }),
            Err(StoreError::Invalid(_))
        ));
        let mut no_prompt = issued_record(&before);
        no_prompt.attempt.prompt = None;
        assert!(matches!(
            store.commit(Commit {
                expected_revision: before.revision,
                expected_dataset: before.dataset.clone(),
                writes: vec![Write::Issue(no_prompt)],
            }),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(store.snapshot().unwrap(), before);
        let valid = issued_record(&before);
        store
            .commit(Commit {
                expected_revision: before.revision,
                expected_dataset: before.dataset,
                writes: vec![
                    Write::Issue(valid.clone()),
                    Write::Active(Some(valid.attempt.id.clone())),
                ],
            })
            .unwrap();
        let after = store.snapshot().unwrap();
        assert_eq!(after.attempts[&AttemptId::from("attempt-1")], valid);
        assert_eq!(after.active_attempt, Some(valid.attempt.id));
    }
}
