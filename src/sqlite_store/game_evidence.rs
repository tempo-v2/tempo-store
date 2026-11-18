use super::{SqliteStore, database_error, load_study, row_values, save_snapshot};
use crate::store_error::{conflict, invalid};
use crate::transaction::stage_commit;
use crate::{AcceptedGames, GameReceipt, PgnTiming};
use crate::{
    Commit, ComparedGame, GameCommit, GameHistory, GameHistoryEntry, StoreError, StudyScope, Write,
};
use rusqlite::Connection;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::BTreeMap;
use tempo_chess::ContentKind;
use tempo_learning::{AssessmentSource, LearningEffect};
use tempo_types::DatasetId;
use tempo_types::{ContentVersion, Grade, TargetId};

pub(super) fn game_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<crate::GameRecord> {
    Ok(crate::GameRecord {
        source: row.get(0)?,
        id: row.get(1)?,
        url: row.get(2)?,
        white: row.get(3)?,
        black: row.get(4)?,
        start_ms: row.get(5)?,
        end_ms: row.get(6)?,
        fetched_ms: row.get(7)?,
        pgn: row.get(8)?,
        mainline_signature: row.get(9)?,
        played_date_ms: row.get(10)?,
    })
}

pub(super) fn save_failure(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    key: &crate::GameFailureKey,
    assessment: Option<&crate::AssessmentRecord>,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO game_failures VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            dataset.0,
            key.source,
            key.game_id,
            key.target.0,
            key.version.0.to_be_bytes().as_slice(),
            assessment.map(row_values::encode).transpose()?,
            key.chosen_position,
        ],
    )
    .map_err(database_error)?;
    Ok(())
}

pub(super) fn read_game_failures(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
) -> Result<Vec<crate::GameFailureKey>, StoreError> {
    row_values::read_selected(
        tx,
        "SELECT source,game_id,target_id,version,chosen_position FROM game_failures
         WHERE dataset_id=?1 ORDER BY source,game_id,target_id",
        [&dataset.0],
        |row| {
            Ok(crate::GameFailureKey {
                source: row.get(0).map_err(database_error)?,
                game_id: row.get(1).map_err(database_error)?,
                target: TargetId(row.get(2).map_err(database_error)?),
                version: ContentVersion(row_values::decode_counter(
                    row.get(3).map_err(database_error)?,
                )?),
                chosen_position: row.get(4).map_err(database_error)?,
            })
        },
    )
}

pub(super) fn read_portable_evidence(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
) -> Result<crate::PortableGameEvidence, StoreError> {
    let game_failures = read_game_failures(tx, dataset)?;
    let repertoire_reset_boundaries = row_values::read_selected(
        tx,
        "SELECT repertoire_id,max(reset_at_ms) FROM (
            SELECT repertoire_id,reset_at_ms FROM repertoire_resets WHERE dataset_id=?1
            UNION ALL
            SELECT repertoire_id,reset_at_ms FROM restored_reset_boundaries WHERE dataset_id=?1
         ) GROUP BY repertoire_id",
        [&dataset.0],
        |row| {
            Ok((
                row.get(0).map_err(database_error)?,
                row.get(1).map_err(database_error)?,
            ))
        },
    )?;
    let resolved_game_choices = row_values::read_selected(
        tx,
        "SELECT DISTINCT source,game_id,position FROM game_choice_boundaries
         WHERE dataset_id=?1 ORDER BY source,game_id,position",
        [&dataset.0],
        |row| {
            Ok(crate::ResolvedGameChoice {
                source: row.get(0).map_err(database_error)?,
                game_id: row.get(1).map_err(database_error)?,
                position: row.get(2).map_err(database_error)?,
            })
        },
    )?;
    Ok(crate::PortableGameEvidence {
        game_failures,
        repertoire_reset_boundaries,
        resolved_game_choices,
    })
}

pub(super) fn save_portable_evidence(
    tx: &Transaction<'_>,
    next: &crate::Snapshot,
    evidence: &crate::PortableGameEvidence,
) -> Result<(), StoreError> {
    let repertoires: std::collections::BTreeSet<_> = next
        .catalog
        .items
        .values()
        .filter_map(|item| match &item.definition.kind {
            ContentKind::Repertoire { repertoire, .. } => Some(repertoire),
            ContentKind::Tactic { .. } => None,
        })
        .collect();
    for (repertoire, at_ms) in &evidence.repertoire_reset_boundaries {
        if !repertoires.contains(repertoire) || i64::try_from(*at_ms).is_err() {
            return Err(invalid("invalid portable repertoire reset boundary"));
        }
        tx.execute(
            "INSERT INTO restored_reset_boundaries VALUES (?1,?2,?3)",
            params![next.dataset.0, repertoire, at_ms],
        )
        .map_err(database_error)?;
    }
    for key in &evidence.game_failures {
        if key.source.is_empty()
            || key.game_id.is_empty()
            || key.chosen_position.as_ref().is_some_and(String::is_empty)
            || next
                .catalog
                .items
                .get(&key.target)
                .is_none_or(|item| key.version.0 == 0 || item.version.0 < key.version.0)
        {
            return Err(invalid("invalid portable game failure provenance"));
        }
        save_failure(tx, &next.dataset, key, None)?;
    }
    for choice in &evidence.resolved_game_choices {
        if choice.source.is_empty() || choice.game_id.is_empty() || choice.position.is_empty() {
            return Err(invalid("invalid portable game choice resolution"));
        }
        tx.execute(
            "INSERT INTO restored_game_choices VALUES (?1,?2,?3,?4)",
            params![
                next.dataset.0,
                choice.source,
                choice.game_id,
                choice.position
            ],
        )
        .map_err(database_error)?;
    }
    Ok(())
}

fn failure_key(
    entry: &ComparedGame,
    assessment: &crate::AssessmentRecord,
) -> crate::GameFailureKey {
    crate::GameFailureKey {
        source: entry.game.source.clone(),
        game_id: entry.game.id.clone(),
        target: assessment.target.clone(),
        version: assessment.content_version,
        chosen_position: entry
            .resolved_choice
            .as_ref()
            .map(|(_, _, position)| position.clone()),
    }
}

fn validate_game_transitions(
    previous: &crate::Snapshot,
    writes: &[Write],
    games: &[ComparedGame],
) -> Result<(), StoreError> {
    let mut final_states = BTreeMap::new();
    for entry in games {
        for assessment in &entry.failures {
            let item = previous
                .catalog
                .items
                .get(&assessment.target)
                .filter(|item| item.active() && item.version == assessment.content_version)
                .ok_or_else(|| invalid("game failure does not address an active answer"))?;
            let prior = final_states
                .get(&assessment.target)
                .copied()
                .unwrap_or(&previous.learning[&assessment.target]);
            let state = &assessment.transition.state;
            if entry.game.source.is_empty()
                || entry.game.id.is_empty()
                || assessment.key != failure_key(entry, assessment).assessment_key()
                || assessment.dataset != previous.dataset
                || assessment.input.grade != Grade::Again
                || !matches!(&assessment.input.source, AssessmentSource::Game { source, game_id, .. }
                    if *source == entry.game.source && *game_id == entry.game.id)
                || assessment.transition.effect != LearningEffect::MissRecorded
                || state.last_review_at != Some(assessment.input.accepted_at)
                || state
                    .correction
                    .as_ref()
                    .is_none_or(|correction| correction.answer_version != item.version)
                || prior.miss_revision.0.checked_add(1) != Some(state.miss_revision.0)
                || prior.scheduled_reviews.checked_add(1) != Some(state.scheduled_reviews)
            {
                return Err(invalid("invalid game failure assessment"));
            }
            final_states.insert(assessment.target.clone(), state);
        }
    }
    for write in writes {
        let Write::Learning { target, state } = write else {
            return Err(invalid(
                "game evidence only accepts associated learning facts",
            ));
        };
        if final_states.remove(target) != Some(state) {
            return Err(invalid("game assessment and final learning write disagree"));
        }
    }
    if !final_states.is_empty() {
        return Err(invalid(
            "game assessment is missing its final learning write",
        ));
    }
    Ok(())
}

impl SqliteStore {
    pub(super) fn save_game_evidence(
        &mut self,
        commit: Commit,
        games: &[ComparedGame],
        successful_refresh_ms: Option<u64>,
        receipt: GameReceipt<'_>,
    ) -> Result<AcceptedGames, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let scope = StudyScope {
            catalog: !commit.writes.is_empty()
                || games.iter().any(|entry| !entry.failures.is_empty()),
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

        validate_game_transitions(&previous, &commit.writes, games)?;

        let dataset = &previous.dataset.0;
        let mut result = GameCommit::default();
        for entry in games {
            let game = &entry.game;
            let raw = transaction
                .execute(
                    "INSERT INTO games VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
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
                        game.mainline_signature,
                        game.played_date_ms
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
            if let Some((comparison, ply, position)) = &entry.resolved_choice {
                transaction
                    .execute(
                        "INSERT INTO game_choice_resolutions VALUES (?1,?2,?3,?4,?5,?6)",
                        params![dataset, comparison, ply, game.source, game.id, position],
                    )
                    .map_err(database_error)?;
            }
            for assessment in &entry.failures {
                save_failure(
                    &transaction,
                    &previous.dataset,
                    &failure_key(entry, assessment),
                    Some(assessment),
                )?;
            }
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
                        played_date_ms: None,
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
                    "SELECT comparison.dataset_id, comparison.id, comparison.summary
                 FROM game_comparisons AS comparison
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
                    ))
                })
                .map_err(database_error)?;
            let mut results = Vec::new();
            for record in records {
                let (dataset, comparison, summary) = record.map_err(database_error)?;
                let mut summary: crate::GameSummary = row_values::decode(&summary)?;
                if !summary.choices.is_empty() {
                    let resolved: Vec<String> = row_values::read_selected(
                        &tx,
                        "SELECT position FROM game_choice_boundaries WHERE dataset_id=?1 AND source=?2 AND game_id=?3 UNION SELECT chosen_position FROM game_failures WHERE dataset_id=?1 AND source=?2 AND game_id=?3 AND chosen_position IS NOT NULL",
                        params![dataset, summary.source, summary.game_id],
                        |row| row.get(0).map_err(database_error),
                    )?;
                    summary
                        .choices
                        .retain(|choice| !resolved.contains(&choice.position));
                    if summary.choices.is_empty() {
                        summary.correction_effect =
                            Some(tempo_learning::GameReviewEffect::AlreadyAddressed);
                    }
                }
                results.push(GameHistoryEntry {
                    comparison_id: comparison,
                    summary,
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

    fn comparison(id: &str, end_ms: Option<u64>) -> ComparedGame {
        ComparedGame {
            game: GameRecord {
                source: "pgn".into(),
                id: id.into(),
                url: None,
                white: "learner".into(),
                black: "opponent".into(),
                start_ms: Some(100),
                end_ms,
                fetched_ms: 1000,
                played_date_ms: None,
                pgn: "game".into(),
                mainline_signature: id.into(),
            },
            id: format!("comparison-{id}"),
            failures: vec![],
            resolved_choice: None,
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
                correction_effect: None,
                choices: vec![],
                disagreements: vec![],
            },
            observations: vec![],
        }
    }

    #[test]
    fn game_batch_requires_the_final_transition_and_canonical_event_identity() {
        use tempo_learning::{AssessmentInput, DailyLearning};
        use tempo_types::{StudyDay, StudyTime};

        let dir = tempfile::tempdir().unwrap();
        let policy = DailyLearning::default();
        let dataset: DatasetId = "games".into();
        let mut store = SqliteStore::create(
            dir.path().join("games.sqlite"),
            dataset.clone(),
            tempo_chess::demo_catalog(),
            policy.settings(),
        )
        .unwrap();
        let before = store.snapshot().unwrap();
        let target = TargetId::from("r1:p1");
        let version = before.catalog.items[&target].version;
        let mut prior = before.learning[&target].clone();
        let mut games = [comparison("first", None), comparison("second", None)];
        for entry in &mut games {
            let input = AssessmentInput {
                source: AssessmentSource::Game {
                    source: entry.game.source.clone(),
                    game_id: entry.game.id.clone(),
                    played_date: None,
                    started_at_ms: entry.game.start_ms,
                    ended_at_ms: entry.game.end_ms,
                },
                grade: Grade::Again,
                accepted_at: StudyTime {
                    day: StudyDay(1),
                    unix_ms: 86_400_000,
                },
            };
            let transition =
                DailyLearning::assess_game(&policy, &prior, &input, version, None).unwrap();
            prior = transition.state.clone();
            let mut record = crate::AssessmentRecord {
                key: crate::AssessmentKey::External {
                    provider: "placeholder".into(),
                    event_id: "placeholder".into(),
                },
                dataset: dataset.clone(),
                target: target.clone(),
                content_version: version,
                input,
                transition,
            };
            record.key = failure_key(entry, &record).assessment_key();
            entry.failures.push(record);
        }
        let commit = |writes| Commit {
            expected_revision: before.revision,
            expected_dataset: dataset.clone(),
            writes,
        };
        let write = |state| Write::Learning {
            target: target.clone(),
            state,
        };
        let receipt = || GameReceipt {
            id: "batch",
            fingerprint: "batch",
            report: "{}",
        };
        for writes in [
            vec![],
            vec![write(before.learning[&target].clone())],
            vec![write(games[0].failures[0].transition.state.clone())],
            vec![write(prior.clone()), write(prior.clone())],
        ] {
            assert!(matches!(
                store.commit_games(commit(writes), &games, Some(123), receipt()),
                Err(StoreError::Invalid(_))
            ));
        }
        let mut mismatched = games.clone();
        mismatched[1].failures[0].key = games[0].failures[0].key.clone();
        assert!(matches!(
            store.commit_games(
                commit(vec![write(prior.clone())]),
                &mismatched,
                Some(123),
                receipt()
            ),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(store.snapshot().unwrap(), before);
        assert!(
            store
                .stored_game(&dataset, "pgn", "first")
                .unwrap()
                .is_none()
        );
        assert!(store.game_history().unwrap().results.is_empty());
        assert_eq!(
            store.game_history().unwrap().last_successful_refresh_ms,
            None
        );
        assert!(store.game_failures(&dataset).unwrap().is_empty());
        assert!(
            store
                .game_receipt(&dataset, "batch", "batch")
                .unwrap()
                .is_none()
        );

        let accepted = store
            .commit_games(
                commit(vec![write(prior.clone())]),
                &games,
                Some(123),
                receipt(),
            )
            .unwrap();
        assert_eq!(accepted.counts.new_games, 2);
        assert_eq!(store.snapshot().unwrap().learning[&target], prior);
        assert_eq!(store.game_failures(&dataset).unwrap().len(), 2);
        let replay = store
            .commit_games(commit(vec![write(prior)]), &games, Some(123), receipt())
            .unwrap();
        assert_eq!(replay.report, accepted.report);
        assert_eq!(replay.counts.new_games, 2);
        assert_eq!(store.game_failures(&dataset).unwrap().len(), 2);
    }

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
