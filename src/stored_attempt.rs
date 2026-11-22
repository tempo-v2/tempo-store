//! StoredAttempt and its usage scenarios.

use crate::{ChessAttempt, TacticGameOrigin};
use serde::{Deserialize, Serialize};
use tempo_types::{MissRevision, Revision, StudyTime, WorkKind, WorkToken};

/// Fixed when an activity is durably issued.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityMode {
    Teach,
    Repair,
    #[default]
    Test,
    Practice,
}

impl ActivityMode {
    pub fn guided(self) -> bool {
        matches!(self, Self::Teach | Self::Repair)
    }
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

/// The latest learner decision whose prescribed move an accepted response disclosed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevealedDecision {
    pub decision_index: usize,
}

/// Presentation attribution recorded at issue, independent of exercise identity
/// and the contents of the response shown to a blind learner.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IssuedTacticAttribution {
    pub title: String,
    pub provenance: Vec<String>,
    pub game: Option<TacticGameOrigin>,
    /// Capture evidence fixed when the tactic was issued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_capture: Option<tempo_chess::GameCapture>,
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
    /// Store revision immediately before issuance. Older rows precede tracked issuance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_revision: Option<Revision>,
    pub issued_miss_revision: MissRevision,
    /// Exact obligation selected by the prepared plan at issue. Legacy attempts
    /// have no token, and a subsequent generation never supplies one for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_work: Option<WorkToken>,
    #[serde(default)]
    pub mode: ActivityMode,
    #[serde(default)]
    pub route: Option<String>,
    #[serde(default)]
    pub first_introduction: bool,
    /// Optional public display text fixed when the attempt is issued.
    // Keep the historical SQLite JSON key readable by earlier binaries.
    #[serde(default, rename = "repertoire_name")]
    pub exercise_name: Option<String>,
    /// Absent on attempts issued before origin attribution was retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tactic_attribution: Option<IssuedTacticAttribution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relevance_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lesson: Option<tempo_chess::RepertoireLesson>,
    /// The presentation context available when Study issued this attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opponent_lead_in: Option<tempo_chess::OpponentLeadIn>,
    /// Fix the warm-up presentation at issuance for resumed and replayed views.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repertoire_warmup: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<ActivityAcceptance>,
    /// Set by Tempo from the accepted Study transition, independent of the first grade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revealed_decision: Option<RevealedDecision>,
}

impl StoredAttempt {
    pub(crate) fn has_consistent_work_identity(&self) -> bool {
        let Some(work) = &self.issued_work else {
            return true;
        };
        let kind = match self.mode {
            ActivityMode::Teach => WorkKind::Teach,
            ActivityMode::Test => WorkKind::Test,
            ActivityMode::Repair => WorkKind::Correction,
            ActivityMode::Practice => return false,
        };
        work.day == self.issued_at.day
            && work.obligation.target == self.attempt.target
            && work.obligation.version == self.attempt.content_version
            && work.obligation.revision == self.issued_miss_revision
            && work.obligation.kind == kind
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::{issued_record, store};
    use crate::{Commit, Store, StoreError, Write};
    use tempo_chess::demo_catalog;
    use tempo_types::{AttemptId, MissRevision};

    #[test]
    fn the_display_name_round_trips_with_the_legacy_storage_key() {
        let store = store(demo_catalog());
        let mut record = issued_record(&store.snapshot().unwrap());
        record.exercise_name = Some("A named exercise".into());
        let mut raw = serde_json::to_value(&record).unwrap();
        assert_eq!(raw["repertoire_name"], "A named exercise");
        assert!(raw.get("exercise_name").is_none());
        assert_eq!(
            serde_json::from_value::<super::StoredAttempt>(raw.clone()).unwrap(),
            record
        );

        raw.as_object_mut().unwrap().remove("repertoire_name");
        assert_eq!(
            serde_json::from_value::<super::StoredAttempt>(raw)
                .unwrap()
                .exercise_name,
            None
        );
    }

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

    #[test]
    fn issued_work_is_durable_without_a_generation_and_cannot_change_on_progress() {
        use crate::{
            SqliteStore,
            test_support::{policy, successful_answer_writes},
        };
        use tempo_learning::LearningPolicy;
        use tempo_types::{StudyDay, WorkKind, WorkObligation, WorkToken};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("issued.sqlite");
        let mut store = SqliteStore::create(
            &path,
            "issued-work".into(),
            demo_catalog(),
            policy().settings(),
        )
        .unwrap();
        let before = store.snapshot().unwrap();
        let mut issued = issued_record(&before);
        let legacy = serde_json::to_value(&issued).unwrap();
        assert!(legacy.get("issued_work").is_none());
        assert_eq!(
            serde_json::from_value::<super::StoredAttempt>(legacy).unwrap(),
            issued
        );
        let work = WorkToken {
            day: issued.issued_at.day,
            obligation: WorkObligation {
                target: issued.attempt.target.clone(),
                version: issued.attempt.content_version,
                kind: WorkKind::Test,
                revision: issued.issued_miss_revision,
            },
        };
        let mut mismatched = issued.clone();
        mismatched.issued_work = Some(WorkToken {
            day: StudyDay(work.day.0 + 1),
            ..work.clone()
        });
        let commit = |record: super::StoredAttempt| {
            let active = record.attempt.id.clone();
            Commit {
                expected_revision: before.revision,
                expected_dataset: before.dataset.clone(),
                writes: vec![Write::Issue(record), Write::Active(Some(active))],
            }
        };
        assert!(matches!(
            store.commit(commit(mismatched)),
            Err(StoreError::Invalid(_))
        ));
        issued.issued_work = Some(work.clone());
        store.commit(commit(issued.clone())).unwrap();
        drop(store);

        let mut store = SqliteStore::open(&path).unwrap();
        let before = store.snapshot().unwrap();
        let id = &issued.attempt.id;
        assert_eq!(before.attempts[id].issued_work, Some(work.clone()));
        assert_eq!(store.study_plan_head(&before.dataset).unwrap(), None);
        let mut changed = successful_answer_writes(&before);
        for write in &mut changed {
            if let Write::Progress(record) = write {
                record.issued_work = None;
            }
        }
        let commit = |writes| Commit {
            expected_revision: before.revision,
            expected_dataset: before.dataset.clone(),
            writes,
        };
        assert!(matches!(
            store.commit(commit(changed)),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(store.snapshot().unwrap(), before);
        store.commit(commit(successful_answer_writes(&before))).unwrap();
        assert_eq!(store.snapshot().unwrap().attempts[id].issued_work, Some(work));
    }
}
