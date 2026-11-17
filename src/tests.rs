use super::*;
use tempo_chess::{ExactSequenceGrader, demo_catalog};
use tempo_learning::{LearningPolicy, StandardLearning};
use tempo_study::Grader;
use tempo_types::StudyDay;

fn policy() -> StandardLearning {
    StandardLearning::default()
}
fn store(catalog: Catalog) -> MemoryStore {
    MemoryStore::new("dataset-a".into(), catalog, policy().settings()).unwrap()
}
fn commit(store: &mut MemoryStore, writes: Vec<Write>) -> Result<Revision, StoreError> {
    let snapshot = store.snapshot()?;
    store.commit(Commit {
        expected_revision: snapshot.revision,
        expected_dataset: snapshot.dataset,
        writes,
    })
}
fn issued_record(snapshot: &Snapshot) -> StoredAttempt {
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
        issued_miss_revision: snapshot.learning[&target].miss_revision,
    }
}
fn issue(store: &mut MemoryStore) {
    let row = issued_record(&store.snapshot().unwrap());
    commit(
        store,
        vec![Write::Issue(row), Write::Active(Some("attempt-1".into()))],
    )
    .unwrap();
}
fn good_writes(snapshot: &Snapshot) -> Vec<Write> {
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

#[test]
fn failed_commit_publishes_nothing_and_same_batch_can_retry() {
    let mut store = store(demo_catalog());
    issue(&mut store);
    let before = store.snapshot().unwrap();
    let writes = good_writes(&before);
    store.fail_next_commit();
    assert!(matches!(
        commit(&mut store, writes.clone()),
        Err(StoreError::Unavailable(_))
    ));
    assert_eq!(store.snapshot().unwrap(), before);
    commit(&mut store, writes).unwrap();
    let after = store.snapshot().unwrap();
    assert_eq!(after.assessments.len(), 1);
    assert_eq!(after.receipts.len(), 1);
    assert_eq!(
        after.learning[&TargetId::from("r1:p1")].due,
        Some(StudyDay(13))
    );
    assert_eq!(
        after.attempts[&AttemptId::from("attempt-1")]
            .attempt
            .terminal,
        Some(Grade::Good)
    );
}

#[test]
fn terminal_assessment_cannot_commit_without_its_learning_transition() {
    let mut store = store(demo_catalog());
    issue(&mut store);
    let before = store.snapshot().unwrap();
    let writes = good_writes(&before)
        .into_iter()
        .filter(|write| !matches!(write, Write::Learning { .. }))
        .collect();
    assert!(matches!(
        commit(&mut store, writes),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(store.snapshot().unwrap(), before);
}

#[test]
fn receipt_cannot_publish_a_different_or_incomplete_outcome() {
    let mut store = store(demo_catalog());
    issue(&mut store);
    let before = store.snapshot().unwrap();
    for omit_assessment in [false, true] {
        let mut writes = good_writes(&before);
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
            commit(&mut store, writes),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(store.snapshot().unwrap(), before);
    }
}

#[test]
fn terminal_identity_and_expected_revision_prevent_duplicate_effects() {
    let mut store = store(demo_catalog());
    issue(&mut store);
    let issued = store.snapshot().unwrap();
    commit(&mut store, good_writes(&issued)).unwrap();
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
        commit(&mut store, vec![Write::Assessment(duplicate)]),
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(store.snapshot().unwrap(), after);
}

#[test]
fn empty_dataset_identities_remain_retired_after_restore() {
    let mut store = store(Catalog::default());
    commit(
        &mut store,
        vec![Write::Restore {
            dataset: "dataset-b".into(),
            catalog: Catalog::default(),
            learning: BTreeMap::new(),
            policy: policy().settings(),
        }],
    )
    .unwrap();
    let before = store.snapshot().unwrap();
    assert!(before.content_history.is_empty());
    assert_eq!(before.known_datasets.len(), 2);
    assert!(matches!(
        commit(
            &mut store,
            vec![Write::Restore {
                dataset: "dataset-a".into(),
                catalog: Catalog::default(),
                learning: BTreeMap::new(),
                policy: policy().settings(),
            }]
        ),
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(store.snapshot().unwrap(), before);
}

#[test]
fn issuance_requires_current_miss_revision_and_consistent_presentation() {
    let mut store = store(demo_catalog());
    let before = store.snapshot().unwrap();
    let mut stale = issued_record(&before);
    stale.issued_miss_revision = MissRevision(1);
    assert!(matches!(
        commit(&mut store, vec![Write::Issue(stale)]),
        Err(StoreError::Invalid(_))
    ));
    let mut no_prompt = issued_record(&before);
    no_prompt.attempt.prompt = None;
    assert!(matches!(
        commit(&mut store, vec![Write::Issue(no_prompt)]),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(store.snapshot().unwrap(), before);
}

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
        commit(
            &mut store,
            vec![
                Write::Learning {
                    target,
                    state: transition.state
                },
                Write::Assessment(record),
            ]
        ),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(store.snapshot().unwrap(), before);
}

#[test]
fn exact_historical_result_may_have_an_alias_receipt_after_restore() {
    let mut store = store(demo_catalog());
    issue(&mut store);
    let issued = store.snapshot().unwrap();
    commit(&mut store, good_writes(&issued)).unwrap();
    let mut alias = store.snapshot().unwrap().receipts[&SubmissionId::from("submission-1")].clone();
    alias.id = "alias-receipt".into();
    commit(
        &mut store,
        vec![Write::Restore {
            dataset: "dataset-b".into(),
            catalog: Catalog::default(),
            learning: BTreeMap::new(),
            policy: policy().settings(),
        }],
    )
    .unwrap();
    commit(&mut store, vec![Write::Receipt(alias.clone())]).unwrap();
    let after = store.snapshot().unwrap();
    assert_eq!(after.receipts[&alias.id], alias);
    assert!(after.learning.is_empty());
    assert_eq!(after.assessments.len(), 1);
}
