//! SQLite-specific acceptance with independent connections to real files.

use super::*;
use crate::Write;
use crate::test_support::{issued_record, policy, successful_answer_writes};
use rusqlite::ffi::{
    SQLITE_CONSTRAINT_FOREIGNKEY, SQLITE_CONSTRAINT_PRIMARYKEY, SQLITE_CONSTRAINT_UNIQUE,
};
use std::{collections::BTreeMap, sync::Barrier, thread};
use tempo_chess::demo_catalog;
use tempo_learning::LearningPolicy;
use tempo_types::{ContentVersion, SubmissionId, TargetId};

fn create(path: &Path, catalog: Catalog) -> SqliteStore {
    SqliteStore::create(path, "dataset-a".into(), catalog, policy().settings()).unwrap()
}

fn commit(
    store: &mut SqliteStore,
    before: &Snapshot,
    writes: Vec<Write>,
) -> Result<Revision, StoreError> {
    store.commit(Commit {
        expected_revision: before.revision,
        expected_dataset: before.dataset.clone(),
        writes,
    })
}

fn issue(store: &mut SqliteStore) -> Snapshot {
    let before = store.snapshot().unwrap();
    commit(
        store,
        &before,
        vec![
            Write::Issue(issued_record(&before)),
            Write::Active(Some("attempt-1".into())),
        ],
    )
    .unwrap();
    store.snapshot().unwrap()
}

#[test]
fn reopening_preserves_full_snapshot_including_ordered_repeated_occurrences() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("study.sqlite");
    let mut catalog = demo_catalog();
    catalog.lines[0].targets.push("r1:p1".into());
    catalog.lines.push(tempo_chess::RepertoireLine {
        id: "another-line".into(),
        repertoire: "r1".into(),
        targets: vec!["r1:p2".into()],
    });
    catalog
        .items
        .get_mut(&TargetId::from("t1"))
        .unwrap()
        .version = ContentVersion(u64::MAX);
    let mut store = create(&path, catalog.clone());
    let issued = issue(&mut store);
    assert_eq!(issued.catalog, catalog);
    drop(store);
    let mut reopened = SqliteStore::open(&path).unwrap();
    assert_eq!(reopened.snapshot().unwrap(), issued);
    commit(&mut reopened, &issued, successful_answer_writes(&issued)).unwrap();
    let answered = reopened.snapshot().unwrap();
    drop(reopened);
    let reopened = SqliteStore::open(&path).unwrap();
    assert_eq!(reopened.snapshot().unwrap(), answered);
    assert_eq!(answered.assessments.len(), 1);
    assert_eq!(answered.receipts.len(), 1);
    assert_eq!(answered.active_attempt, None);
}

#[test]
fn schema_sixteen_opens_with_the_same_saved_results_and_no_repertoire_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("study.sqlite");
    let mut store = create(&path, demo_catalog());
    let issued = issue(&mut store);
    commit(&mut store, &issued, successful_answer_writes(&issued)).unwrap();
    let before = store.snapshot().unwrap();
    store
        .connection
        .execute_batch(
            "DROP INDEX last_applied_human_test;
         DROP TABLE repertoire_run_steps;
         DROP VIEW tactical_report_requests;
         DROP VIEW withheld_game_tactics;
         DROP TABLE tactical_qualifications;
         DROP TABLE study_exclusions;
         DROP INDEX tactical_requests_card_origin;
         DROP TABLE repertoire_partition_lineage; DROP TABLE repertoire_coverage_start; PRAGMA user_version=16;",
        )
        .unwrap();
    drop(store);

    let upgraded = SqliteStore::open(&path).unwrap();
    assert_eq!(
        schema_version(&upgraded.connection).unwrap(),
        SCHEMA_VERSION
    );
    assert_eq!(upgraded.snapshot().unwrap(), before);
}

#[test]
fn schema_twenty_one_upgrades_with_its_active_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("study.sqlite");
    let mut store = create(&path, demo_catalog());
    let before = issue(&mut store);
    store
        .connection
        .pragma_update(None, "user_version", 21)
        .unwrap();
    drop(store);

    let upgraded = SqliteStore::open(&path).unwrap();
    assert_eq!(
        schema_version(&upgraded.connection).unwrap(),
        SCHEMA_VERSION
    );
    assert_eq!(upgraded.snapshot().unwrap(), before);
}

#[test]
fn simultaneous_writers_from_one_revision_have_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("study.sqlite");
    let left = create(&path, demo_catalog());
    let right = SqliteStore::open(&path).unwrap();
    let before = left.snapshot().unwrap();
    assert_eq!(right.snapshot().unwrap(), before);
    let barrier = Barrier::new(2);
    let write = |mut store: SqliteStore| {
        barrier.wait();
        commit(&mut store, &before, vec![Write::Active(None)])
    };
    let (first, second) = thread::scope(|scope| {
        let left = scope.spawn(|| write(left));
        let right = scope.spawn(|| write(right));
        (left.join().unwrap(), right.join().unwrap())
    });
    assert!(matches!(
        (first, second),
        (Ok(Revision(1)), Err(StoreError::Conflict(_)))
            | (Err(StoreError::Conflict(_)), Ok(Revision(1)))
    ));
    assert_eq!(
        SqliteStore::open(&path)
            .unwrap()
            .snapshot()
            .unwrap()
            .revision,
        Revision(1)
    );
}

#[test]
fn repertoire_reset_rolls_back_scoped_deletions_when_its_receipt_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reset.sqlite");
    let mut store = create(&path, demo_catalog());
    let issued = issue(&mut store);
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER reject_reset_receipt BEFORE INSERT ON repertoire_resets
         BEGIN SELECT RAISE(ABORT, 'injected reset failure'); END;",
        )
        .unwrap();
    let fresh = LearningState::default();
    assert!(
        store
            .reset_repertoire(&issued.dataset, "r1", "reset-one", 100, &fresh)
            .is_err()
    );
    assert_eq!(store.snapshot().unwrap(), issued);
    assert_eq!(
        store.repertoire_reset_at(&issued.dataset, "r1").unwrap(),
        None
    );

    store
        .connection
        .execute_batch("DROP TRIGGER reject_reset_receipt")
        .unwrap();
    assert!(matches!(
        store
            .reset_repertoire(&issued.dataset, "r1", "reset-one", 100, &fresh)
            .unwrap(),
        RepertoireReset::Applied(_)
    ));
    let reset = store.snapshot().unwrap();
    assert!(reset.active_attempt.is_none());
    assert!(reset.attempts.is_empty());
    assert_eq!(reset.learning, issued.learning);
    assert!(matches!(
        commit(&mut store, &issued, successful_answer_writes(&issued)),
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(store.snapshot().unwrap(), reset);
}

#[test]
fn repertoire_activity_is_atomic_and_an_old_receipt_cannot_resuspend_it() {
    use crate::{
        RepertoireActivity, RepertoireActivityCommand, RepertoireActivityRead,
        RepertoireActivityResult,
    };

    let dir = tempfile::tempdir().unwrap();
    let mut store = create(&dir.path().join("suspension.sqlite"), demo_catalog());
    let issued = issue(&mut store);
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER reject_activity_receipt BEFORE INSERT ON content_receipts
         BEGIN SELECT RAISE(ABORT, 'injected activity receipt failure'); END;",
        )
        .unwrap();
    let change = |store: &mut SqliteStore, id, activity| {
        let command = RepertoireActivityCommand {
            dataset: &issued.dataset,
            repertoire: "r1",
            command_id: id,
            activity,
        };
        let read = store.repertoire_activity(&command)?;
        let RepertoireActivityRead::Pending { revision, active } = read else {
            return store.set_repertoire_activity(&command, Revision(0), None);
        };
        let abandonment = active
            .filter(|_| activity == RepertoireActivity::Suspended)
            .map(|attempt| tempo_study::abandon(&attempt, attempt.revision).unwrap());
        store.set_repertoire_activity(&command, revision, abandonment)
    };
    assert!(change(&mut store, "pause", RepertoireActivity::Suspended).is_err());
    assert_eq!(store.snapshot().unwrap(), issued);
    assert_eq!(store.content_state().unwrap().content_revision, Revision(0));
    assert!(!store.repertoire_document("r1").unwrap().suspended);

    store
        .connection
        .execute_batch("DROP TRIGGER reject_activity_receipt")
        .unwrap();
    assert!(matches!(
        change(&mut store, "pause", RepertoireActivity::Suspended).unwrap(),
        RepertoireActivityResult::Applied
    ));
    assert!(matches!(
        change(&mut store, "pause", RepertoireActivity::Active).unwrap(),
        RepertoireActivityResult::CommandConflict
    ));
    assert!(matches!(
        change(&mut store, "resume", RepertoireActivity::Active).unwrap(),
        RepertoireActivityResult::Applied
    ));
    assert!(matches!(
        change(&mut store, "pause", RepertoireActivity::Suspended).unwrap(),
        RepertoireActivityResult::Applied
    ));
    assert!(!store.repertoire_document("r1").unwrap().suspended);
    assert!(matches!(
        commit(&mut store, &issued, successful_answer_writes(&issued)),
        Err(StoreError::Conflict(_))
    ));
}

#[test]
fn receipt_trigger_aborts_late_writes_and_the_same_batch_can_retry_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("study.sqlite");
    let mut store = create(&path, demo_catalog());
    let issued = issue(&mut store);
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER reject_completed_receipt BEFORE INSERT ON receipts
         WHEN (SELECT count(*) FROM assessments) = 1
         BEGIN SELECT RAISE(ABORT, 'injected receipt failure'); END;",
        )
        .unwrap();
    let id = SubmissionId::from("submission-1");
    let scope = StudyScope {
        submission: Some(&id),
        ..Default::default()
    };
    let batch = Commit {
        expected_revision: issued.revision,
        expected_dataset: issued.dataset.clone(),
        writes: successful_answer_writes(&issued),
    };
    let failure = store.commit_study(batch.clone(), scope).unwrap_err();
    assert!(
        matches!(failure, StoreError::Unavailable(message) if message.contains("injected receipt failure"))
    );
    drop(store);

    let mut store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.snapshot().unwrap(), issued);
    store
        .connection
        .execute("DROP TRIGGER reject_completed_receipt", [])
        .unwrap();
    assert_eq!(
        store.commit_study(batch, scope),
        Ok(Revision(issued.revision.0 + 1))
    );
    drop(store);
    let answered = SqliteStore::open(&path).unwrap().snapshot().unwrap();
    assert_eq!(answered.assessments.len(), 1);
    assert_eq!(answered.receipts.len(), 1);
    assert_eq!(answered.active_attempt, None);
}

fn assert_constraint(error: rusqlite::Error, code: i32) {
    assert!(
        matches!(error, rusqlite::Error::SqliteFailure(error, _) if error.extended_code == code),
        "{error}"
    );
}

#[test]
fn sql_constraints_enforce_content_identity_references_and_unique_outcomes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("study.sqlite");
    let mut store = create(&path, demo_catalog());
    let issued = issue(&mut store);
    commit(&mut store, &issued, successful_answer_writes(&issued)).unwrap();
    let before = store.snapshot().unwrap();
    let transaction = store.connection.transaction().unwrap();
    transaction
        .execute(
            "INSERT INTO content_history SELECT 99, dataset_id, 'competing-id', version, item
         FROM content_history WHERE target_id = 'r1:p1'",
            [],
        )
        .unwrap();
    assert_constraint(transaction.execute(
        "INSERT INTO current_content SELECT 'competing-id', dataset_id, version, repertoire, position, item
         FROM current_content WHERE target_id = 'r1:p1'",
        [],
    ).unwrap_err(), SQLITE_CONSTRAINT_UNIQUE);
    assert_constraint(
        transaction
            .execute(
                "INSERT INTO line_targets VALUES ('line-1', 99, 'r1', 't1')",
                [],
            )
            .unwrap_err(),
        SQLITE_CONSTRAINT_FOREIGNKEY,
    );
    assert_constraint(transaction.execute(
        "INSERT INTO assessments SELECT 99, dataset_id, target_id, version, attempt_id, provider, event_id, record
         FROM assessments WHERE attempt_id = 'attempt-1'",
        [],
    ).unwrap_err(), SQLITE_CONSTRAINT_UNIQUE);
    transaction.execute(
        "INSERT INTO assessments SELECT 98, dataset_id, target_id, version, NULL, 'provider', 'event', record
         FROM assessments WHERE attempt_id = 'attempt-1'",
        [],
    ).unwrap();
    assert_constraint(transaction.execute(
        "INSERT INTO assessments SELECT 97, dataset_id, target_id, version, NULL, provider, event_id, record
         FROM assessments WHERE ordinal = 98",
        [],
    ).unwrap_err(), SQLITE_CONSTRAINT_UNIQUE);
    assert_constraint(
        transaction
            .execute("INSERT INTO receipts SELECT * FROM receipts", [])
            .unwrap_err(),
        SQLITE_CONSTRAINT_PRIMARYKEY,
    );
    transaction.rollback().unwrap();
    assert_eq!(store.snapshot().unwrap(), before);
}

#[test]
fn historical_receipts_and_content_survive_edits_and_restore() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("study.sqlite");
    let mut store = create(&path, demo_catalog());
    let issued = issue(&mut store);
    commit(&mut store, &issued, successful_answer_writes(&issued)).unwrap();
    let answered = store.snapshot().unwrap();
    let target = TargetId::from("r1:p1");
    let mut catalog = answered.catalog.clone();
    let item = catalog.items.get_mut(&target).unwrap();
    item.version = ContentVersion(2);
    item.definition.decisions[0].expected = "d2d4".into();
    commit(&mut store, &answered, vec![Write::Catalog(catalog)]).unwrap();
    let edited = store.snapshot().unwrap();
    assert_eq!(edited.receipts, answered.receipts);
    assert_eq!(edited.catalog.items[&target].version, ContentVersion(2));
    let empty_restore = |dataset: &str| Write::Restore {
        dataset: dataset.into(),
        catalog: Catalog::default(),
        learning: BTreeMap::new(),
        policy: policy().settings(),
    };
    commit(&mut store, &edited, vec![empty_restore("dataset-b")]).unwrap();
    let empty = store.snapshot().unwrap();
    commit(&mut store, &empty, vec![empty_restore("dataset-c")]).unwrap();
    drop(store);

    let mut store = SqliteStore::open(&path).unwrap();
    let restored = store.snapshot().unwrap();
    assert_eq!(
        restored.receipts[&SubmissionId::from("submission-1")],
        answered.receipts[&SubmissionId::from("submission-1")]
    );
    assert_eq!(restored.assessments, answered.assessments);
    assert_eq!(restored.content_history, edited.content_history);
    assert_eq!(
        restored.content_history.last().unwrap().item.version,
        ContentVersion(2)
    );
    assert!(matches!(
        commit(&mut store, &restored, vec![empty_restore("dataset-b")]),
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(store.snapshot().unwrap(), restored);
}

#[test]
fn create_protects_existing_data_and_open_rejects_missing_or_future_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("study.sqlite");
    assert!(SqliteStore::open(&path).is_err());
    for non_file in ["", ":memory:"] {
        assert!(matches!(
            SqliteStore::create(
                non_file,
                "dataset".into(),
                Catalog::default(),
                policy().settings()
            ),
            Err(StoreError::Invalid(_))
        ));
    }
    assert!(!path.exists());
    let store = create(&path, demo_catalog());
    let before = store.snapshot().unwrap();
    assert!(matches!(
        SqliteStore::create(
            &path,
            "different-dataset".into(),
            Catalog::default(),
            policy().settings()
        ),
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(store.snapshot().unwrap(), before);
    store
        .connection
        .pragma_update(None, "user_version", 99)
        .unwrap();
    assert!(matches!(
        SqliteStore::open(&path),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(schema_version(&store.connection).unwrap(), 99);
}

#[test]
fn current_document_source_catalog_learning_and_receipt_roll_back_together() {
    use crate::{ContentReceipt, ContentSource};

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("document.sqlite");
    let catalog = demo_catalog();
    let mut store = create(&path, catalog.clone());
    let before = store.snapshot().unwrap();
    let (repertoire, target) = catalog
        .items
        .values()
        .find_map(|item| match &item.definition.kind {
            tempo_chess::ContentKind::Repertoire { repertoire, .. } => {
                Some((repertoire.as_str(), &item.target))
            }
            _ => None,
        })
        .unwrap();
    let mut renamed = catalog.clone();
    renamed.items.get_mut(target).unwrap().label = "Updated metadata".into();
    let mut learning = before.learning[target].clone();
    learning.first_introduced = Some(tempo_types::StudyDay(1));
    let commit = Commit {
        expected_dataset: before.dataset.clone(),
        expected_revision: before.revision,
        writes: vec![
            Write::Catalog(renamed),
            Write::Learning {
                target: target.clone(),
                state: learning,
            },
        ],
    };
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER reject_receipt BEFORE INSERT ON content_receipts
         BEGIN SELECT RAISE(ABORT, 'injected receipt failure'); END;",
        )
        .unwrap();

    let stamp = crate::ModelInputStamp {
        key: "fixture-model-input".into(),
        targets: crate::model_targets(&catalog, repertoire),
    };
    let content_revision = store.game_comparison_marker().unwrap().1.content;
    let save = |store: &mut SqliteStore| {
        store.commit_content(
            commit.clone(),
            Some(ContentSource::RepertoireDocument {
                id: "source",
                pgn: "1. e4 *",
                repertoire_id: repertoire,
                expected_content_revision: content_revision,
                model_input: &stamp,
                forecast: None,
            }),
            Some(ContentReceipt {
                request: "request",
                reply: "{}",
            }),
        )
    };
    assert!(save(&mut store).is_err());
    assert_eq!(store.snapshot().unwrap(), before);
    assert_eq!(store.repertoire_document(repertoire).unwrap().pgn, None);
    assert!(store.content_source_ids().unwrap().is_empty());
    assert_eq!(store.content_receipt("request").unwrap(), None);
    store
        .connection
        .execute("DROP TRIGGER reject_receipt", [])
        .unwrap();
    save(&mut store).unwrap();
    assert_eq!(
        store
            .repertoire_document(repertoire)
            .unwrap()
            .pgn
            .as_deref(),
        Some("1. e4 *")
    );
    assert_eq!(
        store.content_receipt("request").unwrap().as_deref(),
        Some("{}")
    );
}

#[test]
fn scoped_read_distinguishes_unloaded_absent_and_present_targets() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = create(&directory.path().join("scope.sqlite"), demo_catalog());
    let issued = issue(&mut store);
    let active = &issued.attempts[issued.active_attempt.as_ref().unwrap()].attempt;
    let absent = TargetId::from("missing-target");
    let unloaded = issued
        .catalog
        .items
        .keys()
        .find(|id| **id != active.target)
        .unwrap();
    let read = store
        .study_state(StudyScope {
            target: Some(&absent),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(read.attempts.len(), 1);
    assert_eq!(
        read.target(&active.target).unwrap(),
        Some((
            &issued.catalog.items[&active.target],
            &issued.learning[&active.target]
        ))
    );
    assert!(read.target(&absent).unwrap().is_none());
    assert!(read.target(unloaded).is_err());
    assert!(read.complete_catalog().is_err());

    let complete = store
        .study_state(StudyScope {
            catalog: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        complete.complete_catalog().unwrap(),
        (&issued.catalog, &issued.learning)
    );
    assert!(complete.target(unloaded).unwrap().is_some());

    let content = store.content_state().unwrap();
    assert_eq!(content.catalog, issued.catalog);
    assert_eq!(content.learning, issued.learning);
    assert_eq!(content.active_attempt, issued.active_attempt);
    assert_eq!(content.revision, issued.revision);
    assert_eq!(content.content_revision, Revision(0));
}

#[test]
fn study_lead_in_backfill_only_changes_current_metadata_and_rejects_stale_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("study.sqlite");
    let mut store = create(&path, demo_catalog());
    let before = issue(&mut store);
    let readonly = SqliteStore::open_read_only(&path).unwrap();
    let content_revision = readonly.content_state().unwrap().content_revision;
    let target = TargetId::from("r1:p2");
    let initial = tempo_chess::apply_setup_move(tempo_chess::START_FEN, "e2e4").unwrap();
    let solver = &before.catalog.items[&target].definition.decisions[0].fen;
    let context = tempo_chess::OpponentLeadIn::verified(&initial, "e7e5", solver).unwrap();
    let updates = BTreeMap::from([(target.clone(), LeadInEvidence::Verified(context.clone()))]);

    store
        .backfill_opponent_lead_ins(&before.dataset, before.revision, &updates)
        .unwrap();
    let after = readonly.snapshot().unwrap();
    assert_eq!(after.learning, before.learning);
    assert_eq!(after.attempts, before.attempts);
    assert_eq!(after.active_attempt, before.active_attempt);
    assert_eq!(after.content_history, before.content_history);
    assert_eq!(
        after.catalog.items[&target]
            .opponent_lead_in
            .verified_move(),
        Some(&context)
    );
    assert_eq!(
        readonly.content_state().unwrap().content_revision,
        content_revision
    );
    assert!(
        store
            .backfill_opponent_lead_ins(&before.dataset, before.revision, &updates)
            .is_err()
    );
    assert!(
        store
            .backfill_opponent_lead_ins(&before.dataset, after.revision, &updates)
            .is_err()
    );
    assert_eq!(
        readonly.snapshot().unwrap(),
        after,
        "a failed metadata write must roll back the revision"
    );
}
