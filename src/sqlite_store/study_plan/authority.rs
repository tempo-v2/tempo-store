//! Day-local Study authority and the transactional Settings fence.

use super::{next_epoch, projection_state, store_count};
use crate::{
    AcceptanceKind, ActivityMode, ReceiptResult, StoreError, StoredAttempt,
    sqlite_store::{
        SqliteStore, database_error,
        row_values::{decode, receipt_value},
    },
    store_error::conflict,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;
use std::collections::BTreeSet;
use tempo_learning::LearningState;
use tempo_study::AttemptStatus;
use tempo_types::{ContentVersion, DatasetId, StudyDay};

const SCHEMA: &str = include_str!("../study_day_authority.sql");

/// Authoritative usage and the Skip frontier for an observed dataset day.
/// An unproven legacy subtotal cannot be used to grant new introductions.
#[derive(Debug, PartialEq, Eq)]
pub struct StudyDayAuthority {
    pub opening_spent: u64,
    pub tactic_spent: u64,
    pub spend_proven: bool,
    pub latest_skip: u64,
    pub skip_cutoff: u64,
}

/// The Settings owner compares effective values before asking Store to save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StudyProfilePlanEffect {
    Preserve,
    InvalidateMembership,
}

pub(in crate::sqlite_store) fn initialize_authority(
    tx: &Transaction<'_>,
) -> Result<(), StoreError> {
    tx.execute_batch(SCHEMA).map_err(database_error)
}

pub(super) fn ensure_day(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    day: StudyDay,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO study_day_authority(dataset_id,study_day) VALUES(?1,?2)
         ON CONFLICT(dataset_id,study_day) DO NOTHING",
        params![dataset.0, day.0],
    )
    .map_err(database_error)?;
    Ok(())
}

pub(super) fn read_day(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    day: StudyDay,
) -> Result<Option<StudyDayAuthority>, StoreError> {
    tx.query_row(
        "SELECT opening_spent,tactic_spent,spend_proven,latest_skip,skip_cutoff
         FROM study_day_authority WHERE dataset_id=?1 AND study_day=?2",
        params![dataset.0, day.0],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )
    .optional()
    .map_err(database_error)?
    .map(|(opening, tactic, spend_proven, latest, cutoff)| {
        Ok(StudyDayAuthority {
            opening_spent: store_count(opening)?,
            tactic_spent: store_count(tactic)?,
            spend_proven,
            latest_skip: store_count(latest)?,
            skip_cutoff: store_count(cutoff)?,
        })
    })
    .transpose()
}

impl SqliteStore {
    /// Compare and replace the committed Settings document for the named dataset.
    /// The effective-policy change and its planning fence commit together.
    pub fn update_study_profile_for_plan(
        &self,
        dataset: &DatasetId,
        expected: &str,
        replacement: &str,
        effect: StudyProfilePlanEffect,
    ) -> Result<(), StoreError> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let changed = tx
            .execute(
                "UPDATE store_state SET local_profile=?3
                 WHERE singleton=1 AND dataset_id=?1 AND local_profile=?2",
                params![dataset.0, expected, replacement],
            )
            .map_err(database_error)?;
        if changed != 1 {
            return Err(conflict("study settings changed; reload before retrying"));
        }
        if effect == StudyProfilePlanEffect::InvalidateMembership {
            invalidate_membership(&tx, dataset)?;
        }
        tx.commit().map_err(database_error)
    }
}

fn invalidate_membership(tx: &Transaction<'_>, dataset: &DatasetId) -> Result<(), StoreError> {
    let state = projection_state(tx, dataset)?
        .ok_or_else(|| conflict("Study projection dataset is no longer current"))?;
    let membership = next_epoch(state.membership_epoch.0)?.to_be_bytes();
    let summary = next_epoch(state.summary_epoch.0)?.to_be_bytes();
    tx.execute(
        "UPDATE study_projection_state
         SET membership_epoch=?2,summary_epoch=?3,summary_dirty=1 WHERE dataset_id=?1",
        params![dataset.0, membership.as_slice(), summary.as_slice()],
    )
    .map_err(database_error)?;
    Ok(())
}

pub(in crate::sqlite_store) fn migrate_authority(tx: &Transaction<'_>) -> Result<(), StoreError> {
    initialize_authority(tx)?;
    let dataset = DatasetId(
        tx.query_row(
            "SELECT dataset_id FROM store_state WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .map_err(database_error)?,
    );
    if let Some(day) = projection_state(tx, &dataset)?.and_then(|state| state.day) {
        ensure_day(tx, &dataset, day)?;
    }

    // Do not charge issued lessons, Repair, assessments or imported Learning.
    // A dated first-Teach record with insufficient evidence marks its day unproven.
    let mut statement = tx
        .prepare(
            "SELECT a.id,a.record,c.target_id,c.repertoire,l.state,
                json_extract(a.record,'$.acceptance.at.day'),a.version
         FROM attempts a
         LEFT JOIN current_content c ON (c.dataset_id,c.target_id,c.version) =
                                        (a.dataset_id,a.target_id,a.version)
         LEFT JOIN learning l ON l.target_id=c.target_id
         WHERE a.dataset_id=?1 AND CASE WHEN json_valid(a.record) THEN
             json_extract(a.record,'$.mode')='teach'
             AND json_extract(a.record,'$.first_introduction')=1
             AND json_extract(a.record,'$.acceptance.at.day') BETWEEN -2147483648 AND 2147483647
             AND json_type(a.record,'$.acceptance.at.day')='integer'
             ELSE 0 END",
        )
        .map_err(database_error)?;
    let mut rows = statement.query([&dataset.0]).map_err(database_error)?;
    let mut proven_targets = BTreeSet::new();
    while let Some(row) = rows.next().map_err(database_error)? {
        let day = StudyDay(row.get(5).map_err(database_error)?);
        ensure_day(tx, &dataset, day)?;
        let attempt_id: String = row.get(0).map_err(database_error)?;
        let record: String = row.get(1).map_err(database_error)?;
        let current_target: Option<String> = row.get(2).map_err(database_error)?;
        let repertoire: Option<String> = row.get(3).map_err(database_error)?;
        let learning: Option<String> = row.get(4).map_err(database_error)?;

        let version: Vec<u8> = row.get(6).map_err(database_error)?;
        let proven = if let (Some(target), Some(learning)) = (current_target, learning) {
            let version = ContentVersion(super::super::row_values::decode_counter(version)?);
            prove_first_teach(
                tx,
                &dataset,
                day,
                &attempt_id,
                &target,
                version,
                &record,
                &learning,
            )? && proven_targets.insert((day.0, target))
        } else {
            false
        };
        let (opening, tactic) = if proven {
            (
                i64::from(repertoire.is_some()),
                i64::from(repertoire.is_none()),
            )
        } else {
            (0, 0)
        };
        tx.execute(
            "UPDATE study_day_authority
             SET opening_spent=opening_spent+?3,tactic_spent=tactic_spent+?4,
                 spend_proven=spend_proven AND ?5 WHERE dataset_id=?1 AND study_day=?2",
            params![dataset.0, day.0, opening, tactic, proven],
        )
        .map_err(database_error)?;
    }
    // Prior generations were composed without these authority records.
    invalidate_membership(tx, &dataset)
}

fn prove_first_teach(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    day: StudyDay,
    attempt_id: &str,
    target: &str,
    version: ContentVersion,
    record: &str,
    learning: &str,
) -> Result<bool, StoreError> {
    let (Ok(record), Ok(learning)) = (
        decode::<StoredAttempt>(record),
        decode::<LearningState>(learning),
    ) else {
        return Ok(false);
    };
    let Some(acceptance) = record.acceptance else {
        return Ok(false);
    };
    if record.mode != ActivityMode::Teach
        || !record.first_introduction
        || record.attempt.status != AttemptStatus::Complete
        || record.attempt.id.0 != attempt_id
        || &record.attempt.dataset != dataset
        || record.attempt.content_version != version
        || record.attempt.target.0 != target
        || acceptance.kind != AcceptanceKind::Teaching
        || acceptance.at.day != day
        || learning.first_introduced != Some(day)
        || learning.taught_at != Some(acceptance.at)
    {
        return Ok(false);
    }

    let mut statement = tx
        .prepare("SELECT id,command,result FROM receipts WHERE attempt_id=?1")
        .map_err(database_error)?;
    let mut rows = statement.query([attempt_id]).map_err(database_error)?;
    let mut proven = false;
    while let Some(row) = rows.next().map_err(database_error)? {
        let id: String = row.get(0).map_err(database_error)?;
        let command: String = row.get(1).map_err(database_error)?;
        let Ok((operation, payload)) = decode::<(String, Value)>(&command) else {
            continue;
        };
        if operation != "acknowledge"
            || payload["dataset_id"].as_str() != Some(dataset.0.as_str())
            || payload["attempt_id"].as_str() != Some(attempt_id)
            || payload["submission_id"].as_str() != Some(id.as_str())
            || payload["expected_revision"]
                .as_u64()
                .and_then(|revision| revision.checked_add(1))
                != Some(record.attempt.revision.0)
        {
            continue;
        }
        let Ok(ReceiptResult::Study(result)) = receipt_value(row, 2) else {
            return Ok(false);
        };
        if result.attempt == record.attempt
            && result.assessment.is_none()
            && result.learning_effect.is_none()
        {
            if proven {
                return Ok(false);
            }
            proven = true;
        }
    }
    Ok(proven)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActivityAcceptance, AnswerResult, Store,
        sqlite_store::{SCHEMA_VERSION, row_values::encode, schema_version},
        test_support::issued_record,
    };
    use serde_json::json;
    use std::path::Path;
    use tempo_chess::demo_catalog;
    use tempo_learning::{DailyLearning, LearningPolicy};
    use tempo_types::{Revision, StudyTime};

    fn create(path: &Path) -> (SqliteStore, DatasetId) {
        let dataset = DatasetId::from("authority-fixture");
        let store = SqliteStore::create_with_profile(
            path,
            dataset.clone(),
            demo_catalog(),
            DailyLearning::default().settings(),
            Some("original"),
        )
        .unwrap();
        (store, dataset)
    }

    #[test]
    fn profile_and_epochs_commit_together_for_current_dataset_and_expected_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("study.sqlite");
        let (mut store, dataset) = create(&path);
        let original = store
            .observe_study_day(&dataset, StudyDay(20))
            .unwrap()
            .unwrap();
        let other = SqliteStore::open(&path).unwrap();
        other
            .update_study_profile_for_plan(
                &dataset,
                "original",
                "same effective settings",
                StudyProfilePlanEffect::Preserve,
            )
            .unwrap();
        assert_eq!(
            store.study_projection_basis(&dataset).unwrap(),
            Some(original)
        );
        let before = store.study_projection_basis(&dataset).unwrap().unwrap();

        for (wrong_dataset, expected) in [
            (dataset.clone(), "original"),
            (DatasetId::from("retired"), "same effective settings"),
        ] {
            assert!(matches!(
                store.update_study_profile_for_plan(
                    &wrong_dataset,
                    expected,
                    "conflicting",
                    StudyProfilePlanEffect::InvalidateMembership,
                ),
                Err(StoreError::Conflict(_))
            ));
        }
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_membership BEFORE UPDATE OF membership_epoch
             ON study_projection_state BEGIN SELECT RAISE(ABORT,'injected epoch failure'); END;",
            )
            .unwrap();
        assert!(
            store
                .update_study_profile_for_plan(
                    &dataset,
                    "same effective settings",
                    "new limits",
                    StudyProfilePlanEffect::InvalidateMembership,
                )
                .is_err()
        );
        assert_eq!(
            other.local_profile().unwrap().as_deref(),
            Some("same effective settings")
        );
        assert_eq!(
            other.study_projection_basis(&dataset).unwrap().as_ref(),
            Some(&before)
        );

        store
            .connection
            .execute_batch("DROP TRIGGER reject_membership;")
            .unwrap();
        store
            .update_study_profile_for_plan(
                &dataset,
                "same effective settings",
                "new limits",
                StudyProfilePlanEffect::InvalidateMembership,
            )
            .unwrap();
        let after = other.study_projection_basis(&dataset).unwrap().unwrap();
        assert_eq!(
            other.local_profile().unwrap().as_deref(),
            Some("new limits")
        );
        assert_eq!(
            after.plan.membership_epoch.0,
            before.plan.membership_epoch.0 + 1
        );
        assert_eq!(after.plan.rank_revision, before.plan.rank_revision);
        assert_eq!(after.summary.epoch.0, before.summary.epoch.0 + 1);
        assert!(after.summary_dirty);
    }

    fn legacy_teach(store: &SqliteStore, dataset: &DatasetId, applied: bool) {
        let snapshot = store.snapshot().unwrap();
        let mut record = issued_record(&snapshot);
        let target = record.attempt.target.clone();
        let version = record.attempt.content_version.0.to_be_bytes();
        let accepted = StudyTime {
            day: StudyDay(11),
            unix_ms: 200,
        };
        record.mode = ActivityMode::Teach;
        record.first_introduction = true;
        record.attempt.revision = Revision(1);
        record.attempt.prompt = None;
        record.attempt.status = AttemptStatus::Complete;
        record.acceptance = Some(ActivityAcceptance {
            at: accepted,
            kind: AcceptanceKind::Teaching,
        });
        store
            .connection
            .execute(
                "INSERT INTO attempts VALUES(?1,?2,?3,?4,?5)",
                params![
                    record.attempt.id.0,
                    dataset.0,
                    target.0,
                    version.as_slice(),
                    encode(&record).unwrap()
                ],
            )
            .unwrap();
        let command = json!(["acknowledge", {
            "dataset_id":dataset, "attempt_id":record.attempt.id, "submission_id":"prior",
            "expected_revision":0, "answer":{"uci":"e2e4"}
        }]);
        let receipt = ReceiptResult::Study(AnswerResult {
            attempt: record.attempt,
            feedback: None,
            assessment: None,
            learning_effect: None,
        });
        store
            .connection
            .execute(
                "INSERT INTO receipts VALUES('prior','attempt-1',?1,?2)",
                // Direct legacy payload, independent of the new live writer.
                params![command.to_string(), encode(&receipt).unwrap()],
            )
            .unwrap();
        if applied {
            let mut learning = snapshot.learning[&target].clone();
            learning.first_introduced = Some(accepted.day);
            learning.taught_at = Some(accepted);
            store
                .connection
                .execute(
                    "UPDATE learning SET state=?2 WHERE target_id=?1",
                    params![target.0, encode(&learning).unwrap()],
                )
                .unwrap();
        }
    }

    #[test]
    fn migration_proves_only_applied_exact_first_teach_and_preserves_bytes_and_rollover() {
        for applied in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("study.sqlite");
            let (mut store, dataset) = create(&path);
            let accepted_day = StudyDay(11);
            let before = store
                .observe_study_day(&dataset, accepted_day)
                .unwrap()
                .unwrap();
            legacy_teach(&store, &dataset, applied);
            let receipt_bytes = |db: &SqliteStore| -> (String, String) {
                db.connection
                    .query_row(
                        "SELECT command,result FROM receipts WHERE id='prior'",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .unwrap()
            };
            let old = receipt_bytes(&store);
            store.connection.execute_batch(
                "DROP TABLE study_deferrals; DROP TABLE study_day_authority; PRAGMA user_version=24;",
            ).unwrap();
            drop(store);

            let mut store = SqliteStore::open(&path).unwrap();
            assert_eq!(schema_version(&store.connection).unwrap(), SCHEMA_VERSION);
            assert_eq!(receipt_bytes(&store), old);
            let after = store.study_projection_basis(&dataset).unwrap().unwrap();
            assert_eq!(
                after.plan.membership_epoch.0,
                before.plan.membership_epoch.0 + 1
            );
            let authority = |store: &SqliteStore| {
                let super::super::StudyPlanCapture::Incomplete(facts) =
                    store.capture_study_plan(&dataset).unwrap()
                else {
                    panic!("expected incomplete capture")
                };
                facts.day_authority.unwrap()
            };
            let migrated = authority(&store);
            assert_eq!(migrated.opening_spent, u64::from(applied));
            assert_eq!(migrated.tactic_spent, 0);
            assert_eq!(migrated.spend_proven, applied);
            store.observe_study_day(&dataset, StudyDay(10)).unwrap();
            let issue_day = authority(&store);
            assert_eq!(issue_day.opening_spent, 0);
            assert!(issue_day.spend_proven);
            store.observe_study_day(&dataset, accepted_day).unwrap();
            assert_eq!(authority(&store), migrated);
        }
    }
}
