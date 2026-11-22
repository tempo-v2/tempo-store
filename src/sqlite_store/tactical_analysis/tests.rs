use super::*;
use std::collections::BTreeMap;
use tempo_chess::{ExactSequenceGrader, START_FEN, TacticRecord, compile_tactics};
use tempo_learning::{DailyLearning, LearningPolicy};
use tempo_study::{Exercise, start_selected_exercise};
use tempo_types::{StudyDay, StudyTime};

fn fixture(path: &std::path::Path) -> (SqliteStore, TargetId) {
    let record = TacticRecord {
        namespace: "test".into(),
        source_id: "puzzle".into(),
        solver_fen: START_FEN.into(),
        solution: vec!["e2e4".into()],
        label: "A puzzle".into(),
        provenance: vec![],
    };
    let target = record.target_id();
    let store = SqliteStore::create(
        path,
        "dataset".into(),
        compile_tactics(&[record]).unwrap(),
        DailyLearning::default().settings(),
    )
    .unwrap();
    (store, target)
}

fn game(store: &SqliteStore, id: &str) {
    store
        .connection
        .execute(
            "INSERT INTO games(dataset_id,source,id,white,black,fetched_ms,pgn,mainline_signature)
        VALUES('dataset','pgn',?1,'White','Black',0,'1.e4 e5 1-0',?1)",
            [id],
        )
        .unwrap();
    store
        .connection
        .execute(
            "INSERT INTO game_comparisons(dataset_id,id,source,game_id,summary)
        VALUES('dataset',?1,'pgn',?1,json_object('learner','white'))",
            [id],
        )
        .unwrap();
}

fn analysis(id: &str, target: Option<&TargetId>) -> TacticalAnalysis {
    TacticalAnalysis {
        position: None,
        source: "pgn".into(),
        game_id: id.into(),
        learner: "white".into(),
        report: "hanging_material".into(),
        analysis_version: "v1".into(),
        game_fingerprint: id.into(),
        analyzed_at_ms: 1_000,
        decisions_scanned: 1,
        status: TacticalAnalysisStatus::Complete,
        error: None,
        observations: target
            .map(|target| TacticalObservation {
                request_scope: TacticalRequestScope::Finding,
                producer: None,
                ply: 0,
                motif: "hanging_material".into(),
                outcome: "missed_good_capture".into(),
                details_json: "{\"played\":\"d2d4\"}".into(),
                train: Some(TacticalPuzzleRef {
                    target: target.clone(),
                    version: ContentVersion(1),
                }),
            })
            .into_iter()
            .collect(),
    }
}

fn accept(
    store: &mut SqliteStore,
    analysis: &TacticalAnalysis,
    target: &TargetId,
) -> TacticalAcceptance {
    let snapshot = store.snapshot().unwrap();
    let mut targets = BTreeMap::new();
    if !snapshot.learning[target].introduced && !analysis.observations.is_empty() {
        targets.insert(
            target.clone(),
            TacticalTargetChange {
                content: None,
                introduction: Some(
                    DailyLearning::introduce_capture(
                        &snapshot.learning[target],
                        StudyTime {
                            day: StudyDay(10),
                            unix_ms: 1_000,
                        },
                    )
                    .unwrap(),
                ),
            },
        );
    }
    store
        .commit_tactical_analysis(
            TacticalCommit {
                expected_dataset: snapshot.dataset,
                expected_revision: snapshot.revision,
                targets,
            },
            analysis,
        )
        .unwrap()
}

fn issue_request(store: &mut SqliteStore, request: &TacticalRequest, name: &str) -> StoredAttempt {
    let snapshot = store.snapshot().unwrap();
    let item = &snapshot.catalog.items[&request.target];
    let exercise = Exercise {
        target: item.target.clone(),
        version: item.version,
        definition: item.definition.clone(),
    };
    let attempt = start_selected_exercise(
        &exercise,
        &ExactSequenceGrader,
        snapshot.dataset.clone(),
        || name.into(),
    )
    .unwrap();
    let record = StoredAttempt {
        attempt,
        issued_at: StudyTime {
            day: StudyDay(10),
            unix_ms: 2_000,
        },
        issued_revision: Some(snapshot.revision),
        issued_miss_revision: snapshot.learning[&request.target].miss_revision,
        mode: ActivityMode::Practice,
        route: None,
        first_introduction: false,
        exercise_name: None,
        tactic_attribution: None,
        relevance_reason: None,
        lesson: None,
        acceptance: None,
        revealed_decision: None,
        opponent_lead_in: None,
        repertoire_warmup: false,
    };
    store
        .commit_daily_issuance(
            Commit {
                expected_dataset: snapshot.dataset,
                expected_revision: snapshot.revision,
                writes: vec![
                    Write::Issue(record.clone()),
                    Write::Active(Some(record.attempt.id.clone())),
                ],
            },
            StudyScope {
                target: Some(&request.target),
                ..Default::default()
            },
            Some(request.id),
            StudyOrderingWrite::OweRegularReview,
        )
        .unwrap();
    record
}

#[test]
fn atomic_acceptance_replay_and_changed_detector_version_do_not_duplicate_real_occurrence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("study.sqlite");
    let (mut store, target) = fixture(&path);
    game(&store, "one");
    let original = store.snapshot().unwrap();
    let request = analysis("one", Some(&target));
    let accepted = accept(&mut store, &request, &target);
    assert_eq!((accepted.new_observations, accepted.new_requests), (1, 1));
    assert_eq!(accepted.new_request_targets, vec![target.clone()]);
    let replay = store
        .commit_tactical_analysis(
            TacticalCommit {
                expected_dataset: original.dataset.clone(),
                expected_revision: original.revision,
                targets: BTreeMap::new(),
            },
            &request,
        )
        .unwrap();
    assert!(replay.replayed);
    assert!(replay.new_request_targets.is_empty());
    assert_eq!(replay.revision, accepted.revision);
    drop(store);
    let mut store = SqliteStore::open(&path).unwrap();
    let mut revised = request.clone();
    revised.analysis_version = "v2".into();
    let second = accept(&mut store, &revised, &target);
    assert_eq!((second.new_observations, second.new_requests), (0, 0));
    assert!(second.new_request_targets.is_empty());
    assert_eq!(
        store
            .pending_tactical_requests(&original.dataset)
            .unwrap()
            .len(),
        1
    );
    let stats = store
        .tactical_statistics(&original.dataset, "hanging_material", "v2")
        .unwrap();
    assert_eq!(
        (
            stats.completed_games,
            stats.observations,
            stats.pending_requests
        ),
        (1, 1, 1)
    );
    let mut inconsistent = revised;
    inconsistent.observations[0].details_json = "{\"changed\":true}".into();
    let before = store.snapshot().unwrap();
    assert!(
        store
            .commit_tactical_analysis(
                TacticalCommit {
                    expected_dataset: before.dataset.clone(),
                    expected_revision: before.revision,
                    targets: BTreeMap::new(),
                },
                &inconsistent
            )
            .is_err()
    );
    assert_eq!(before, store.snapshot().unwrap());
}

#[test]
fn a_bound_finding_replays_after_its_unadopted_proposal_resolves_to_a_different_target() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("study.sqlite");
    let (mut store, target) = fixture(&path);
    let dataset: DatasetId = "dataset".into();
    game(&store, "one");
    let first = analysis("one", Some(&target));
    accept(&mut store, &first, &target);
    let request = store.pending_tactical_requests(&dataset).unwrap().remove(0);
    let issued = issue_request(&mut store, &request, "bound");
    let before = store.snapshot().unwrap();
    let mut revised = analysis("one", Some(&"proposed".into()));
    revised.analysis_version = "revised".into();
    revised.observations[0].details_json = r#"{"answer":"d2d4"}"#.into();
    let saved = accept(&mut store, &revised, &target);
    assert_eq!(saved.new_requests, 0);
    drop(store);

    let mut store = SqliteStore::open(&path).unwrap();
    revised.observations[0].train.as_mut().unwrap().target = "independent-alias".into();
    let replay = accept(&mut store, &revised, &target);
    assert!(replay.replayed);
    assert_eq!(replay.revision, saved.revision);
    let after = store.snapshot().unwrap();
    assert_eq!(after.catalog, before.catalog);
    assert_eq!(after.learning, before.learning);
    assert_eq!(after.active_attempt, Some(issued.attempt.id));

    revised.observations[0].details_json = r#"{"answer":"c2c4"}"#.into();
    assert!(matches!(
        store.commit_tactical_analysis(
            TacticalCommit {
                expected_dataset: dataset,
                expected_revision: after.revision,
                targets: BTreeMap::new(),
            },
            &revised,
        ),
        Err(StoreError::Conflict(_))
    ));
}

#[test]
fn zero_based_occurrences_retract_on_replacement_without_erasing_audit_or_mixing_versions() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, target) = fixture(&dir.path().join("study.sqlite"));
    let dataset: DatasetId = "dataset".into();
    game(&store, "one");
    let original = analysis("one", Some(&target));
    assert_eq!(original.observations[0].ply, 0);
    accept(&mut store, &original, &target);
    let first = store
        .tactical_observations(&dataset, "hanging_material", "v1", None, 10)
        .unwrap();
    assert_eq!(first.observations[0].observation.ply, 0);
    game(&store, "two");
    accept(&mut store, &analysis("two", Some(&target)), &target);

    // The accepted game's move history can change without a report upgrade.
    store
        .connection
        .execute(
            "UPDATE games SET mainline_signature='one-replaced' WHERE id='one'",
            [],
        )
        .unwrap();
    let mut replacement = analysis("one", None);
    replacement.game_fingerprint = "one-replaced".into();
    accept(&mut store, &replacement, &target);
    let stats = store
        .tactical_statistics(&dataset, "hanging_material", "v1")
        .unwrap();
    assert_eq!(
        (
            stats.completed_games,
            stats.observations,
            stats.pending_requests
        ),
        (2, 1, 1)
    );
    let page = store
        .tactical_observations(&dataset, "hanging_material", "v1", None, 1)
        .unwrap();
    assert_eq!(page.observations.len(), 1);
    assert_eq!(page.observations[0].game_id, "two");
    assert!(page.next_before.is_none());
    let retained: (u64, bool, bool) = store
        .connection
        .query_row(
            "SELECT count(*),o.current,r.cancelled FROM tactical_observations o
         JOIN tactical_requests r ON r.observation_id=o.id WHERE o.game_id='one'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(retained, (1, false, true));

    // A failed upgrade leaves accepted prior facts intact, but they do not leak
    // into the new version's report or its pagination.
    let mut failed = analysis("two", None);
    failed.analysis_version = "v2".into();
    failed.status = TacticalAnalysisStatus::Failed;
    failed.decisions_scanned = 0;
    accept(&mut store, &failed, &target);
    assert_eq!(
        store
            .tactical_observations(&dataset, "hanging_material", "v1", None, 1)
            .unwrap()
            .observations
            .len(),
        1
    );
    let empty = store
        .tactical_observations(&dataset, "hanging_material", "v2", None, 1)
        .unwrap();
    assert!(empty.observations.is_empty());
    assert!(empty.next_before.is_none());
    let failed_stats = store
        .tactical_statistics(&dataset, "hanging_material", "v2")
        .unwrap();
    assert_eq!(
        (failed_stats.failed_games, failed_stats.observations),
        (1, 0)
    );
}

#[test]
fn cancelled_request_projection_is_coherent_after_report_only_reanalysis() {
    for outcome in ["declined_inferior_capture", "uncertain"] {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, target) = fixture(&dir.path().join("study.sqlite"));
        let dataset: DatasetId = "dataset".into();
        game(&store, "one");
        let original = analysis("one", Some(&target));
        accept(&mut store, &original, &target);
        let request = store.pending_tactical_requests(&dataset).unwrap().remove(0);
        let before = store.snapshot().unwrap();

        let mut revised = original;
        revised.analysis_version = "v2".into();
        revised.observations[0].outcome = outcome.into();
        revised.observations[0].train = None;
        let accepted = accept(&mut store, &revised, &target);
        assert_eq!((accepted.new_observations, accepted.new_requests), (0, 0));
        let page = store
            .tactical_observations(&dataset, "hanging_material", "v2", None, 10)
            .unwrap();
        assert_eq!(page.observations.len(), 1);
        let row = &page.observations[0];
        assert_eq!(row.id, request.observation_id);
        assert_eq!(row.observation.outcome, outcome);
        assert!(row.observation.train.is_none());
        assert!(row.request_id.is_none());
        assert!(!row.fulfilled);
        assert!(
            store
                .pending_tactical_requests(&dataset)
                .unwrap()
                .is_empty()
        );
        let stats = store
            .tactical_statistics(&dataset, "hanging_material", "v2")
            .unwrap();
        assert_eq!((stats.observations, stats.pending_requests), (1, 0));
        assert_eq!(stats.outcomes[outcome], 1);
        let after = store.snapshot().unwrap();
        assert_eq!(after.catalog, before.catalog);
        assert_eq!(after.learning, before.learning);

        let audit: (u64, bool, bool) = store
            .connection
            .query_row(
                "SELECT observation_id,cancelled,fulfilled FROM tactical_requests WHERE id=?1",
                [request.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(audit, (request.observation_id, true, false));
    }
}

#[test]
fn late_acceptance_failure_rolls_back_content_learning_observation_and_request() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, target) = fixture(&dir.path().join("study.sqlite"));
    game(&store, "one");
    let before = store.snapshot().unwrap();
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER fail_analysis BEFORE INSERT ON tactical_analyses
        BEGIN SELECT RAISE(ABORT,'injected'); END;",
        )
        .unwrap();
    let state = DailyLearning::introduce_capture(
        &before.learning[&target],
        StudyTime {
            day: StudyDay(10),
            unix_ms: 1_000,
        },
    )
    .unwrap();
    let mut item = before.catalog.items[&target].clone();
    item.label = "accepted label".into();
    let commit = TacticalCommit {
        expected_dataset: before.dataset.clone(),
        expected_revision: before.revision,
        targets: [(
            target.clone(),
            TacticalTargetChange {
                content: Some(item),
                introduction: Some(state),
            },
        )]
        .into(),
    };
    let generation = store.tactical_report_generation(&before.dataset).unwrap();
    assert!(
        store
            .commit_tactical_analysis(commit.clone(), &analysis("one", Some(&target)))
            .is_err()
    );
    assert_eq!(before, store.snapshot().unwrap());
    assert_eq!(
        generation,
        store.tactical_report_generation(&before.dataset).unwrap()
    );
    assert!(
        store
            .pending_tactical_requests(&before.dataset)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .tactical_observations(&before.dataset, "hanging_material", "v1", None, 10)
            .unwrap()
            .observations
            .is_empty()
    );
    store
        .connection
        .execute_batch("DROP TRIGGER fail_analysis")
        .unwrap();
    assert_eq!(
        store
            .commit_tactical_analysis(commit, &analysis("one", Some(&target)))
            .unwrap()
            .new_requests,
        1
    );
}

#[test]
fn older_completion_fulfills_only_its_bound_request_and_replay_cannot_consume_newer_miss() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("study.sqlite");
    let (mut store, target) = fixture(&path);
    game(&store, "one");
    accept(&mut store, &analysis("one", Some(&target)), &target);
    let dataset: DatasetId = "dataset".into();
    let first = store.pending_tactical_requests(&dataset).unwrap().remove(0);
    assert!(
        !store
            .has_pending_tactical_request_for_attempt(&dataset, &"first-attempt".into())
            .unwrap()
    );
    let issued = issue_request(&mut store, &first, "first-attempt");
    assert!(
        store
            .has_pending_tactical_request_for_attempt(&dataset, &issued.attempt.id)
            .unwrap()
    );
    assert!(
        !store
            .has_pending_tactical_request_for_attempt(&dataset, &"unrelated".into())
            .unwrap()
    );
    assert!(
        store
            .has_pending_tactical_request_for_attempt(&"wrong-dataset".into(), &issued.attempt.id)
            .is_err()
    );
    game(&store, "two");
    accept(&mut store, &analysis("two", Some(&target)), &target);
    assert_eq!(
        store.snapshot().unwrap().active_attempt,
        Some(issued.attempt.id.clone())
    );
    let pending = store.pending_tactical_requests(&dataset).unwrap();
    assert_eq!(pending.len(), 1);
    assert_ne!(pending[0].id, first.id);
    let snapshot = store.snapshot().unwrap();
    let learning = snapshot.learning.clone();
    let result = tempo_study::submit(
        &ExactSequenceGrader,
        &snapshot.catalog.items[&target].definition,
        &issued.attempt,
        Revision(0),
        &"e2e4".into(),
    )
    .unwrap();
    let receipt = Receipt {
        id: "answered".into(),
        command: "answer one".into(),
        result: ReceiptResult::Study(AnswerResult {
            attempt: result.attempt.clone(),
            feedback: Some(result.feedback),
            assessment: None,
            learning_effect: None,
        }),
    };
    let completed = StoredAttempt {
        attempt: result.attempt,
        ..issued.clone()
    };
    store.connection.execute_batch("CREATE TRIGGER fail_answer BEFORE INSERT ON receipts BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    let commit = Commit {
        expected_dataset: dataset.clone(),
        expected_revision: snapshot.revision,
        writes: vec![
            Write::Progress(completed),
            Write::Active(None),
            Write::Receipt(receipt.clone()),
        ],
    };
    let scope = StudyScope {
        attempt: Some(&issued.attempt.id),
        submission: Some(&receipt.id),
        ..Default::default()
    };
    assert!(store.commit_study(commit.clone(), scope).is_err());
    assert!(
        store
            .has_pending_tactical_request_for_attempt(&dataset, &issued.attempt.id)
            .unwrap()
    );
    assert_eq!(store.snapshot().unwrap(), snapshot);
    assert_eq!(
        store
            .tactical_statistics(&dataset, "hanging_material", "v1")
            .unwrap()
            .pending_requests,
        2
    );
    store
        .connection
        .execute_batch("DROP TRIGGER fail_answer")
        .unwrap();
    store.commit_study(commit, scope).unwrap();
    assert!(
        !store
            .has_pending_tactical_request_for_attempt(&dataset, &issued.attempt.id)
            .unwrap()
    );
    assert_eq!(store.snapshot().unwrap().learning, learning);
    drop(store);
    let mut store = SqliteStore::open(&path).unwrap();
    assert!(
        !store
            .has_pending_tactical_request_for_attempt(&dataset, &issued.attempt.id)
            .unwrap()
    );
    assert_eq!(store.pending_tactical_requests(&dataset).unwrap(), pending);
    assert_eq!(store.snapshot().unwrap().receipts[&receipt.id], receipt);
    let mut revised = analysis("one", Some(&target));
    revised.analysis_version = "v2".into();
    accept(&mut store, &revised, &target);
    assert_eq!(store.pending_tactical_requests(&dataset).unwrap(), pending);
}

#[test]
fn abandonment_releases_exact_request_and_failed_binding_rolls_back_issuance() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, target) = fixture(&dir.path().join("study.sqlite"));
    game(&store, "one");
    accept(&mut store, &analysis("one", Some(&target)), &target);
    let dataset: DatasetId = "dataset".into();
    let request = store.pending_tactical_requests(&dataset).unwrap().remove(0);
    let record = issue_request(&mut store, &request, "first");
    assert!(
        store
            .has_pending_tactical_request_for_attempt(&dataset, &record.attempt.id)
            .unwrap()
    );
    let snapshot = store.snapshot().unwrap();
    let abandoned = tempo_study::abandon(&record.attempt, Revision(0)).unwrap();
    store
        .commit_study(
            Commit {
                expected_dataset: dataset.clone(),
                expected_revision: snapshot.revision,
                writes: vec![
                    Write::Progress(StoredAttempt {
                        attempt: abandoned,
                        ..record.clone()
                    }),
                    Write::Active(None),
                ],
            },
            StudyScope {
                attempt: Some(&record.attempt.id),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        !store
            .has_pending_tactical_request_for_attempt(&dataset, &record.attempt.id)
            .unwrap()
    );
    assert_eq!(
        store.pending_tactical_requests(&dataset).unwrap(),
        vec![request.clone()]
    );
    let before = store.snapshot().unwrap();
    assert!(store.regular_review_owed(&dataset).unwrap());
    let mut attempted = record;
    attempted.attempt.id = "nonexistent-request-attempt".into();
    assert!(
        store
            .commit_daily_issuance(
                Commit {
                    expected_dataset: dataset.clone(),
                    expected_revision: before.revision,
                    writes: vec![
                        Write::Issue(attempted.clone()),
                        Write::Active(Some(attempted.attempt.id.clone()))
                    ],
                },
                StudyScope {
                    target: Some(&target),
                    ..Default::default()
                },
                Some(request.id + 1),
                StudyOrderingWrite::SatisfyRegularReview,
            )
            .is_err()
    );
    assert_eq!(store.snapshot().unwrap(), before);
    assert!(store.regular_review_owed(&dataset).unwrap());
    assert_eq!(
        store.pending_tactical_requests(&dataset).unwrap(),
        vec![request]
    );
}

#[test]
fn pending_keyset_sweep_covers_more_than_501_undated_games_and_records_empty_and_failed_results() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, target) = fixture(&dir.path().join("study.sqlite"));
    for index in 0..600 {
        game(&store, &format!("game-{index:03}"));
    }
    let dataset: DatasetId = "dataset".into();
    let mut cursor = None;
    let mut seen = BTreeSet::new();
    loop {
        let page = store
            .pending_tactical_analyses(&dataset, "hanging_material", "v1", cursor.as_ref(), 256)
            .unwrap();
        if page.is_empty() {
            break;
        }
        if cursor.is_none() {
            assert_eq!(page[0].game.id, "game-599");
        }
        cursor = page.last().map(|entry| entry.cursor.clone());
        for entry in page {
            assert!(entry.game.start_ms.is_none());
            assert!(seen.insert(entry.game.id));
        }
    }
    assert_eq!(seen.len(), 600);
    accept(&mut store, &analysis("game-599", None), &target);
    let mut failed = analysis("game-598", None);
    failed.status = TacticalAnalysisStatus::Failed;
    failed.error = Some("engine unavailable".into());
    failed.decisions_scanned = 0;
    accept(&mut store, &failed, &target);
    let stats = store
        .tactical_statistics(&dataset, "hanging_material", "v1")
        .unwrap();
    assert_eq!(
        (
            stats.completed_games,
            stats.failed_games,
            stats.pending_games,
            stats.observations
        ),
        (1, 1, 0, 0)
    );
    assert_eq!(
        store
            .pending_tactical_analyses(&dataset, "hanging_material", "v1", None, 1)
            .unwrap()[0]
            .game
            .id,
        "game-597"
    );
    assert_eq!(
        store
            .pending_tactical_analyses(&dataset, "hanging_material", "v2", None, 1)
            .unwrap()[0]
            .game
            .id,
        "game-599"
    );
}

fn remove_v15_projection(store: &SqliteStore) {
    let mut statement = store
        .connection
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type='trigger' AND name LIKE 'tactical_report_%'",
        )
        .unwrap();
    let triggers = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for trigger in triggers {
        store
            .connection
            .execute_batch(&format!("DROP TRIGGER {trigger}"))
            .unwrap();
    }
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
         DROP TABLE repertoire_coverage_gaps;
         DROP TABLE repertoire_coverage;
         DROP INDEX tactical_report_current_game;
         DROP INDEX tactical_report_decisions;
         DROP INDEX tactical_report_comparisons;
         DROP TABLE tactical_report_generation;
         DROP TABLE tactical_sources;
         DROP TABLE tactical_identities;
         DROP TABLE repertoire_coverage_start; PRAGMA user_version=14;",
        )
        .unwrap();
}

#[test]
fn schema_13_migration_is_additive_and_an_interrupted_attempt_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("study.sqlite");
    let mut store = SqliteStore::create(
        &path,
        "dataset-a".into(),
        tempo_chess::demo_catalog(),
        crate::test_support::policy().settings(),
    )
    .unwrap();
    let initial = store.snapshot().unwrap();
    let issued = crate::test_support::issued_record(&initial);
    store
        .commit(Commit {
            expected_dataset: initial.dataset,
            expected_revision: initial.revision,
            writes: vec![
                Write::Issue(issued.clone()),
                Write::Active(Some(issued.attempt.id)),
            ],
        })
        .unwrap();
    let issued = store.snapshot().unwrap();
    store
        .commit(Commit {
            expected_dataset: issued.dataset.clone(),
            expected_revision: issued.revision,
            writes: crate::test_support::successful_answer_writes(&issued),
        })
        .unwrap();
    let answered = store.snapshot().unwrap();
    let mut pending = crate::test_support::issued_record(&answered);
    pending.attempt.id = "outstanding-old-attempt".into();
    store
        .commit(Commit {
            expected_dataset: answered.dataset,
            expected_revision: answered.revision,
            writes: vec![
                Write::Issue(pending.clone()),
                Write::Active(Some(pending.attempt.id)),
            ],
        })
        .unwrap();
    let before = store.snapshot().unwrap();
    assert_eq!(before.receipts.len(), 1);
    assert_eq!(before.assessments.len(), 1);
    assert!(before.active_attempt.is_some());
    remove_v15_projection(&store);
    store.connection.execute_batch("DROP TABLE tactical_requests; DROP TABLE tactical_observations; DROP TABLE tactical_analyses; DROP TABLE tactical_positions; DROP TABLE tactical_enrollment; DROP TABLE tactical_pause; DROP TABLE tactical_study_turn; PRAGMA user_version=13;").unwrap();
    store
        .connection
        .execute_batch("CREATE TABLE tactical_requests(blocker INTEGER)")
        .unwrap();
    drop(store);
    assert!(SqliteStore::open(&path).is_err());
    let connection = Connection::open(&path).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 13);
    let created:i64=connection.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('tactical_analyses','tactical_observations')",[],|r|r.get(0)).unwrap();
    assert_eq!(created, 0);
    connection
        .execute_batch("DROP TABLE tactical_requests")
        .unwrap();
    drop(connection);
    let upgraded = SqliteStore::open(&path).unwrap();
    assert_eq!(upgraded.snapshot().unwrap(), before);
    assert_eq!(
        super::super::schema_version(&upgraded.connection).unwrap(),
        super::super::SCHEMA_VERSION
    );
    drop(upgraded);
    assert_eq!(
        SqliteStore::open(&path).unwrap().snapshot().unwrap(),
        before
    );
}

#[test]
fn archived_enrollment_keeps_the_selected_verified_participant() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = fixture(&dir.path().join("study.sqlite"));
    game(&store, "two-perspectives");
    store
        .connection
        .execute(
            "INSERT INTO game_comparisons(dataset_id,id,source,game_id,summary)
         VALUES('dataset','later','pgn','two-perspectives',json_object('learner','black'))",
            [],
        )
        .unwrap();
    let dataset = DatasetId("dataset".into());
    assert!(
        store
            .queue_game_analysis(&dataset, "pgn", "two-perspectives", "unknown")
            .is_err()
    );
    store
        .queue_game_analysis(&dataset, "pgn", "two-perspectives", "White")
        .unwrap();
    let queued = store
        .next_analysis_enrollment(&dataset, "v1")
        .unwrap()
        .unwrap();
    assert_eq!(queued.learner, "white");
}

#[test]
fn retry_and_attribution_follow_explicit_position_contract() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = fixture(&dir.path().join("study.sqlite"));
    game(&store, "position");
    let dataset = DatasetId("dataset".into());
    let member = tempo_chess::DetectorIdentity {
        id: "first".into(),
        revision: "1".into(),
        configuration_fingerprint: "fixture".into(),
    };
    store
        .queue_game_analysis(&dataset, "pgn", "position", "white")
        .unwrap();
    let queued = store
        .next_analysis_enrollment(&dataset, "v1")
        .unwrap()
        .unwrap();
    store
        .expand_analysis_enrollment(&dataset, &queued, "v1", &[member.clone()], &[0, 2])
        .unwrap();
    let first = store
        .next_position_analysis(&dataset, "v1", None)
        .unwrap()
        .unwrap();
    let mut result = analysis("position", None);
    result.position = Some(first.run);
    let mut obsolete = member.clone();
    obsolete.revision = "0".into();
    result.observations.push(TacticalObservation {
        request_scope: TacticalRequestScope::Finding,
        producer: Some(obsolete),
        ply: 0,
        motif: "fixture".into(),
        outcome: "uncertain".into(),
        details_json: "{}".into(),
        train: None,
    });
    let revision = store.snapshot().unwrap().revision;
    let commit = || TacticalCommit {
        expected_dataset: dataset.clone(),
        expected_revision: revision,
        targets: BTreeMap::new(),
    };
    assert!(store.commit_tactical_analysis(commit(), &result).is_err());
    result.observations[0].producer = Some(member);
    store.commit_tactical_analysis(commit(), &result).unwrap();
    let partial = store
        .tactical_statistics(&dataset, "hanging_material", "v1")
        .unwrap();
    assert_eq!(
        (
            partial.completed_games,
            partial.pending_games,
            partial.decisions_scanned
        ),
        (0, 1, 1)
    );
    let second = store
        .next_position_analysis(&dataset, "v1", None)
        .unwrap()
        .unwrap();
    assert_eq!(second.run.ply, 2);
    result.position = Some(second.run);
    result.observations.clear();
    result.status = TacticalAnalysisStatus::Inconclusive;
    store.commit_tactical_analysis(commit(), &result).unwrap();
    let before_retry = store
        .tactical_statistics(&dataset, "hanging_material", "v1")
        .unwrap();
    assert_eq!(
        (
            before_retry.inconclusive_positions,
            before_retry.retryable_positions
        ),
        (1, 1)
    );
    store.retry_position_analysis(&dataset, "v1").unwrap();
    let retry = store
        .next_position_analysis(&dataset, "v1", None)
        .unwrap()
        .unwrap();
    assert_eq!(retry.run.ply, 2);
    assert!(retry.run.deeper);
    result.position = Some(retry.run);
    store.commit_tactical_analysis(commit(), &result).unwrap();
    let exhausted = store
        .tactical_statistics(&dataset, "hanging_material", "v1")
        .unwrap();
    assert_eq!(
        (
            exhausted.inconclusive_positions,
            exhausted.retryable_positions
        ),
        (1, 0)
    );
    store.retry_position_analysis(&dataset, "v1").unwrap();
    assert!(
        store
            .next_position_analysis(&dataset, "v1", None)
            .unwrap()
            .is_none()
    );
}

#[test]
fn archive_coverage_uses_the_requested_report_and_accepted_complete_decision_kind() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = fixture(&dir.path().join("study.sqlite"));
    game(&store, "mixed");
    let dataset = DatasetId("dataset".into());
    let member = |id: &str| tempo_chess::DetectorIdentity {
        id: id.into(),
        revision: "1".into(),
        configuration_fingerprint: "fixture".into(),
    };
    let legacy = member("capture");
    let complete = member("whole-turn");
    store
        .queue_game_analysis(&dataset, "pgn", "mixed", "white")
        .unwrap();
    let queued = store
        .next_analysis_enrollment(&dataset, "v1")
        .unwrap()
        .unwrap();
    store
        .expand_analysis_enrollment(
            &dataset,
            &queued,
            "v1",
            &[legacy.clone(), complete.clone()],
            &[0],
        )
        .unwrap();
    let position = store
        .next_position_analysis(&dataset, "v1", None)
        .unwrap()
        .unwrap();
    let mut result = analysis("mixed", None);
    result.position = Some(position.run);
    result.status = TacticalAnalysisStatus::Inconclusive;
    let snapshot = store.snapshot().unwrap();
    let commit = || TacticalCommit {
        expected_dataset: dataset.clone(),
        expected_revision: snapshot.revision,
        targets: BTreeMap::new(),
    };
    result.observations = vec![
        TacticalObservation {
            producer: Some(legacy),
            ply: 0,
            motif: "hanging_material".into(),
            outcome: "took_good_capture".into(),
            details_json:
                r#"{"assessment":{"analysis":{"status":"complete"},"outcome":"took_good_capture"}}"#
                    .into(),
            train: None,
            request_scope: TacticalRequestScope::Finding,
        },
        TacticalObservation {
            producer: Some(complete),
            ply: 0,
            motif: PERSONAL_DECISION_KIND.into(),
            outcome: "uncertain".into(),
            details_json:
                r#"{"assessment":{"analysis":{"status":"incomplete"},"outcome":"uncertain"}}"#
                    .into(),
            train: None,
            request_scope: TacticalRequestScope::Decision,
        },
    ];
    store.commit_tactical_analysis(commit(), &result).unwrap();
    assert!(
        !store
            .tactical_archive_coverage(&dataset, &result.report, "v1")
            .unwrap()[0]
            .fully_analyzed
    );
    store.retry_position_analysis(&dataset, "v1").unwrap();
    result.position = Some(
        store
            .next_position_analysis(&dataset, "v1", None)
            .unwrap()
            .unwrap()
            .run,
    );
    result.observations[1].outcome = "missed_tactic".into();
    result.observations[1].details_json =
        r#"{"assessment":{"analysis":{"status":"complete"},"outcome":"missed_tactic"}}"#.into();
    // Engine evidence can be complete even when a standalone training check is inconclusive.
    store.commit_tactical_analysis(commit(), &result).unwrap();
    assert!(
        store
            .tactical_archive_coverage(&dataset, &result.report, "v1")
            .unwrap()[0]
            .fully_analyzed
    );
    assert!(
        !store
            .tactical_archive_coverage(&dataset, "another-report", "v1")
            .unwrap()[0]
            .fully_analyzed
    );
}

#[test]
fn retained_decision_replays_after_issuance_and_accepts_legacy_supersession_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("study.sqlite");
    let (mut store, target) = fixture(&path);
    let dataset: DatasetId = "dataset".into();
    game(&store, "one");
    let mut decision = analysis("one", Some(&target));
    decision.observations[0].request_scope = TacticalRequestScope::Decision;
    accept(&mut store, &decision, &target);
    let request = store.pending_tactical_requests(&dataset).unwrap().remove(0);
    decision.analysis_version = "revised-unissued".into();
    let alternative = TacticalPuzzleRef {
        target: "alternate".into(),
        version: ContentVersion(1),
    };
    decision.observations[0].train = Some(alternative.clone());
    decision.observations[0].details_json = r#"{"answer":"d2d4"}"#.into();
    let accepted = accept(&mut store, &decision, &target);
    assert_eq!(accepted.new_requests, 0);
    assert_eq!(
        store.pending_tactical_requests(&dataset).unwrap(),
        vec![request.clone()]
    );
    let issued = issue_request(&mut store, &request, "original-decision");
    drop(store);
    let mut store = SqliteStore::open(&path).unwrap();
    assert!(accept(&mut store, &decision, &target).replayed);
    assert!(
        store
            .has_pending_tactical_request_for_attempt(&dataset, &issued.attempt.id)
            .unwrap()
    );

    // The prior implementation omitted train when a revised issued Decision
    // proposed different content. Reproduce its persisted receipt shape.
    decision.analysis_version = "legacy-issued-revision".into();
    decision.observations[0].train = None;
    let legacy = accept(&mut store, &decision, &target);
    store
        .connection
        .execute(
            "UPDATE tactical_analyses SET acceptance=json_remove(acceptance,'$.pinned_bindings')
         WHERE analysis_version=?1",
            [&decision.analysis_version],
        )
        .unwrap();
    decision.observations[0].train = Some(alternative);
    let replayed = accept(&mut store, &decision, &target);
    assert_eq!(replayed.revision, legacy.revision);
    assert!(replayed.replayed);
    assert!(
        store
            .pending_tactical_requests(&dataset)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.snapshot().unwrap().active_attempt,
        Some(issued.attempt.id)
    );
}

#[test]
fn retained_binding_rejects_staged_catalog_changes_for_an_alternate_target() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, target) = fixture(&dir.path().join("study.sqlite"));
    game(&store, "one");
    accept(&mut store, &analysis("one", Some(&target)), &target);
    let before = store.snapshot().unwrap();
    let alternate = TacticRecord {
        namespace: "test".into(),
        source_id: "alternate".into(),
        solver_fen: START_FEN.into(),
        solution: vec!["d2d4".into()],
        label: "Alternate puzzle".into(),
        provenance: vec![],
    };
    let target_id = alternate.target_id();
    let item = compile_tactics(&[alternate])
        .unwrap()
        .items
        .remove(&target_id)
        .unwrap();
    let mut revised = analysis("one", Some(&target_id));
    revised.analysis_version = "stale-training-plan".into();
    let result = store.commit_tactical_analysis(
        TacticalCommit {
            expected_dataset: before.dataset.clone(),
            expected_revision: before.revision,
            targets: [(
                target_id,
                TacticalTargetChange {
                    content: Some(item),
                    introduction: Some(
                        DailyLearning::introduce_capture(
                            &Default::default(),
                            StudyTime {
                                day: StudyDay(10),
                                unix_ms: 1_000,
                            },
                        )
                        .unwrap(),
                    ),
                },
            )]
            .into(),
        },
        &revised,
    );
    assert!(matches!(result, Err(StoreError::Conflict(_))), "{result:?}");
    assert_eq!(store.snapshot().unwrap(), before);
    assert_eq!(
        store.pending_tactical_requests(&before.dataset).unwrap()[0].target,
        target
    );
    assert!(
        store
            .tactical_observations(
                &before.dataset,
                "hanging_material",
                &revised.analysis_version,
                None,
                1
            )
            .unwrap()
            .observations
            .is_empty()
    );
}

#[test]
fn legacy_finding_preserves_binding_previously_moved_to_a_decision() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, target) = fixture(&dir.path().join("study.sqlite"));
    let dataset: DatasetId = "dataset".into();
    game(&store, "one");
    let mut finding = analysis("one", Some(&target));
    accept(&mut store, &finding, &target);
    let request = store.pending_tactical_requests(&dataset).unwrap().remove(0);

    let mut decision = finding.clone();
    decision.analysis_version = "legacy-personal".into();
    decision.observations[0].motif = PERSONAL_DECISION_KIND.into();
    decision.observations[0].request_scope = TacticalRequestScope::Decision;
    accept(&mut store, &decision, &target);
    let decision_observation: u64 = store
        .connection
        .query_row(
            "SELECT id FROM tactical_observations WHERE motif=?1",
            [PERSONAL_DECISION_KIND],
            |row| row.get(0),
        )
        .unwrap();
    store
        .connection
        .execute(
            "UPDATE tactical_requests SET observation_id=?1 WHERE id=?2",
            params![decision_observation, request.id],
        )
        .unwrap();

    finding.analysis_version = "revised-finding".into();
    finding.observations[0].train.as_mut().unwrap().target = "alternate".into();
    let before = store.snapshot().unwrap();
    let revised = accept(&mut store, &finding, &target);
    assert_eq!(revised.new_requests, 0);
    let page = store
        .tactical_observations(
            &dataset,
            &finding.report,
            &finding.analysis_version,
            None,
            10,
        )
        .unwrap();
    assert_eq!(page.observations[0].request_id, Some(request.id));
    assert_eq!(
        page.observations[0]
            .observation
            .train
            .as_ref()
            .unwrap()
            .target,
        target
    );
    let pending = store.pending_tactical_requests(&dataset).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, request.id);
    assert_eq!(pending[0].observation_id, decision_observation);
    let after = store.snapshot().unwrap();
    assert_eq!(after.catalog, before.catalog);
    assert_eq!(after.learning, before.learning);
}

/// The counters track actual SQLite row writes, including no-op rewrites.
fn tactical_write_counts(connection: &Connection) -> BTreeMap<String, u64> {
    let mut query = connection
        .prepare("SELECT name, count(*) FROM tactical_write_probe GROUP BY name")
        .unwrap();
    query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn probe_tactical_writes(connection: &Connection) {
    connection
        .execute_batch("CREATE TEMP TABLE tactical_write_probe(name TEXT NOT NULL);")
        .unwrap();
    for table in [
        "content_history",
        "current_content",
        "learning",
        "attempts",
        "assessments",
        "receipts",
        "repertoire_lines",
        "line_targets",
        "games",
        "game_comparisons",
        "tactical_requests",
        "tactical_observations",
        "tactical_analyses",
    ] {
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            connection
                .execute_batch(&format!(
                "CREATE TEMP TRIGGER track_{table}_{operation} AFTER {operation} ON main.{table}
                 BEGIN INSERT INTO tactical_write_probe VALUES('{table}'); END;"
            ))
                .unwrap();
        }
    }
}

#[test]
fn tactical_acceptance_work_stays_addressed_as_unrelated_state_grows() {
    use rusqlite::StatementStatus;
    use tempo_chess::{Catalog, prepare_tactical_puzzle};

    let lookup_work = |db: &Connection, dataset: &DatasetId, lookup: &tempo_chess::TacticLookup| {
        let identity = encode(&lookup.identity).unwrap();
        let tokens = encode(&lookup.source_tokens).unwrap();
        let input = params![lookup.target.0, identity, tokens, dataset.0];
        let mut explain = db
            .prepare(&format!("EXPLAIN QUERY PLAN {}", tactical_targets::MATCHES))
            .unwrap();
        let plan: Vec<String> = explain
            .query_map(input, |row| row.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            plan.iter().any(|step| step
                .contains("SEARCH tactical_identities USING INDEX tactical_identities_exact")),
            "{plan:?}"
        );
        assert!(
            plan.iter()
                .any(|step| step.contains("SEARCH tactical_sources USING")
                    && step.contains("token=?")),
            "{plan:?}"
        );
        assert!(
            plan.iter()
                .any(|step| step.contains("SEARCH c USING") && step.contains("target_id=?")),
            "{plan:?}"
        );
        let mut query = db.prepare(tactical_targets::MATCHES).unwrap();
        let decoded: Vec<tempo_chess::ContentItem> = query
            .query_map(input, |row| row.get::<_, String>(0))
            .unwrap()
            .map(|row| decode(&row.unwrap()).unwrap())
            .collect();
        (
            decoded,
            query.get_status(StatementStatus::VmStep),
            query.get_status(StatementStatus::FullscanStep),
        )
    };

    let mut work = Vec::new();
    for (items, versions, attempts, assessments, receipts, games) in
        [(3, 4, 5, 6, 7, 8), (192, 227, 173, 219, 353, 151)]
    {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, exact) = fixture(&dir.path().join("scoped.sqlite"));
        let basis = store.tactical_acceptance_basis().unwrap();
        let mut catalog = store.content_state().unwrap().catalog;
        let unrelated = compile_tactics(&[TacticRecord {
            namespace: "growth".into(),
            source_id: "unrelated-line".into(),
            solver_fen: START_FEN.into(),
            solution: vec!["c2c4".into()],
            label: "Different solution at the same position".into(),
            provenance: vec![],
        }])
        .unwrap()
        .items
        .into_values()
        .next()
        .unwrap();
        let mut extra_learning = Vec::new();
        for index in 0..items {
            let mut item = unrelated.clone();
            item.target = TargetId(format!("unrelated-{index}"));
            item.version = ContentVersion(1000);
            extra_learning.push(Write::Learning {
                target: item.target.clone(),
                state: Default::default(),
            });
            catalog.items.insert(item.target.clone(), item);
        }
        let mut writes = vec![Write::Catalog(catalog)];
        writes.extend(extra_learning);
        store
            .commit(Commit {
                expected_dataset: basis.dataset.clone(),
                expected_revision: basis.revision,
                writes,
            })
            .unwrap();
        game(&store, "focus");
        for index in 0..games {
            game(&store, &format!("unrelated-game-{index}"));
        }
        // A deliberately undecodable payload is a read probe: materializing any
        // unrelated record from these tables would fail acceptance.
        let high = ContentVersion(1000).0.to_be_bytes();
        let tx = store.connection.transaction().unwrap();
        for version in 1..=versions {
            tx.execute(
                "INSERT INTO content_history(dataset_id,target_id,version,item)
                 VALUES('dataset','unrelated-0',?1,'{}')",
                [(version as u64).to_be_bytes().as_slice()],
            )
            .unwrap();
        }
        tx.execute_batch(
            "UPDATE current_content SET item='{}' WHERE target_id LIKE 'unrelated-%';
             UPDATE learning SET state='{}' WHERE target_id LIKE 'unrelated-%';",
        )
        .unwrap();
        for index in 0..attempts {
            tx.execute(
                "INSERT INTO attempts VALUES(?1,'dataset','unrelated-0',?2,'{}')",
                params![format!("unrelated-attempt-{index}"), high.as_slice()],
            )
            .unwrap();
        }
        for index in 0..assessments {
            tx.execute(
                "INSERT INTO assessments VALUES(NULL,'dataset','unrelated-0',?1,NULL,'fixture',?2,'{}')",
                params![high.as_slice(), format!("unrelated-assessment-{index}")]
            ).unwrap();
        }
        for index in 0..receipts {
            tx.execute(
                "INSERT INTO receipts VALUES(?1,NULL,'{}','{}')",
                [format!("unrelated-receipt-{index}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        probe_tactical_writes(&store.connection);

        let basis = store.tactical_acceptance_basis().unwrap();
        let new = prepare_tactical_puzzle(START_FEN, &"d2d4".into(), &[]).unwrap();
        let (decoded, new_steps, new_scans) =
            lookup_work(&store.connection, &basis.dataset, &new.lookup());
        assert!(decoded.is_empty());
        let items = store
            .tactical_content_matches(&basis.dataset, &new.lookup())
            .unwrap();
        assert!(items.is_empty());
        let plan = new.plan(&Catalog::default()).unwrap();
        let target = plan.target;
        let item = plan.content.catalog.items[&target].clone();
        let introduction = DailyLearning::introduce_capture(
            &Default::default(),
            StudyTime {
                day: StudyDay(10),
                unix_ms: 1_000,
            },
        )
        .unwrap();
        let first = analysis("focus", Some(&target));
        let accepted = store
            .commit_tactical_analysis(
                TacticalCommit {
                    expected_dataset: basis.dataset.clone(),
                    expected_revision: basis.revision,
                    targets: [(
                        target.clone(),
                        TacticalTargetChange {
                            content: Some(item),
                            introduction: Some(introduction),
                        },
                    )]
                    .into(),
                },
                &first,
            )
            .unwrap();
        assert_eq!((accepted.new_observations, accepted.new_requests), (1, 1));
        assert_eq!(
            tactical_write_counts(&store.connection),
            [
                ("content_history".into(), 1),
                ("current_content".into(), 1),
                ("learning".into(), 1),
                ("tactical_analyses".into(), 1),
                ("tactical_observations".into(), 1),
                ("tactical_requests".into(), 1),
            ]
            .into()
        );

        store
            .connection
            .execute_batch("DELETE FROM tactical_write_probe;")
            .unwrap();
        let revised = prepare_tactical_puzzle(START_FEN, &"e2e4".into(), &[]).unwrap();
        let (decoded, match_steps, match_scans) =
            lookup_work(&store.connection, &basis.dataset, &revised.lookup());
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].target, exact);
        work.push((new_steps, new_scans, match_steps, match_scans));
        let mut second = analysis("focus", Some(&exact));
        second.analysis_version = "revision".into();
        let report_epoch = store.tactical_report_generation(&basis.dataset).unwrap();
        let commit = TacticalCommit {
            expected_dataset: basis.dataset.clone(),
            expected_revision: accepted.revision,
            targets: BTreeMap::new(),
        };
        let amended = store
            .commit_tactical_analysis(commit.clone(), &second)
            .unwrap();
        assert_eq!((amended.new_observations, amended.new_requests), (0, 0));
        assert_eq!(
            store.pending_tactical_requests(&basis.dataset).unwrap()[0].target,
            target
        );
        assert_eq!(
            tactical_write_counts(&store.connection),
            [
                ("tactical_analyses".into(), 1),
                ("tactical_observations".into(), 2),
            ]
            .into()
        );
        let amended_epoch = store.tactical_report_generation(&basis.dataset).unwrap();
        assert!(amended_epoch.evidence > report_epoch.evidence);
        assert_eq!(amended_epoch.archive, report_epoch.archive);

        store
            .connection
            .execute_batch("DELETE FROM tactical_write_probe;")
            .unwrap();
        assert!(
            store
                .commit_tactical_analysis(commit.clone(), &second)
                .unwrap()
                .replayed
        );
        assert!(tactical_write_counts(&store.connection).is_empty());
        second.analysis_version = "stale".into();
        assert!(matches!(
            store.commit_tactical_analysis(commit, &second),
            Err(StoreError::Conflict(_))
        ));
        assert!(tactical_write_counts(&store.connection).is_empty());
        assert_eq!(
            store.tactical_report_generation(&basis.dataset).unwrap(),
            amended_epoch
        );
    }
    assert_eq!(work[0], work[1]);
    println!("Lookup VM steps/full-scan steps at both sizes: {work:?}");
}

#[test]
fn tactical_schema_15_backfills_lookup_and_preserves_schema_14_history() {
    use tempo_chess::prepare_tactical_puzzle;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schema-14.sqlite");
    let (mut store, target) = fixture(&path);
    game(&store, "before");
    let first = analysis("before", Some(&target));
    accept(&mut store, &first, &target);
    let before = store.snapshot().unwrap();
    let request = store
        .pending_tactical_requests(&before.dataset)
        .unwrap()
        .remove(0);
    let issued = issue_request(&mut store, &request, "pinned-at-migration");
    let before = store.snapshot().unwrap();

    remove_v15_projection(&store);
    drop(store);
    let mut store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.snapshot().unwrap(), before);
    let lookup = prepare_tactical_puzzle(START_FEN, &"e2e4".into(), &[])
        .unwrap()
        .lookup();
    let matched = store
        .tactical_content_matches(&before.dataset, &lookup)
        .unwrap();
    assert_eq!(matched, vec![before.catalog.items[&target].clone()]);
    assert!(
        store
            .has_pending_tactical_request_for_attempt(&before.dataset, &issued.attempt.id)
            .unwrap()
    );
    assert!(accept(&mut store, &first, &target).replayed);
    assert_eq!(store.snapshot().unwrap(), before);
    assert_eq!(
        store
            .connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, usize>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn payoff_waits_for_issued_work_and_publishes_one_version_atomically() {
    use crate::TacticPayoffPublication::{Deferred, Obsolete, Published};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("payoff.sqlite");
    let fen = "r2qk1nr/ppp3pp/2p1p3/2b5/4P3/8/PPPP1PPP/RNBQK2R w KQkq - 0 1";
    let plan =
        tempo_chess::plan_tactical_puzzle(&Default::default(), fen, &"d1h5".into(), &[]).unwrap();
    let target = plan.target;
    let prior = plan.content.catalog.items[&target].clone();
    let pv = ["d1h5", "g7g6", "h5c5", "g8f6"].map(str::to_owned);
    let qualification = tempo_chess::requalify_game_tactic(&prior, &pv).unwrap();
    let tempo_chess::GameTacticQualification::Revised(extension) = &qualification else {
        panic!("expected a verified fork");
    };
    let dataset: DatasetId = "dataset".into();
    let mut store = SqliteStore::create(
        &path,
        dataset.clone(),
        plan.content.catalog,
        DailyLearning::default().settings(),
    )
    .unwrap();
    game(&store, "one");
    accept(&mut store, &analysis("one", Some(&target)), &target);
    let request = store.pending_tactical_requests(&dataset).unwrap().remove(0);
    let issued = issue_request(&mut store, &request, "old-version");
    game(&store, "two");
    accept(&mut store, &analysis("two", Some(&target)), &target);
    // Start from the previously installed schema with both an active attempt
    // and another request waiting for its first issuance.
    store
        .connection
        .execute_batch(
            "DROP VIEW tactical_report_requests;
         DROP VIEW withheld_game_tactics;
         DROP TABLE tactical_qualifications;",
        )
        .unwrap();
    store
        .connection
        .execute_batch(
            include_str!("../study_exclusions.sql")
                .split_once("CREATE VIEW tactical_report_requests")
                .map(|(_, view)| {
                    format!("CREATE VIEW tactical_report_requests{view} DROP TABLE repertoire_coverage_start; PRAGMA user_version=18;")
                })
                .unwrap()
                .as_str(),
        )
        .unwrap();
    drop(store);
    let mut store = SqliteStore::open(&path).unwrap();
    assert!(
        store
            .pending_tactical_requests(&dataset)
            .unwrap()
            .is_empty()
    );
    let publish = |s: &SqliteStore| {
        s.publish_game_tactic_qualification(&dataset, &prior, qualification.clone())
    };
    assert_eq!(publish(&store).unwrap(), Deferred);
    assert!(
        store
            .game_tactic_qualification_held(&dataset, &prior)
            .unwrap()
    );
    let result = tempo_study::submit(
        &ExactSequenceGrader,
        &prior.definition,
        &issued.attempt,
        Revision(0),
        &"d1h5".into(),
    )
    .unwrap();
    assert_eq!(result.assessment, Some(tempo_types::Grade::Good));
    let receipt = Receipt {
        id: "finished".into(),
        command: "old answer".into(),
        result: ReceiptResult::Study(AnswerResult {
            attempt: result.attempt.clone(),
            feedback: Some(result.feedback),
            assessment: None,
            learning_effect: None,
        }),
    };
    store
        .commit_study(
            Commit {
                expected_dataset: dataset.clone(),
                expected_revision: store.snapshot().unwrap().revision,
                writes: vec![
                    Write::Progress(StoredAttempt {
                        attempt: result.attempt,
                        ..issued.clone()
                    }),
                    Write::Active(None),
                    Write::Receipt(receipt.clone()),
                ],
            },
            StudyScope {
                attempt: Some(&issued.attempt.id),
                submission: Some(&receipt.id),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        !store
            .game_tactic_qualification_held(&dataset, &prior)
            .unwrap()
    );
    let before = store.snapshot().unwrap();
    store.connection.execute_batch("CREATE TRIGGER fail_payoff BEFORE UPDATE ON current_content BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(publish(&store).is_err());
    assert_eq!(store.snapshot().unwrap(), before);
    store
        .connection
        .execute_batch("DROP TRIGGER fail_payoff")
        .unwrap();
    assert_eq!(publish(&store).unwrap(), Published);
    drop(store);
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(publish(&store).unwrap(), Obsolete);
    let after = store.snapshot().unwrap();
    assert_eq!(after.catalog.items[&target], *extension.replacement());
    let queued = store.pending_tactical_requests(&dataset).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].version, extension.replacement().version);
    assert_eq!(after.learning, before.learning);
    assert_eq!(after.receipts, before.receipts);
    assert_eq!(after.attempts, before.attempts);
    assert_eq!(
        store
            .issued_item(&dataset, &target, prior.version)
            .unwrap()
            .unwrap(),
        prior
    );
}
