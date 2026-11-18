use super::{SqliteStore, database_error, load_study, row_values, save_snapshot};
use crate::store_error::{conflict, invalid};
use crate::transaction::stage_commit;
use crate::{Commit, ComparedGame, GameCommit, GameHistory, StoreError, StudyScope, Write};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

impl SqliteStore {
    pub(super) fn save_game_evidence(
        &mut self,
        commit: Commit,
        games: &[ComparedGame],
        successful_refresh_ms: Option<u64>,
    ) -> Result<GameCommit, StoreError> {
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
                    "INSERT INTO games VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
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
                        game.pgn
                    ],
                )
                .map_err(database_error)?;
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
        transaction.commit().map_err(database_error)?;
        Ok(result)
    }

    pub(super) fn read_game_history(&self) -> Result<GameHistory, StoreError> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
            .map_err(database_error)?;
        let results = {
            let mut statement = tx
                .prepare(
                    "SELECT summary FROM game_comparisons WHERE dataset_id=
                    (SELECT dataset_id FROM store_state WHERE singleton=1)
                 ORDER BY ordinal DESC LIMIT 100",
                )
                .map_err(database_error)?;
            let records = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(database_error)?;
            records
                .map(|row| row_values::decode(&row.map_err(database_error)?))
                .collect::<Result<_, _>>()?
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
