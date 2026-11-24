use super::*;
use crate::{Commit, Store, Write};
use rusqlite::hooks::{Action, AuthAction};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tempo_chess::demo_catalog;
use tempo_learning::{DailyLearning, LearningPolicy};

fn create(directory: &tempfile::TempDir) -> (SqliteStore, DatasetId) {
    let dataset = DatasetId::from("plan-fixture");
    let store = SqliteStore::create(
        directory.path().join("study.sqlite"),
        dataset.clone(),
        demo_catalog(),
        DailyLearning::default().settings(),
    )
    .unwrap();
    (store, dataset)
}

fn entry(day: StudyDay, ordinal: u64, priority: PlanPriority) -> PlanEntry {
    let kind = match priority {
        PlanPriority::Correction => WorkKind::Correction,
        PlanPriority::DueTest(_) => WorkKind::Test,
        PlanPriority::Teach => WorkKind::Teach,
    };
    PlanEntry {
        work: WorkToken {
            day,
            obligation: WorkObligation {
                target: TargetId(format!("target-{ordinal:03}")),
                version: ContentVersion(1),
                kind,
                revision: MissRevision(ordinal),
            },
        },
        priority,
        ordinal,
        state: PlanEntryState::Ready,
    }
}

fn watch_writes(store: &SqliteStore) -> Arc<AtomicUsize> {
    let count = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&count);
    store
        .connection
        .update_hook(Some(move |_: Action, _: &str, table: &str, _: i64| {
            if matches!(
                table,
                "study_plan_entries" | "study_plan_generations" | "study_projection_state"
            ) {
                observed.fetch_add(1, Ordering::Relaxed);
            }
        }));
    count
}

fn query_plan(store: &SqliteStore, statement: &str, generation: PlanGeneration) -> Vec<String> {
    store
        .connection
        .prepare(&format!("EXPLAIN QUERY PLAN {statement}"))
        .unwrap()
        .query_map(params![sqlite_count(generation.0).unwrap(), 32], |row| {
            row.get(3)
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn plan_generation_storage_restarts_unservable_then_publishes_in_bounded_index_order() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, dataset) = create(&directory);
    assert!(store.study_projection_basis(&dataset).unwrap().is_none());
    let day = StudyDay(10);
    let basis = store
        .observe_study_day(&dataset, day)
        .unwrap()
        .unwrap()
        .plan;
    let mut entries = (0..130)
        .map(|ordinal| entry(day, ordinal, PlanPriority::Teach))
        .collect::<Vec<_>>();
    entries[1] = entry(day, 1, PlanPriority::DueTest(StudyDay(9)));
    entries[2] = entry(day, 2, PlanPriority::DueTest(StudyDay(8)));
    entries[3] = entry(day, 3, PlanPriority::Correction);
    entries[4].state = PlanEntryState::Deferred;
    let generation = store.begin_study_plan(&basis, 130).unwrap().unwrap();
    let written = watch_writes(&store);

    assert!(
        store
            .stage_study_plan(generation, &basis, 0, &entries[..128])
            .is_err()
    );
    assert_eq!(written.swap(0, Ordering::Relaxed), 0);
    assert_eq!(
        store
            .stage_study_plan(generation, &basis, 0, &entries[..127])
            .unwrap(),
        PlanStageResult::Staged { next_ordinal: 127 },
    );
    assert_eq!(written.swap(0, Ordering::Relaxed), 128);
    assert_eq!(
        store
            .stage_study_plan(generation, &basis, 0, &entries[..127])
            .unwrap(),
        PlanStageResult::CursorMismatch { next_ordinal: 127 },
    );
    assert_eq!(written.swap(0, Ordering::Relaxed), 0);
    assert_eq!(
        store.publish_study_plan(generation, &basis, day).unwrap(),
        PlanPublishResult::Incomplete
    );
    assert!(store.study_plan_head(&dataset).unwrap().is_none());
    assert!(
        store
            .ready_study_plan_entries(&dataset, generation, day)
            .unwrap()
            .is_none()
    );

    drop(store);
    let mut store = SqliteStore::open(directory.path().join("study.sqlite")).unwrap();
    assert!(store.study_plan_head(&dataset).unwrap().is_none());
    assert_eq!(
        store
            .stage_study_plan(generation, &basis, 127, &entries[127..])
            .unwrap(),
        PlanStageResult::Staged { next_ordinal: 130 },
    );
    let written = watch_writes(&store);
    let (publication, access) = super::super::operation_probe::probe_access(
        &mut store,
        |action| match action {
            AuthAction::Read { table_name, .. } => !matches!(
                table_name,
                "study_projection_state" | "study_plan_generations" | "store_state" | "datasets"
            ),
            AuthAction::Update { table_name, .. } => !matches!(
                table_name,
                "study_projection_state" | "study_plan_generations"
            ),
            _ => true,
        },
        |store| store.publish_study_plan(generation, &basis, day),
    );
    assert_eq!(
        publication.unwrap(),
        PlanPublishResult::Published,
        "{access:?}"
    );
    assert!(access.denied.is_empty(), "{access:?}");
    assert_eq!(written.swap(0, Ordering::Relaxed), 2);
    assert_eq!(
        store.publish_study_plan(generation, &basis, day).unwrap(),
        PlanPublishResult::AlreadyPublished
    );
    assert_eq!(written.swap(0, Ordering::Relaxed), 0);

    drop(store);
    let store = SqliteStore::open(directory.path().join("study.sqlite")).unwrap();
    assert_eq!(
        store.study_plan_head(&dataset).unwrap().unwrap(),
        PlanHead { generation, basis },
    );
    let ready = store
        .ready_study_plan_entries(&dataset, generation, day)
        .unwrap()
        .unwrap();
    let plan = query_plan(&store, READY_PLAN_SQL, generation);
    assert!(
        plan.iter()
            .any(|step| step.contains("INDEX study_plan_entries_ready")
                && step.contains("generation=?")),
        "{plan:?}"
    );
    assert!(
        !plan.iter().any(|step| step.contains("TEMP B-TREE")),
        "{plan:?}"
    );
    assert_eq!(ready.len(), 32);
    assert_eq!(
        &ready[..4],
        &[
            entries[3].clone(),
            entries[2].clone(),
            entries[1].clone(),
            entries[0].clone()
        ]
    );
    assert!(!ready.contains(&entries[4]));
    assert_eq!(ready[4], entries[5]);
}

#[test]
fn plan_generation_storage_fences_rank_membership_and_day_against_other_writers() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, dataset) = create(&directory);
    let day = StudyDay(10);
    let basis = store.observe_study_day(&dataset, day).unwrap().unwrap();
    let older_stage = store.begin_study_plan(&basis.plan, 0).unwrap().unwrap();
    let head = store.begin_study_plan(&basis.plan, 0).unwrap().unwrap();
    assert_eq!(
        store.publish_study_plan(head, &basis.plan, day).unwrap(),
        PlanPublishResult::Published
    );
    assert_eq!(
        store
            .publish_study_plan(older_stage, &basis.plan, day)
            .unwrap(),
        PlanPublishResult::Stale,
    );
    let stale_rank = store.begin_study_plan(&basis.plan, 0).unwrap().unwrap();
    let mut writer = SqliteStore::open(directory.path().join("study.sqlite")).unwrap();
    writer
        .connection
        .execute(
            "UPDATE study_projection_state SET rank_revision=?1 WHERE dataset_id=?2",
            params![1u64.to_be_bytes().as_slice(), dataset.0],
        )
        .unwrap();
    let ranked = store.study_projection_basis(&dataset).unwrap().unwrap();
    assert_eq!(ranked.summary, basis.summary);
    assert_eq!(ranked.summary_dirty, basis.summary_dirty);
    assert_eq!(
        store
            .publish_study_plan(stale_rank, &basis.plan, day)
            .unwrap(),
        PlanPublishResult::Stale
    );
    assert_eq!(
        store.ready_study_plan_entries(&dataset, head, day).unwrap(),
        Some(vec![])
    );

    let stale_membership = store.begin_study_plan(&ranked.plan, 0).unwrap().unwrap();
    writer
        .connection
        .execute(
            "UPDATE study_projection_state SET membership_epoch=?1,summary_epoch=?1,summary_dirty=1
         WHERE dataset_id=?2",
            params![1u64.to_be_bytes().as_slice(), dataset.0],
        )
        .unwrap();
    assert_eq!(
        store
            .publish_study_plan(stale_membership, &ranked.plan, day)
            .unwrap(),
        PlanPublishResult::Stale,
    );
    assert!(
        store
            .ready_study_plan_entries(&dataset, head, day)
            .unwrap()
            .is_none()
    );

    let current = store.study_projection_basis(&dataset).unwrap().unwrap();
    let stale_day = store.begin_study_plan(&current.plan, 0).unwrap().unwrap();
    let tomorrow = StudyDay(11);
    assert_eq!(
        store
            .publish_study_plan(stale_day, &current.plan, tomorrow)
            .unwrap(),
        PlanPublishResult::Stale,
    );
    let rolled = writer
        .observe_study_day(&dataset, tomorrow)
        .unwrap()
        .unwrap();
    assert_eq!(
        rolled.plan.membership_epoch.0,
        current.plan.membership_epoch.0 + 1
    );
    assert_eq!(rolled.plan.rank_revision, current.plan.rank_revision);
    assert_eq!(rolled.summary.epoch.0, current.summary.epoch.0 + 1);
    assert!(rolled.summary_dirty);
    assert_eq!(
        store
            .publish_study_plan(stale_day, &current.plan, day)
            .unwrap(),
        PlanPublishResult::Stale
    );
}

#[test]
fn plan_generation_storage_rolls_back_stage_and_head_on_late_sqlite_failure() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, dataset) = create(&directory);
    let day = StudyDay(10);
    let basis = store
        .observe_study_day(&dataset, day)
        .unwrap()
        .unwrap()
        .plan;
    let old = store.begin_study_plan(&basis, 0).unwrap().unwrap();
    store.publish_study_plan(old, &basis, day).unwrap();
    let newer = store.begin_study_plan(&basis, 3).unwrap().unwrap();
    let entries = (0..3)
        .map(|ordinal| entry(day, ordinal, PlanPriority::Teach))
        .collect::<Vec<_>>();
    store
        .connection
        .execute_batch(
            "CREATE TEMP TRIGGER abort_middle_stage BEFORE INSERT ON study_plan_entries
         WHEN new.target_id='target-001' BEGIN SELECT RAISE(ABORT,'late stage failure'); END;",
        )
        .unwrap();
    let written = watch_writes(&store);
    assert!(store.stage_study_plan(newer, &basis, 0, &entries).is_err());
    assert_eq!(written.swap(0, Ordering::Relaxed), 1);
    let rows: i64 = store
        .connection
        .query_row(
            "SELECT count(*) FROM study_plan_entries WHERE generation=?1",
            [sqlite_count(newer.0).unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
    assert_eq!(
        generation_header(&store.connection, &dataset, sqlite_count(newer.0).unwrap())
            .unwrap()
            .unwrap()
            .staged_entries,
        0
    );

    store
        .connection
        .execute_batch("DROP TRIGGER abort_middle_stage;")
        .unwrap();
    store.stage_study_plan(newer, &basis, 0, &entries).unwrap();
    store.connection.execute_batch(
        "CREATE TEMP TRIGGER abort_head_switch BEFORE UPDATE OF head_generation ON study_projection_state
         BEGIN SELECT RAISE(ABORT,'late publication failure'); END;",
    ).unwrap();
    assert!(store.publish_study_plan(newer, &basis, day).is_err());
    assert_eq!(
        store.study_plan_head(&dataset).unwrap().unwrap().generation,
        old
    );
    assert_eq!(
        generation_header(&store.connection, &dataset, sqlite_count(newer.0).unwrap())
            .unwrap()
            .unwrap()
            .phase,
        "staging"
    );
    store
        .connection
        .execute_batch("DROP TRIGGER abort_head_switch;")
        .unwrap();
    assert_eq!(
        store.publish_study_plan(newer, &basis, day).unwrap(),
        PlanPublishResult::Published
    );
}

#[test]
fn plan_generation_storage_reclaims_only_non_head_in_short_turns() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, dataset) = create(&directory);
    let protected = store.snapshot().unwrap();
    let day = StudyDay(10);
    let basis = store
        .observe_study_day(&dataset, day)
        .unwrap()
        .unwrap()
        .plan;
    let first = store.begin_study_plan(&basis, 128).unwrap().unwrap();
    let entries = (0..128)
        .map(|ordinal| entry(day, ordinal, PlanPriority::Teach))
        .collect::<Vec<_>>();
    store
        .stage_study_plan(first, &basis, 0, &entries[..127])
        .unwrap();
    store
        .stage_study_plan(first, &basis, 127, &entries[127..])
        .unwrap();
    store.publish_study_plan(first, &basis, day).unwrap();
    let plan = query_plan(&store, RECLAIM_PLAN_SQL, first);
    assert!(
        plan.iter()
            .any(|step| step.contains("INDEX") && step.contains("generation=?")),
        "{plan:?}"
    );
    assert!(
        !plan.iter().any(|step| step.contains("TEMP B-TREE")),
        "{plan:?}"
    );
    let written = watch_writes(&store);
    assert_eq!(
        store.reclaim_study_plan(&dataset, first).unwrap(),
        PlanReclaimResult::Protected
    );
    assert_eq!(written.swap(0, Ordering::Relaxed), 0);
    let replacement = store.begin_study_plan(&basis, 0).unwrap().unwrap();
    store.publish_study_plan(replacement, &basis, day).unwrap();
    written.store(0, Ordering::Relaxed);

    assert_eq!(
        store.reclaim_study_plan(&dataset, first).unwrap(),
        PlanReclaimResult::Pending
    );
    assert_eq!(written.swap(0, Ordering::Relaxed), 128);
    assert_eq!(
        store.reclaim_study_plan(&dataset, first).unwrap(),
        PlanReclaimResult::Pending
    );
    assert_eq!(written.swap(0, Ordering::Relaxed), 1);
    assert_eq!(
        store.reclaim_study_plan(&dataset, first).unwrap(),
        PlanReclaimResult::Reclaimed
    );
    assert_eq!(written.swap(0, Ordering::Relaxed), 1);
    assert_eq!(
        store.reclaim_study_plan(&dataset, first).unwrap(),
        PlanReclaimResult::Missing
    );
    assert_eq!(
        store.study_plan_head(&dataset).unwrap().unwrap().generation,
        replacement
    );
    assert_eq!(store.snapshot().unwrap(), protected);
}

#[test]
fn plan_generation_storage_schema24_keeps_previous_schema_authority() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, dataset) = create(&directory);
    let before = store.snapshot().unwrap();
    store
        .commit(Commit {
            expected_dataset: dataset.clone(),
            expected_revision: before.revision,
            writes: vec![
                Write::Issue(crate::test_support::issued_record(&before)),
                Write::Active(Some("attempt-1".into())),
            ],
        })
        .unwrap();
    let issued = store.snapshot().unwrap();
    remove_schema_for_prior_version_fixture(&store.connection);
    store
        .connection
        .pragma_update(None, "user_version", 23)
        .unwrap();
    drop(store);

    let store = SqliteStore::open(directory.path().join("study.sqlite")).unwrap();
    assert_eq!(
        super::super::schema_version(&store.connection).unwrap(),
        super::super::SCHEMA_VERSION
    );
    assert_eq!(store.snapshot().unwrap(), issued);
    assert!(store.study_plan_head(&dataset).unwrap().is_none());
    assert!(store.study_projection_basis(&dataset).unwrap().is_none());
    let invalid: i64 = store
        .connection
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(invalid, 0);
}

#[test]
fn calendar_observation_fences_mismatched_profiles_and_rollover() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("calendar.sqlite");
    let dataset: DatasetId = "calendar".into();
    let before = r#"{"study_timezone":"America/New_York"}"#;
    let after = r#"{"study_timezone":"Europe/London"}"#;
    let mut store = SqliteStore::create_with_profile(
        &path,
        dataset.clone(),
        demo_catalog(),
        DailyLearning::default().settings(),
        Some(before),
    )
    .unwrap();
    let observed = store
        .observe_study_calendar_day(&dataset, "America/New_York", StudyDay(20))
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .observe_study_calendar_day(&dataset, "America/New_York", StudyDay(20))
            .unwrap(),
        Some(observed),
    );

    let settings = SqliteStore::open(&path).unwrap();
    settings
        .update_study_profile_for_plan(
            &dataset,
            before,
            after,
            StudyProfilePlanEffect::InvalidateMembership,
        )
        .unwrap();
    let fenced = store.study_projection_basis(&dataset).unwrap();
    assert_eq!(
        store
            .observe_study_calendar_day(&dataset, "America/New_York", StudyDay(21))
            .unwrap(),
        None,
    );
    assert_eq!(store.study_projection_basis(&dataset).unwrap(), fenced);

    let rolled = store
        .observe_study_calendar_day(&dataset, "Europe/London", StudyDay(21))
        .unwrap()
        .unwrap();
    assert_eq!(rolled.plan.day, StudyDay(21));
    assert_eq!(rolled.summary.day, StudyDay(21));
    let fenced = fenced.unwrap();
    assert!(rolled.plan.membership_epoch.0 > fenced.plan.membership_epoch.0);
    assert!(rolled.summary.epoch.0 > fenced.summary.epoch.0);
}
