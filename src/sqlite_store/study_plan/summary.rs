//! Read and publish Study counts independently of the prepared order.
use crate::{
    SqliteStore, StoreError,
    sqlite_store::{
        database_error,
        row_values::{decode, decode_counter, encode},
    },
    store_error::invalid,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use tempo_types::{DatasetId, StudyDay, SummaryBasis, SummaryEpoch};

pub(in crate::sqlite_store) const SCHEMA: &str = include_str!("summary.sql");

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StudyFamilySummary {
    pub ready_teach: u64,
    pub ready_test: u64,
    pub ready_correction: u64,
    pub spent_introductions: u64,
    pub remaining_introductions: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StudySummary {
    pub opening: StudyFamilySummary,
    pub tactics: StudyFamilySummary,
    pub deferred: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StudySummaryRead {
    Current {
        basis: SummaryBasis,
        summary: StudySummary,
    },
    Updating,
    Unavailable,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PublishStudySummary {
    Published,
    AlreadyPublished,
    Stale,
}

impl SqliteStore {
    /// Read one summary row and its independent authority for the named day.
    pub fn study_summary(
        &self,
        dataset: &DatasetId,
        day: StudyDay,
    ) -> Result<StudySummaryRead, StoreError> {
        let row = self
            .connection
            .query_row(
                "SELECT state.summary_epoch,state.summary_dirty,result.payload
             FROM store_state active
             JOIN study_projection_state state USING(dataset_id)
             LEFT JOIN study_summary_projection result
               ON (result.dataset_id,result.study_day,result.summary_epoch)=
                  (state.dataset_id,state.study_day,state.summary_epoch)
             WHERE active.singleton=1 AND active.dataset_id=?1 AND state.study_day=?2",
                params![dataset.0, day.0],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(database_error)?;
        let Some((epoch, dirty, value)) = row else {
            return Ok(StudySummaryRead::Unavailable);
        };
        if dirty {
            return Ok(StudySummaryRead::Updating);
        }
        let Some(summary) = value.and_then(|payload| decode(&payload).ok()) else {
            return Ok(StudySummaryRead::Unavailable);
        };
        Ok(StudySummaryRead::Current {
            basis: SummaryBasis {
                dataset: dataset.clone(),
                day,
                epoch: SummaryEpoch(decode_counter(epoch)?),
            },
            summary,
        })
    }

    /// Publish counts only when their original independent basis still matches.
    pub fn publish_study_summary(
        &mut self,
        basis: &SummaryBasis,
        summary: &StudySummary,
    ) -> Result<PublishStudySummary, StoreError> {
        let payload = encode(summary)?;
        let tx = self.plan_write()?;
        let current = tx
            .query_row(
                "SELECT summary_dirty FROM store_state active
             JOIN study_projection_state state USING(dataset_id)
             WHERE active.singleton=1 AND active.dataset_id=?1 AND state.study_day=?2
               AND state.summary_epoch=?3",
                params![
                    basis.dataset.0,
                    basis.day.0,
                    basis.epoch.0.to_be_bytes().as_slice()
                ],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .map_err(database_error)?;
        let Some(dirty) = current else {
            return Ok(PublishStudySummary::Stale);
        };
        if !dirty {
            let saved: Option<String> = tx
                .query_row(
                    "SELECT payload FROM study_summary_projection
                 WHERE dataset_id=?1 AND study_day=?2 AND summary_epoch=?3",
                    params![
                        basis.dataset.0,
                        basis.day.0,
                        basis.epoch.0.to_be_bytes().as_slice()
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(database_error)?;
            if let Some(saved) = saved {
                if let Ok(saved_summary) = decode::<StudySummary>(&saved) {
                    if saved_summary != *summary {
                        return Err(invalid("a published summary basis has different counts"));
                    }
                    return Ok(PublishStudySummary::AlreadyPublished);
                }
            }
        }
        tx.execute(
            "INSERT INTO study_summary_projection(dataset_id,study_day,summary_epoch,payload)
             VALUES (?1,?2,?3,?4)
             ON CONFLICT(dataset_id) DO UPDATE SET study_day=excluded.study_day,
                 summary_epoch=excluded.summary_epoch,payload=excluded.payload",
            params![
                basis.dataset.0,
                basis.day.0,
                basis.epoch.0.to_be_bytes().as_slice(),
                payload
            ],
        )
        .map_err(database_error)?;
        tx.execute(
            "UPDATE study_projection_state SET summary_dirty=0 WHERE dataset_id=?1",
            [&basis.dataset.0],
        )
        .map_err(database_error)?;
        tx.commit().map_err(database_error)?;
        Ok(PublishStudySummary::Published)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_store::operation_probe::probe_access;
    use rusqlite::hooks::AuthAction;
    use tempo_chess::demo_catalog;
    use tempo_learning::{DailyLearning, LearningPolicy};

    #[test]
    fn summary_is_independent_of_rank_and_rejects_stale_acceptance_basis() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("summary.sqlite");
        let dataset: DatasetId = "summary".into();
        let mut store = SqliteStore::create(
            &path,
            dataset.clone(),
            demo_catalog(),
            DailyLearning::default().settings(),
        )
        .unwrap();
        let day = StudyDay(20);
        assert_eq!(
            store.study_summary(&dataset, day).unwrap(),
            StudySummaryRead::Unavailable
        );
        let basis = store
            .observe_study_day(&dataset, day)
            .unwrap()
            .unwrap()
            .summary;
        assert_eq!(
            store.study_summary(&dataset, day).unwrap(),
            StudySummaryRead::Updating
        );
        let counts = StudySummary {
            opening: StudyFamilySummary {
                ready_teach: 2,
                spent_introductions: 1,
                ..Default::default()
            },
            deferred: 3,
            ..Default::default()
        };
        store
            .connection
            .execute(
                "UPDATE study_projection_state SET rank_revision=?1",
                [7_u64.to_be_bytes().as_slice()],
            )
            .unwrap();
        assert_eq!(
            store.publish_study_summary(&basis, &counts).unwrap(),
            PublishStudySummary::Published
        );
        assert_eq!(
            store.publish_study_summary(&basis, &counts).unwrap(),
            PublishStudySummary::AlreadyPublished
        );
        assert!(
            store
                .publish_study_summary(&basis, &StudySummary::default())
                .is_err()
        );
        let (read, access) = probe_access(
            &mut store,
            |action| match action {
                AuthAction::Read {
                    table_name,
                    column_name,
                } => {
                    !matches!(
                        table_name,
                        "store_state" | "study_projection_state" | "study_summary_projection"
                    ) || column_name == "rank_revision"
                        || column_name == "head_generation"
                }
                _ => true,
            },
            |store| store.study_summary(&dataset, day),
        );
        assert!(access.denied.is_empty());
        assert_eq!(
            read.unwrap(),
            StudySummaryRead::Current {
                basis: basis.clone(),
                summary: counts.clone()
            },
        );

        let rival = SqliteStore::open(&path).unwrap();
        let tx = rival.connection.unchecked_transaction().unwrap();
        super::super::authority::mark_acceptance_projections_dirty(&tx, &dataset).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            store.publish_study_summary(&basis, &counts).unwrap(),
            PublishStudySummary::Stale
        );
        assert_eq!(
            store.study_summary(&dataset, day).unwrap(),
            StudySummaryRead::Updating
        );
        assert_eq!(
            store.study_summary(&dataset, StudyDay(21)).unwrap(),
            StudySummaryRead::Unavailable
        );
        assert_eq!(
            store.study_summary(&"changed".into(), day).unwrap(),
            StudySummaryRead::Unavailable
        );
        let fresh = store
            .study_projection_basis(&dataset)
            .unwrap()
            .unwrap()
            .summary;
        store.connection.execute_batch(
            "CREATE TRIGGER fail_summary BEFORE UPDATE OF summary_dirty ON study_projection_state
             WHEN new.summary_dirty=0 BEGIN SELECT RAISE(ABORT,'injected'); END",
        ).unwrap();
        assert!(
            store
                .publish_study_summary(&fresh, &StudySummary::default())
                .is_err()
        );
        assert_eq!(
            store.study_summary(&dataset, day).unwrap(),
            StudySummaryRead::Updating
        );
        let original: Vec<u8> = store
            .connection
            .query_row(
                "SELECT summary_epoch FROM study_summary_projection WHERE dataset_id=?1",
                [&dataset.0],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(decode_counter(original).unwrap(), basis.epoch.0);
    }
}
