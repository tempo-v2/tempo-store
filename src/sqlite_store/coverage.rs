//! Durable Games forecasts, published against verified repertoire inputs.
use super::{SqliteStore, database_error, game_model, game_relevance, row_values};
use crate::{
    CoverageSummary, ForecastGap, ProjectionBook, ProjectionScope, SavedCoverage, StoreError,
    store_error::conflict,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use tempo_chess::{OpponentGap, PositionKey};
use tempo_types::DatasetId;

impl SqliteStore {
    pub fn coverage_book(
        &self,
        dataset: &DatasetId,
        repertoire: &str,
    ) -> Result<Option<ProjectionBook>, StoreError> {
        Ok(game_model::books(
            &self.connection,
            dataset,
            ProjectionScope::AllVerified,
            Some(repertoire),
        )?
        .pop())
    }

    pub(super) fn read_coverage_summary(
        &self,
        dataset: &DatasetId,
        repertoire: &str,
    ) -> Result<Option<SavedCoverage>, StoreError> {
        let row: Option<(String, bool)> = self.connection.query_row(
            "SELECT summary, input.input_key IS NOT report.input_key OR input.state IS NOT 'verified'
             FROM repertoire_coverage AS report
             LEFT JOIN game_model_inputs AS input USING(dataset_id,repertoire_id)
             WHERE report.dataset_id=?1 AND report.repertoire_id=?2",
            params![dataset.0, repertoire],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(database_error)?;
        row.map(|(summary, stale)| {
            Ok(SavedCoverage {
                summary: row_values::decode(&summary)?,
                stale,
            })
        })
        .transpose()
    }

    pub(super) fn read_forecast_gaps(
        &self,
        dataset: &DatasetId,
        repertoire: Option<&str>,
    ) -> Result<Vec<ForecastGap>, StoreError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT gap.repertoire_id,position,actual_move,probability,
                    input.input_key IS NOT report.input_key
             FROM repertoire_coverage_gaps AS gap
             JOIN repertoire_coverage AS report USING(dataset_id,repertoire_id)
             JOIN game_model_inputs AS input USING(dataset_id,repertoire_id)
             WHERE gap.dataset_id=?1 AND input.state='verified' AND (?2 IS NULL OR gap.repertoire_id=?2)
             AND NOT EXISTS (SELECT 1 FROM repertoire_suspensions AS suspended
                 WHERE (suspended.dataset_id,suspended.repertoire_id)=(gap.dataset_id,gap.repertoire_id))
             ORDER BY probability DESC,gap.repertoire_id,position,actual_move",
        ).map_err(database_error)?;
        statement
            .query_map(params![dataset.0, repertoire], |row| {
                Ok(ForecastGap {
                    gap: OpponentGap {
                        repertoire_id: row.get(0)?,
                        position: PositionKey(row.get(1)?),
                        actual_move: row.get(2)?,
                    },
                    probability: row.get(3)?,
                    stale: row.get(4)?,
                })
            })
            .map_err(database_error)?
            .collect::<Result<_, _>>()
            .map_err(database_error)
    }

    pub fn publish_coverage(
        &mut self,
        dataset: &DatasetId,
        repertoire: &str,
        input_key: &str,
        prior_report_id: Option<&str>,
        summary: &CoverageSummary,
        gaps: &[(PositionKey, String, f64)],
    ) -> Result<(), StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        game_relevance::check_dataset(&tx, dataset)?;
        if !game_model::current_input(&tx, dataset, repertoire, input_key)? {
            return Err(conflict("The repertoire changed during the scan."));
        }
        let prior: Option<String> = tx
            .query_row(
                "SELECT json_extract(summary,'$.request_id') FROM repertoire_coverage
             WHERE dataset_id=?1 AND repertoire_id=?2",
                params![dataset.0, repertoire],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)?;
        if prior.as_deref() != prior_report_id {
            return Err(conflict("A newer coverage scan was published."));
        }
        tx.execute(
            "DELETE FROM repertoire_coverage WHERE dataset_id=?1 AND repertoire_id=?2",
            params![dataset.0, repertoire],
        )
        .map_err(database_error)?;
        tx.execute(
            "INSERT INTO repertoire_coverage VALUES (?1,?2,?3,?4)",
            params![
                dataset.0,
                repertoire,
                input_key,
                row_values::encode(summary)?
            ],
        )
        .map_err(database_error)?;
        {
            let mut insert = tx
                .prepare_cached("INSERT INTO repertoire_coverage_gaps VALUES (?1,?2,?3,?4,?5)")
                .map_err(database_error)?;
            for (position, played, probability) in gaps {
                insert
                    .execute(params![
                        dataset.0,
                        repertoire,
                        position.0,
                        played,
                        probability
                    ])
                    .map_err(database_error)?;
            }
        }
        tx.commit().map_err(database_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use tempo_chess::Catalog;
    use tempo_learning::{LearningPolicy, StandardLearning};

    #[test]
    fn upgrade_from_schema_15_preserves_the_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema-15.sqlite");
        let dataset = DatasetId("coverage-dataset".into());
        let store = SqliteStore::create(
            &path,
            dataset.clone(),
            Catalog::default(),
            StandardLearning::<tempo_learning::FixedIntervalScheduler>::default().settings(),
        )
        .unwrap();
        store.connection.execute_batch(
            "DROP TABLE repertoire_coverage_gaps; DROP TABLE repertoire_coverage; PRAGMA user_version=15;"
        ).unwrap();
        drop(store);
        let upgraded = SqliteStore::open(&path).unwrap();
        assert_eq!(
            upgraded
                .connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            16
        );
        assert!(upgraded.forecast_gaps(&dataset, None).unwrap().is_empty());
        assert!(
            upgraded
                .coverage_summary(&dataset, "book")
                .unwrap()
                .is_none()
        );
    }
}
