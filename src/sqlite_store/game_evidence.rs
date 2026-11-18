use super::{SqliteStore, database_error, load_study, row_values, save_snapshot};
use crate::store_error::{conflict, invalid};
use crate::transaction::stage_commit;
use crate::{AcceptedGames, GameReceipt, PgnTiming};
use crate::{
    Commit, ComparedGame, GameCommit, GameDeviation, GameHistory, GameHistoryEntry, StoreError,
    StudyScope, Write,
};
use rusqlite::Connection;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use tempo_types::{ContentVersion, DatasetId, TargetId};

impl SqliteStore {
    pub(super) fn save_game_evidence(
        &mut self,
        commit: Commit,
        games: &[ComparedGame],
        successful_refresh_ms: Option<u64>,
        receipt: GameReceipt<'_>,
    ) -> Result<AcceptedGames, StoreError> {
        if commit
            .writes
            .iter()
            .any(|write| !matches!(write, Write::Learning { .. }))
        {
            return Err(invalid(
                "game evidence only accepts associated learning facts",
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let scope = StudyScope {
            catalog: !commit.writes.is_empty(),
            ..StudyScope::default()
        };
        let previous = load_study::load_study(&transaction, scope)?;
        if let Some(accepted) = read_receipt(
            &transaction,
            &commit.expected_dataset,
            receipt.id,
            receipt.fingerprint,
        )? {
            return Ok(accepted);
        }
        if previous.dataset != commit.expected_dataset
            || previous.revision != commit.expected_revision
        {
            return Err(conflict("repertoire or learning changed during comparison"));
        }

        let dataset = &previous.dataset.0;
        let mut result = GameCommit::default();
        for entry in games {
            let game = &entry.game;
            let raw = transaction
                .execute(
                    "INSERT INTO games VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
                 ON CONFLICT(dataset_id,source,id) DO NOTHING",
                    params![
                        dataset,
                        game.source,
                        game.id,
                        game.url,
                        game.white,
                        game.black,
                        game.start_ms,
                        game.end_ms,
                        game.fetched_ms,
                        game.pgn,
                        game.mainline_signature
                    ],
                )
                .map_err(database_error)?;
            if raw == 0
                && let Some(timing) = game.precise_pgn_timing()
            {
                let mut current = read_pgn_timing(&transaction, dataset, &game.id)?
                    .ok_or_else(|| conflict("the stored PGN start time does not match"))?;
                let previous_end = current.end_ms;
                current.merge_end(&timing)?;
                if current.end_ms != timing.end_ms {
                    return Err(conflict(
                        "PGN timing changed during comparison; retry the command",
                    ));
                }
                if previous_end != current.end_ms {
                    transaction.execute(
                        "UPDATE games SET end_ms=?3 WHERE dataset_id=?1 AND source='pgn' AND id=?2",
                        params![dataset, game.id, current.end_ms],
                    ).map_err(database_error)?;
                }
            } else if raw == 0 {
                let same: bool = transaction
                    .query_row(
                        "SELECT (mainline_signature IS NULL OR mainline_signature=?4)
                         AND start_ms IS ?5 AND end_ms IS ?6
                     FROM games WHERE dataset_id=?1 AND source=?2 AND id=?3",
                        params![
                            dataset,
                            game.source,
                            game.id,
                            game.mainline_signature,
                            game.start_ms,
                            game.end_ms
                        ],
                        |row| row.get(0),
                    )
                    .map_err(database_error)?;
                if !same {
                    return Err(conflict(
                        "a source game identity was delivered with different game facts",
                    ));
                }
            }
            result.new_games += raw;
            result.duplicates += 1 - raw;
            let compared = transaction
                .execute(
                    "INSERT INTO game_comparisons(dataset_id,id,source,game_id,summary)
                 VALUES (?1,?2,?3,?4,?5) ON CONFLICT(dataset_id,id) DO NOTHING",
                    params![
                        dataset,
                        entry.id,
                        game.source,
                        game.id,
                        row_values::encode(&entry.summary)?
                    ],
                )
                .map_err(database_error)?;
            if compared == 0 {
                if let Some(timing) = game.precise_pgn_timing()
                    && let Some(end_ms) = timing.end_ms
                {
                    transaction
                        .execute(
                            "UPDATE game_comparisons
                         SET summary=json_set(summary,'$.occurrence_end_ms',?3)
                         WHERE dataset_id=?1 AND id=?2
                           AND json_extract(summary,'$.occurrence_end_ms') IS NULL",
                            params![dataset, entry.id, end_ms],
                        )
                        .map_err(database_error)?;
                }
                continue;
            }
            for observation in &entry.observations {
                transaction
                    .execute(
                        "INSERT INTO game_observations VALUES (?1,?2,?3,?4,?5,?6,?7)",
                        params![
                            dataset,
                            entry.id,
                            observation.ply,
                            observation.target.0,
                            observation.version.0.to_be_bytes().as_slice(),
                            observation.played,
                            observation.prescribed
                        ],
                    )
                    .map_err(database_error)?;
            }
        }
        if let Some(at_ms) = successful_refresh_ms {
            transaction
                .execute(
                    "INSERT INTO successful_game_refreshes VALUES (?1,?2)
                 ON CONFLICT(dataset_id) DO UPDATE SET at_ms=max(at_ms,excluded.at_ms)",
                    params![dataset, at_ms],
                )
                .map_err(database_error)?;
        }
        if !commit.writes.is_empty() {
            let next = stage_commit(&previous, commit)?;
            save_snapshot(&transaction, Some(&previous), &next)?;
        }
        let accepted = AcceptedGames {
            counts: GameCommit {
                new_games: result.new_games,
                duplicates: result.duplicates,
            },
            report: receipt.report.into(),
        };
        transaction
            .execute(
                "INSERT INTO game_receipts VALUES (?1,?2,?3,?4)",
                params![
                    dataset,
                    receipt.id,
                    receipt.fingerprint,
                    row_values::encode(&accepted)?
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)?;
        Ok(accepted)
    }

    pub(super) fn read_game_receipt(
        &self,
        dataset: &DatasetId,
        id: &str,
        fingerprint: &str,
    ) -> Result<Option<AcceptedGames>, StoreError> {
        read_receipt(&self.connection, dataset, id, fingerprint)
    }

    pub(super) fn read_verified_game(
        &self,
        dataset: &DatasetId,
        url: &str,
        signature: &str,
    ) -> Result<Option<crate::GameRecord>, StoreError> {
        self.connection
            .query_row(
                "SELECT source,id,white,black,start_ms,end_ms,fetched_ms,pgn FROM games
             WHERE dataset_id=?1 AND url=?2 AND mainline_signature=?3 LIMIT 1",
                params![dataset.0, url, signature],
                |row| {
                    Ok(crate::GameRecord {
                        source: row.get(0)?,
                        id: row.get(1)?,
                        url: Some(url.into()),
                        white: row.get(2)?,
                        black: row.get(3)?,
                        start_ms: row.get(4)?,
                        end_ms: row.get(5)?,
                        fetched_ms: row.get(6)?,
                        pgn: row.get(7)?,
                        mainline_signature: signature.into(),
                    })
                },
            )
            .optional()
            .map_err(database_error)
    }

    pub(super) fn read_game_history(&self) -> Result<GameHistory, StoreError> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
            .map_err(database_error)?;
        let results = {
            let mut statement = tx
                .prepare(
                    "SELECT comparison.dataset_id, comparison.id, comparison.summary, game.start_ms
                 FROM game_comparisons AS comparison
                 JOIN games AS game ON game.dataset_id=comparison.dataset_id
                    AND game.source=comparison.source AND game.id=comparison.game_id
                 WHERE comparison.dataset_id=(SELECT dataset_id FROM store_state WHERE singleton=1)
                 ORDER BY comparison.ordinal DESC LIMIT 100",
                )
                .map_err(database_error)?;
            let records = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<u64>>(3)?,
                    ))
                })
                .map_err(database_error)?;
            let mut misses = tx
                .prepare(
                    "SELECT DISTINCT target_id, version FROM game_observations
                 WHERE dataset_id=?1 AND comparison_id=?2 AND played<>prescribed",
                )
                .map_err(database_error)?;
            let mut results = Vec::new();
            for record in records {
                let (dataset, comparison, summary, start_ms) = record.map_err(database_error)?;
                let summary: crate::GameSummary = row_values::decode(&summary)?;
                let rows = misses
                    .query_map(params![dataset, comparison], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
                    })
                    .map_err(database_error)?;
                let deviations = rows
                    .map(|row| {
                        let (target, version) = row.map_err(database_error)?;
                        Ok(GameDeviation {
                            target: TargetId(target),
                            version: ContentVersion(row_values::decode_counter(version)?),
                            start_ms,
                            end_ms: summary.occurrence_end_ms,
                        })
                    })
                    .collect::<Result<Vec<_>, StoreError>>()?;
                results.push(GameHistoryEntry {
                    comparison_id: comparison,
                    summary,
                    deviations,
                });
            }
            results
        };
        let last_successful_refresh_ms = tx
            .query_row(
                "SELECT at_ms FROM successful_game_refreshes WHERE dataset_id=
                (SELECT dataset_id FROM store_state WHERE singleton=1)",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)?;
        tx.commit().map_err(database_error)?;
        Ok(GameHistory {
            results,
            last_successful_refresh_ms,
        })
    }
}

pub(super) fn read_pgn_timing(
    connection: &Connection,
    dataset: &str,
    id: &str,
) -> Result<Option<PgnTiming>, StoreError> {
    connection
        .query_row(
            "SELECT mainline_signature, start_ms, end_ms FROM games
         WHERE dataset_id=?1 AND source='pgn' AND id=?2 AND start_ms IS NOT NULL",
            params![dataset, id],
            |row| {
                Ok(PgnTiming {
                    mainline_signature: row.get(0)?,
                    start_ms: row.get(1)?,
                    end_ms: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(database_error)
}

fn read_receipt(
    connection: &Connection,
    dataset: &DatasetId,
    id: &str,
    fingerprint: &str,
) -> Result<Option<AcceptedGames>, StoreError> {
    let saved: Option<(String, String)> = connection
        .query_row(
            "SELECT fingerprint, accepted FROM game_receipts WHERE dataset_id=?1 AND id=?2",
            params![dataset.0, id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(database_error)?;
    match saved {
        Some((key, _)) if key != fingerprint => {
            Err(conflict("game command ID was reused with different inputs"))
        }
        Some((_, accepted)) => row_values::decode(&accepted).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GameRecord, GameSummary, Store};
    use tempo_learning::LearningPolicy;

    #[test]
    fn a_late_timing_conflict_rolls_back_earlier_enrichment_and_the_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let dataset = DatasetId("games".into());
        let mut store = SqliteStore::create(
            dir.path().join("games.sqlite"),
            dataset.clone(),
            Default::default(),
            crate::test_support::policy().settings(),
        )
        .unwrap();
        let revision = store.snapshot().unwrap().revision;
        let commit = || Commit {
            expected_revision: revision,
            expected_dataset: dataset.clone(),
            writes: vec![],
        };
        let comparison = |id: &str, end_ms| ComparedGame {
            game: GameRecord {
                source: "pgn".into(),
                id: id.into(),
                url: None,
                white: "learner".into(),
                black: "opponent".into(),
                start_ms: Some(100),
                end_ms,
                fetched_ms: 1000,
                pgn: "game".into(),
                mainline_signature: id.into(),
            },
            id: format!("comparison-{id}"),
            summary: GameSummary {
                source: "pgn".into(),
                game_id: id.into(),
                url: None,
                repertoire_id: "book".into(),
                learner: "learner".into(),
                occurrence_end_ms: end_ms,
                matches: 0,
                deviations: 0,
                first_deviation_ply: None,
                coverage_gaps: 0,
                opponent_deviations: 0,
            },
            observations: vec![],
        };
        let receipt = |id| GameReceipt {
            id,
            fingerprint: id,
            report: "{}",
        };
        store
            .commit_games(
                commit(),
                &[comparison("first", None), comparison("second", Some(200))],
                None,
                receipt("base"),
            )
            .unwrap();
        let rejected = store.commit_games(
            commit(),
            &[
                comparison("first", Some(250)),
                comparison("second", Some(300)),
            ],
            None,
            receipt("conflict"),
        );
        assert!(matches!(rejected, Err(StoreError::Conflict(_))));
        assert_eq!(
            store.pgn_timing(&dataset, "first").unwrap().unwrap().end_ms,
            None
        );
        assert_eq!(
            store
                .pgn_timing(&dataset, "second")
                .unwrap()
                .unwrap()
                .end_ms,
            Some(200)
        );
        assert!(
            store
                .game_receipt(&dataset, "conflict", "conflict")
                .unwrap()
                .is_none()
        );
        let recent = store.game_history().unwrap();
        assert_eq!(recent.results.len(), 2);
        assert_eq!(recent.results[1].summary.occurrence_end_ms, None);
    }
}
