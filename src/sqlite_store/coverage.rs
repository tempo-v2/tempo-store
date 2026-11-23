//! Durable Games forecasts, published against verified repertoire inputs.
use super::{SqliteStore, database_error, game_model, game_relevance, row_values};
use crate::{
    CoverageEditBasis, CoverageLineSetting, CoverageSummary, ForecastGap, ProjectionBook,
    ProjectionScope, SavedCoverage, StoreError, store_error::conflict,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use tempo_chess::{OpponentGap, PositionKey};
use tempo_types::{DatasetId, Revision};

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

    pub(super) fn read_coverage_start(
        &self,
        dataset: &DatasetId,
        repertoire: &str,
    ) -> Result<CoverageLineSetting, StoreError> {
        starting_line(&self.connection, dataset, repertoire)
    }

    /// Save the application's verified line against both the source document
    /// and the previous setting. A retry of the same value is a no-op.
    pub(super) fn write_coverage_start(
        &mut self,
        dataset: &DatasetId,
        repertoire: &str,
        content_revision: Revision,
        expected_revision: u64,
        moves: &[String],
    ) -> Result<CoverageLineSetting, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        game_relevance::check_dataset(&tx, dataset)?;
        game_relevance::check_content_revision(&tx, content_revision)?;
        check_coverage_repertoire(&tx, dataset, repertoire)?;
        let prior = starting_line(&tx, dataset, repertoire)?;
        if prior.moves == moves {
            return Ok(prior);
        }
        if prior.revision != expected_revision {
            return Err(conflict(
                "The starting line changed. Reopen coverage settings.",
            ));
        }
        let revision = prior
            .revision
            .checked_add(1)
            .ok_or_else(|| conflict("The starting line revision is exhausted."))?;
        tx.execute(
            "INSERT INTO repertoire_coverage_start VALUES (?1,?2,?3,?4)
             ON CONFLICT(dataset_id,repertoire_id) DO UPDATE
             SET revision=excluded.revision,moves=excluded.moves",
            params![dataset.0, repertoire, revision, row_values::encode(&moves)?],
        )
        .map_err(database_error)?;
        tx.commit().map_err(database_error)?;
        Ok(CoverageLineSetting {
            revision,
            moves: moves.to_vec(),
        })
    }

    pub(super) fn read_coverage_summary(
        &self,
        dataset: &DatasetId,
        repertoire: &str,
    ) -> Result<Option<SavedCoverage>, StoreError> {
        let row: Option<(String, bool)> = self.connection.query_row(
            "SELECT summary, input.input_key IS NOT report.input_key OR input.state IS NOT 'verified'
                      OR coalesce(json_extract(summary,'$.starting_line.revision'),0) <> coalesce(start.revision,0)
             FROM repertoire_coverage AS report
             LEFT JOIN game_model_inputs AS input USING(dataset_id,repertoire_id)
             LEFT JOIN repertoire_coverage_start AS start USING(dataset_id,repertoire_id)
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
                    input.input_key IS NOT report.input_key,
                    json_extract(summary,'$.request_id'), json_extract(summary,'$.through_move'),
                    json_extract(summary,'$.starting_line')
             FROM repertoire_coverage_gaps AS gap
             JOIN repertoire_coverage AS report USING(dataset_id,repertoire_id)
             JOIN game_model_inputs AS input USING(dataset_id,repertoire_id)
             LEFT JOIN repertoire_coverage_start AS start USING(dataset_id,repertoire_id)
             WHERE gap.dataset_id=?1 AND input.state='verified'
             AND coalesce(json_extract(summary,'$.starting_line.revision'),0)=coalesce(start.revision,0)
             AND (?2 IS NULL OR gap.repertoire_id=?2)
             AND NOT EXISTS (SELECT 1 FROM repertoire_suspensions AS suspended
                 WHERE (suspended.dataset_id,suspended.repertoire_id)=(gap.dataset_id,gap.repertoire_id))
             ORDER BY probability DESC,gap.repertoire_id,position,actual_move",
        ).map_err(database_error)?;
        statement
            .query_map(params![dataset.0, repertoire], |row| {
                Ok((
                    ForecastGap {
                        gap: OpponentGap {
                            repertoire_id: row.get(0)?,
                            position: PositionKey(row.get(1)?),
                            actual_move: row.get(2)?,
                        },
                        probability: row.get(3)?,
                        stale: row.get(4)?,
                        report_id: row.get(5)?,
                        through_move: row.get(6)?,
                        starting_line: CoverageLineSetting::default(),
                    },
                    row.get::<_, Option<String>>(7)?,
                ))
            })
            .map_err(database_error)?
            .map(|row| {
                let (mut forecast, setting) = row.map_err(database_error)?;
                if let Some(setting) = setting {
                    forecast.starting_line = row_values::decode(&setting)?;
                }
                Ok(forecast)
            })
            .collect()
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
        if starting_line(&tx, dataset, repertoire)? != summary.starting_line {
            return Err(conflict("The starting line changed during the scan."));
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

pub(super) fn check_edit_basis(
    connection: &Connection,
    dataset: &DatasetId,
    repertoire: &str,
    basis: &CoverageEditBasis,
) -> Result<(), StoreError> {
    check_coverage_repertoire(connection, dataset, repertoire)?;
    let current: bool = connection
        .query_row(
            "SELECT EXISTS (
             SELECT 1 FROM repertoire_coverage AS report
             LEFT JOIN repertoire_coverage_start AS start USING(dataset_id,repertoire_id)
             WHERE report.dataset_id=?1 AND report.repertoire_id=?2
               AND json_extract(report.summary,'$.request_id')=?3
               AND coalesce(json_extract(report.summary,'$.starting_line.revision'),0)=?4
               AND coalesce(start.revision,0)=?4
         )",
            params![dataset.0, repertoire, basis.report_id, basis.scope_revision],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    if current {
        Ok(())
    } else {
        Err(StoreError::StaleCoverage)
    }
}

fn check_coverage_repertoire(
    connection: &Connection,
    dataset: &DatasetId,
    repertoire: &str,
) -> Result<(), StoreError> {
    let available: bool = connection.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM game_model_inputs AS input WHERE dataset_id=?1 AND repertoire_id=?2
                AND state='verified'
                AND NOT EXISTS (SELECT 1 FROM repertoire_suspensions
                                WHERE dataset_id=input.dataset_id AND repertoire_id=input.repertoire_id)
            )",
            params![dataset.0, repertoire], |row| row.get(0),
        ).map_err(database_error)?;
    if !available {
        return Err(StoreError::StaleCoverage);
    }
    Ok(())
}

fn starting_line(
    connection: &Connection,
    dataset: &DatasetId,
    repertoire: &str,
) -> Result<CoverageLineSetting, StoreError> {
    let saved: Option<(u64, String)> = connection.query_row(
        "SELECT revision,moves FROM repertoire_coverage_start WHERE dataset_id=?1 AND repertoire_id=?2",
        params![dataset.0, repertoire],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(database_error)?;
    match saved {
        Some((revision, moves)) => Ok(CoverageLineSetting {
            revision,
            moves: row_values::decode(&moves)?,
        }),
        None => Ok(CoverageLineSetting::default()),
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
        super::super::study_plan::remove_schema_for_prior_version_fixture(&store.connection);
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
             DROP TABLE repertoire_partition_lineage; DROP TABLE repertoire_coverage_start; PRAGMA user_version=15;",
            )
            .unwrap();
        drop(store);
        let upgraded = SqliteStore::open(&path).unwrap();
        assert_eq!(
            upgraded
                .connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            super::super::SCHEMA_VERSION
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
