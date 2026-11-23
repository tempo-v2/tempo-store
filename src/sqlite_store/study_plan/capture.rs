//! Read one original planning basis and its compact facts in the same snapshot.

use super::{StudyDayAuthority, StudyProjectionBasis, authority, projection_state};
use crate::sqlite_store::{
    SqliteStore, database_error,
    load_snapshot::load_lines,
    row_values::{decode, read_selected},
    study_content::{STUDY_CONTENT_FACT_COLUMNS, read_content_facts},
};
use crate::{StoreError, StudyContentFacts, store_error::invalid};
use rusqlite::{Transaction, TransactionBehavior};
use std::sync::LazyLock;
use tempo_chess::RepertoireLine;
use tempo_learning::{DailyAvailability, LearningState};
use tempo_types::{DatasetId, MissRevision};

/// Whether a coherent original basis is available for the batch builder.
///
/// The read exposes compact facts for verification while producer adoption
/// and issued-work authority are still pending.
#[derive(Debug, PartialEq, Eq)]
pub enum StudyPlanCapture {
    MissingBasis,
    /// The capture remains incomplete until the effective profile and all
    /// relevant producers and issued-work facts share this transaction basis.
    Incomplete(StudyPlanInputFacts),
}

#[derive(Debug, PartialEq, Eq)]
pub struct StudyPlanInputFacts {
    pub basis: StudyProjectionBasis,
    pub day_authority: Option<StudyDayAuthority>,
    pub expected_targets: usize,
    pub targets: Vec<StudyPlanTargetFacts>,
    /// Authored routes in durable order with no PGN or playable definitions.
    pub routes: Vec<RepertoireLine>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StudyPlanTargetFacts {
    pub content: StudyContentFacts,
    pub availability: DailyAvailability,
    pub miss_revision: MissRevision,
}

impl SqliteStore {
    /// Read the original basis and current content, Learning and route facts.
    /// The read transaction is closed before returning any captured values.
    pub fn capture_study_plan(&self, dataset: &DatasetId) -> Result<StudyPlanCapture, StoreError> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
            .map_err(database_error)?;
        let capture = match projection_state(&tx, dataset)?.and_then(|state| state.basis(dataset)) {
            Some(basis) => StudyPlanCapture::Incomplete(capture_inputs(&tx, basis)?),
            None => StudyPlanCapture::MissingBasis,
        };
        tx.commit().map_err(database_error)?;
        Ok(capture)
    }
}

fn capture_inputs(
    tx: &Transaction<'_>,
    basis: StudyProjectionBasis,
) -> Result<StudyPlanInputFacts, StoreError> {
    static TARGET_SQL: LazyLock<String> = LazyLock::new(|| {
        format!(
            "SELECT {STUDY_CONTENT_FACT_COLUMNS}, l.state
             FROM current_content c LEFT JOIN learning l USING (target_id)
             WHERE c.dataset_id=?1 ORDER BY c.target_id"
        )
    });
    let dataset = &basis.plan.dataset.0;
    let day = basis.plan.day;
    let expected_targets: i64 = tx
        .query_row(
            "SELECT count(*) FROM current_content WHERE dataset_id=?1",
            [dataset],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    let expected_targets = usize::try_from(expected_targets)
        .map_err(|_| invalid("Study plan target count exceeds capacity"))?;
    let targets: Vec<StudyPlanTargetFacts> = read_selected(tx, &TARGET_SQL, [dataset], |row| {
        let content = read_content_facts(row)?;
        let record: Option<String> = row.get(8).map_err(database_error)?;
        let learning: LearningState =
            decode(&record.ok_or_else(|| invalid("current Study target has no Learning state"))?)?;
        Ok(StudyPlanTargetFacts {
            availability: learning.daily_availability(content.version, day),
            miss_revision: learning.miss_revision,
            content,
        })
    })?;
    if targets.len() != expected_targets {
        return Err(invalid("incomplete captured Study targets"));
    }
    Ok(StudyPlanInputFacts {
        day_authority: authority::read_day(tx, &basis.plan.dataset, basis.plan.day)?,
        basis,
        expected_targets,
        targets,
        routes: load_lines(tx)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_store::{operation_probe::probe_access, row_values::encode};
    use rusqlite::{
        Connection,
        hooks::{AuthAction, AuthContext, Authorization},
        params,
    };
    use std::{
        path::Path,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };
    use tempo_chess::{ContentStatus, demo_catalog};
    use tempo_learning::{DailyLearning, LearningPolicy};
    use tempo_types::{MembershipEpoch, StudyDay, StudyTime, TargetId};

    const DAY: StudyDay = StudyDay(10);

    fn fixture(path: &Path) -> (SqliteStore, DatasetId) {
        let dataset: DatasetId = "capture-dataset".into();
        let store = SqliteStore::create(
            path,
            dataset.clone(),
            demo_catalog(),
            DailyLearning::default().settings(),
        )
        .unwrap();
        (store, dataset)
    }

    fn incomplete(capture: StudyPlanCapture) -> StudyPlanInputFacts {
        let StudyPlanCapture::Incomplete(facts) = capture else {
            panic!("expected an observed basis with incomplete day authority");
        };
        facts
    }

    #[test]
    fn plan_capture_is_compact_read_only_and_explicitly_incomplete() {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, dataset) = fixture(&directory.path().join("study.sqlite"));
        assert_eq!(
            store.capture_study_plan(&dataset).unwrap(),
            StudyPlanCapture::MissingBasis
        );
        let basis = store.observe_study_day(&dataset, DAY).unwrap().unwrap();
        store
            .connection
            .execute_batch(
                "UPDATE current_content SET item=json_set(item,'$.definition',NULL);
             UPDATE content_history SET item='unreadable historical definition';",
            )
            .unwrap();

        let (result, access) = probe_access(
            &mut store,
            |action| match action {
                AuthAction::Read { table_name, .. } => !matches!(
                    table_name,
                    "store_state"
                        | "study_projection_state"
                        | "study_day_authority"
                        | "current_content"
                        | "learning"
                        | "repertoire_lines"
                        | "line_targets"
                        | "repertoire_suspensions"
                        | "study_exclusions"
                        | "tactical_qualifications"
                ),
                AuthAction::Insert { .. }
                | AuthAction::Update { .. }
                | AuthAction::Delete { .. } => true,
                _ => false,
            },
            |store| store.capture_study_plan(&dataset),
        );

        let captured = incomplete(result.unwrap());
        assert!(access.denied.is_empty(), "{:?}", access.denied);
        assert_eq!(captured.basis, basis);
        assert_eq!(captured.expected_targets, 3);
        assert_eq!(captured.targets.len(), 3);
        assert_eq!(captured.routes.len(), 1);
        assert_eq!(
            captured.routes[0].targets,
            [TargetId::from("r1:p1"), TargetId::from("r1:p2")],
        );
        assert!(
            captured
                .targets
                .iter()
                .all(|row| !row.availability.introduced_this_version)
        );
    }

    #[test]
    fn plan_capture_does_not_treat_missing_learning_as_empty_work() {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, dataset) = fixture(&directory.path().join("study.sqlite"));
        store.observe_study_day(&dataset, DAY).unwrap();
        store
            .connection
            .execute("DELETE FROM learning WHERE target_id='t1'", [])
            .unwrap();

        assert_eq!(
            store.capture_study_plan(&dataset).unwrap_err(),
            invalid("current Study target has no Learning state"),
        );
    }

    #[test]
    fn plan_capture_cannot_bless_stale_facts_with_an_intervening_new_basis() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("study.sqlite");
        let (mut store, dataset) = fixture(&path);
        store
            .connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        let old_basis = store.observe_study_day(&dataset, DAY).unwrap().unwrap();
        let new_epoch = old_basis.plan.membership_epoch.0 + 1;
        let learned = encode(
            &DailyLearning::teach(
                &LearningState::default(),
                StudyTime {
                    day: DAY,
                    unix_ms: 864_000_000,
                },
            )
            .unwrap(),
        )
        .unwrap();
        let (start_write, write_requested) = mpsc::sync_channel(1);
        let (write_committed, resume_read) = mpsc::sync_channel(1);
        let capture_dataset = dataset.clone();
        let writer = thread::spawn(move || {
            let mut db = Connection::open(path).unwrap();
            write_requested
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            let tx = db.transaction().unwrap();
            tx.execute(
                "UPDATE study_projection_state
                 SET membership_epoch=?2, summary_epoch=?2, summary_dirty=1
                 WHERE dataset_id=?1",
                params![capture_dataset.0, new_epoch.to_be_bytes().as_slice()],
            )
            .unwrap();
            tx.execute(
                "UPDATE learning SET state=?1 WHERE target_id='t1'",
                [&learned],
            )
            .unwrap();
            tx.execute(
                "UPDATE current_content SET item=json_set(item,'$.status','retired') WHERE target_id='t1'",
                [],
            ).unwrap();
            tx.commit().unwrap();
            write_committed.send(()).unwrap();
        });

        // The first content preparation happens after the capture read its basis.
        // Commit a second connection here to exercise the actual read transaction.
        let triggered = AtomicBool::new(false);
        store.connection.flush_prepared_statement_cache();
        store
            .connection
            .authorizer(Some(move |context: AuthContext<'_>| {
                if matches!(
                    context.action,
                    AuthAction::Read {
                        table_name: "current_content",
                        ..
                    }
                ) && !triggered.swap(true, Ordering::SeqCst)
                    && (start_write.send(()).is_err()
                        || resume_read.recv_timeout(Duration::from_secs(5)).is_err())
                {
                    return Authorization::Deny;
                }
                Authorization::Allow
            }));
        let captured = store.capture_study_plan(&dataset);
        store
            .connection
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        store.connection.flush_prepared_statement_cache();
        writer.join().unwrap();

        let captured = incomplete(captured.unwrap());
        assert_eq!(captured.basis, old_basis);
        let prior = captured
            .targets
            .iter()
            .find(|row| row.content.target == TargetId::from("t1"))
            .unwrap();
        assert_eq!(prior.content.status, ContentStatus::Active);
        assert!(!prior.availability.introduced_this_version);
        assert_eq!(
            store.begin_study_plan(&captured.basis.plan, 0).unwrap(),
            None
        );

        let next = incomplete(store.capture_study_plan(&dataset).unwrap());
        assert_eq!(next.basis.plan.membership_epoch, MembershipEpoch(new_epoch));
        let committed = next
            .targets
            .iter()
            .find(|row| row.content.target == TargetId::from("t1"))
            .unwrap();
        assert_eq!(committed.content.status, ContentStatus::Retired);
        assert!(committed.availability.introduced_this_version);
    }
}
