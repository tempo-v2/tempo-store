//! Resolve one day's deferral cutoff and its immutable delivery receipt.

use super::{
    authority::{ensure_day, mark_acceptance_projections_dirty},
    store_count,
};
use crate::{
    ReceiptResult, StoreError,
    sqlite_store::{
        SqliteStore, database_error,
        row_values::{decode_receipt, encode, encode_receipt},
    },
};
use rusqlite::{OptionalExtension, params};
use tempo_types::{DatasetId, StudyDay, SubmissionId};

#[derive(Debug, PartialEq, Eq)]
pub enum ResumeSkippedOutcome {
    Accepted { day: StudyDay, cutoff: u64 },
    WrongDataset,
    StaleDay,
    CommandConflict,
}

impl SqliteStore {
    /// Advance today's cutoff to the latest committed deferral. The runtime's
    /// Study calendar supplies the current day. Accepted replay uses the
    /// original outcome even after rollover, without advancing another day.
    pub fn resume_skipped(
        &mut self,
        dataset: &DatasetId,
        requested_day: StudyDay,
        submission: &SubmissionId,
        current_day: StudyDay,
    ) -> Result<ResumeSkippedOutcome, StoreError> {
        let tx = self.plan_write()?;
        let active: String = tx
            .query_row(
                "SELECT dataset_id FROM store_state WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        if active != dataset.0 {
            return Ok(ResumeSkippedOutcome::WrongDataset);
        }

        let command = encode(&("resume_skipped", dataset, requested_day))?;
        let receipt = tx
            .query_row(
                "SELECT command,result FROM receipts WHERE id=?1",
                [&submission.0],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(database_error)?;
        if let Some((original, outcome)) = receipt {
            if original != command {
                return Ok(ResumeSkippedOutcome::CommandConflict);
            }
            return Ok(match decode_receipt(&outcome)? {
                ReceiptResult::ResumeSkipped { day, cutoff } if day == requested_day => {
                    ResumeSkippedOutcome::Accepted { day, cutoff }
                }
                _ => ResumeSkippedOutcome::CommandConflict,
            });
        }
        if requested_day != current_day {
            return Ok(ResumeSkippedOutcome::StaleDay);
        }

        ensure_day(&tx, dataset, requested_day)?;
        let (latest, prior): (i64, i64) = tx
            .query_row(
                "SELECT latest_skip,skip_cutoff FROM study_day_authority
             WHERE dataset_id=?1 AND study_day=?2",
                params![dataset.0, requested_day.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(database_error)?;
        let cutoff = store_count(latest)?;
        if latest != prior {
            tx.execute(
                "UPDATE study_day_authority SET skip_cutoff=latest_skip
                 WHERE dataset_id=?1 AND study_day=?2",
                params![dataset.0, requested_day.0],
            )
            .map_err(database_error)?;
            mark_acceptance_projections_dirty(&tx, dataset)?;
        }
        tx.execute(
            "INSERT INTO receipts (id,attempt_id,command,result) VALUES (?1,NULL,?2,?3)",
            params![
                submission.0,
                command,
                encode_receipt(&ReceiptResult::ResumeSkipped {
                    day: requested_day,
                    cutoff
                })?,
            ],
        )
        .map_err(database_error)?;
        tx.commit().map_err(database_error)?;
        Ok(ResumeSkippedOutcome::Accepted {
            day: requested_day,
            cutoff,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StudyReceiptLookup, StudyReceiptRead, test_support::policy};
    use tempo_chess::demo_catalog;
    use tempo_learning::LearningPolicy;

    #[test]
    fn resume_skipped_replays_an_exact_cutoff_without_clearing_later_deferrals() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("deferrals.sqlite");
        let dataset: DatasetId = "deferrals".into();
        let command: SubmissionId = "resume".into();
        let today = StudyDay(20);
        let tomorrow = StudyDay(21);
        let mut store =
            SqliteStore::create(&path, dataset.clone(), demo_catalog(), policy().settings())
                .unwrap();
        let frontier = |store: &SqliteStore, day: StudyDay, ordinal: i64| {
            store
                .connection
                .execute(
                    "INSERT INTO study_day_authority(dataset_id,study_day,latest_skip)
                 VALUES (?1,?2,?3) ON CONFLICT(dataset_id,study_day)
                 DO UPDATE SET latest_skip=excluded.latest_skip",
                    params![dataset.0, day.0, ordinal],
                )
                .unwrap();
        };
        frontier(&store, today, 4);
        frontier(&store, tomorrow, 2);
        store
            .connection
            .execute("DELETE FROM study_projection_state", [])
            .unwrap();

        let cutoff = |store: &SqliteStore, day: StudyDay| -> i64 {
            store.connection.query_row(
                "SELECT skip_cutoff FROM study_day_authority WHERE dataset_id=?1 AND study_day=?2",
                params![dataset.0, day.0],
                |row| row.get(0),
            ).unwrap()
        };
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER refuse_resume BEFORE INSERT ON receipts
             BEGIN SELECT RAISE(ABORT,'test failure'); END",
            )
            .unwrap();
        assert!(
            store
                .resume_skipped(&dataset, today, &command, today)
                .is_err()
        );
        assert_eq!(cutoff(&store, today), 0);
        assert!(matches!(
            store.study_receipt(&dataset, &command).unwrap(),
            StudyReceiptLookup::Missing
        ));
        store
            .connection
            .execute_batch("DROP TRIGGER refuse_resume")
            .unwrap();

        let accepted = ResumeSkippedOutcome::Accepted {
            day: today,
            cutoff: 4,
        };
        assert_eq!(
            store
                .resume_skipped(&dataset, today, &command, today)
                .unwrap(),
            accepted
        );
        assert_eq!(cutoff(&store, today), 4);
        let encoded: String = store
            .connection
            .query_row(
                "SELECT result FROM receipts WHERE id=?1",
                [&command.0],
                |row| row.get(0),
            )
            .unwrap();
        let mut raw: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(raw["ResumeSkipped"]["receipt_version"], 1);
        assert_eq!(
            decode_receipt(&encoded).unwrap(),
            ReceiptResult::ResumeSkipped {
                day: today,
                cutoff: 4
            },
        );
        raw["ResumeSkipped"]["receipt_version"] = 9.into();
        assert!(decode_receipt(&raw.to_string()).is_err());
        frontier(&store, today, 5);
        store
            .connection
            .execute(
                "INSERT INTO study_deferrals
               (dataset_id,study_day,target_id,version,work_kind,obligation_revision,ordinal)
             VALUES (?1,?2,'later',?3,'teach',?3,5)",
                params![dataset.0, today.0, 1_u64.to_be_bytes().as_slice()],
            )
            .unwrap();
        drop(store);

        let mut store = SqliteStore::open(&path).unwrap();
        assert_eq!(
            store
                .resume_skipped(&dataset, today, &command, tomorrow)
                .unwrap(),
            accepted
        );
        assert_eq!(cutoff(&store, today), 4);
        assert_eq!(cutoff(&store, tomorrow), 0);
        let effective: i64 = store
            .connection
            .query_row(
                "SELECT count(*) FROM study_deferrals
             JOIN study_day_authority USING(dataset_id,study_day)
             WHERE dataset_id=?1 AND study_day=?2 AND ordinal>skip_cutoff",
                params![dataset.0, today.0],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(effective, 1);
        assert_eq!(
            store
                .resume_skipped(&dataset, tomorrow, &command, tomorrow)
                .unwrap(),
            ResumeSkippedOutcome::CommandConflict,
        );
        assert_eq!(
            store
                .resume_skipped(&dataset, today, &"new".into(), tomorrow)
                .unwrap(),
            ResumeSkippedOutcome::StaleDay,
        );
        assert_eq!(
            store
                .resume_skipped(&"other".into(), today, &command, today)
                .unwrap(),
            ResumeSkippedOutcome::WrongDataset,
        );
    }
}
