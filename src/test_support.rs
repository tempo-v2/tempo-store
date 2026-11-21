//! Construction helpers for in-file scenarios. These functions do not commit.
//! The complete public assembly example lives in `commit.rs`.
use crate::*;
use tempo_chess::Catalog;
use tempo_chess::ExactSequenceGrader;
use tempo_learning::{AssessmentInput, AssessmentSource};
use tempo_learning::{LearningPolicy, StandardLearning};
use tempo_study::Grader;
use tempo_study::{Attempt, AttemptStatus};
use tempo_types::StudyDay;
use tempo_types::{AttemptId, Grade, Revision, StudyTime, TargetId};

pub(crate) fn policy() -> StandardLearning {
    StandardLearning::default()
}
pub(crate) fn store(catalog: Catalog) -> MemoryStore {
    MemoryStore::new("dataset-a".into(), catalog, policy().settings()).unwrap()
}
pub(crate) fn issued_record(snapshot: &Snapshot) -> StoredAttempt {
    let target: TargetId = "r1:p1".into();
    let item = &snapshot.catalog.items[&target];
    let start = ExactSequenceGrader.start(&item.definition).unwrap();
    StoredAttempt {
        attempt: Attempt {
            id: "attempt-1".into(),
            dataset: snapshot.dataset.clone(),
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
        issued_revision: Some(snapshot.revision),
        issued_miss_revision: snapshot.learning[&target].miss_revision,
        mode: crate::ActivityMode::Test,
        route: None,
        first_introduction: false,
        exercise_name: None,
        tactic_attribution: None,
        relevance_reason: None,
        lesson: None,
        opponent_lead_in: None,
        repertoire_warmup: false,
        acceptance: None,
        revealed_decision: None,
    }
}
pub(crate) fn successful_answer_writes(snapshot: &Snapshot) -> Vec<Write> {
    let stored = &snapshot.attempts[&AttemptId::from("attempt-1")];
    let item = &snapshot.catalog.items[&stored.attempt.target];
    let progress = tempo_study::submit(
        &ExactSequenceGrader,
        &item.definition,
        &stored.attempt,
        Revision(0),
        &"e2e4".into(),
    )
    .unwrap();
    let input = AssessmentInput {
        grade: Grade::Good,
        accepted_at: stored.issued_at,
        source: AssessmentSource::Human {
            attempt_id: stored.attempt.id.clone(),
            issued_miss_revision: stored.issued_miss_revision,
        },
    };
    let transition = policy()
        .apply(&snapshot.learning[&item.target], &input)
        .unwrap();
    let assessment = AssessmentRecord {
        key: AssessmentKey::Human(stored.attempt.id.clone()),
        dataset: snapshot.dataset.clone(),
        target: item.target.clone(),
        content_version: item.version,
        input,
        transition: transition.clone(),
    };
    let receipt = Receipt {
        id: "submission-1".into(),
        command: "canonical command fixture".into(),
        result: ReceiptResult::Study(AnswerResult {
            attempt: progress.attempt.clone(),
            feedback: Some(progress.feedback),
            assessment: progress.assessment,
            learning_effect: Some(transition.effect),
        }),
    };
    vec![
        Write::Progress(StoredAttempt {
            attempt: progress.attempt,
            ..stored.clone()
        }),
        Write::Learning {
            target: item.target.clone(),
            state: transition.state,
        },
        Write::Assessment(assessment),
        Write::Receipt(receipt),
        Write::Active(None),
    ]
}
