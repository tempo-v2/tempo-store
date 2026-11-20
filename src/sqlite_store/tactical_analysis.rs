//! SQL persistence for accepted tactical results and exact occurrence recalls.
use super::{
    database_error, game_evidence,
    load_snapshot::load_snapshot,
    load_study,
    row_values::{decode, decode_counter, encode},
    save_snapshot::save_snapshot,
};
use crate::{
    store_error::{conflict, invalid},
    transaction::stage_commit,
    *,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::BTreeSet;
use tempo_study::AttemptStatus;
use tempo_types::{AttemptId, ContentVersion, DatasetId, Revision, TargetId};

fn active(connection: &Connection, dataset: &DatasetId) -> Result<(), StoreError> {
    let current: String = connection
        .query_row(
            "SELECT dataset_id FROM store_state WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .map_err(database_error)?;
    if current != dataset.0 {
        return Err(conflict("tactical analysis dataset changed"));
    }
    Ok(())
}

fn status_name(status: TacticalAnalysisStatus) -> &'static str {
    match status {
        TacticalAnalysisStatus::Complete => "complete",
        TacticalAnalysisStatus::Failed => "failed",
        TacticalAnalysisStatus::TimedOut => "timed_out",
        TacticalAnalysisStatus::Unsupported => "unsupported",
    }
}

pub(super) fn accept(
    connection: &mut Connection,
    commit: Commit,
    analysis: &TacticalAnalysis,
) -> Result<TacticalAcceptance, StoreError> {
    if [
        &analysis.source,
        &analysis.game_id,
        &analysis.learner,
        &analysis.detector,
        &analysis.analysis_version,
    ]
    .iter()
    .any(|value| value.trim().is_empty())
        || analysis.learner != analysis.learner.trim().to_lowercase()
        || commit
            .writes
            .iter()
            .any(|write| !matches!(write, Write::Catalog(_) | Write::Learning { .. }))
    {
        return Err(invalid(
            "expected an addressed tactical analysis and content/learning writes",
        ));
    }
    if analysis.status != TacticalAnalysisStatus::Complete
        && (!analysis.observations.is_empty()
            || !commit.writes.is_empty()
            || analysis.decisions_scanned != 0)
    {
        return Err(invalid(
            "failed analysis cannot apply observations or training",
        ));
    }
    let mut keys = BTreeSet::new();
    for observation in &analysis.observations {
        if observation.motif.is_empty()
            || observation.outcome.is_empty()
            || !keys.insert((observation.ply, &observation.motif))
            || !serde_json::from_str::<serde_json::Value>(&observation.details_json)
                .is_ok_and(|value| value.is_object())
        {
            return Err(invalid("invalid or duplicate tactical observation"));
        }
    }
    // Acceptance time is delivery metadata, not part of the immutable analysis result.
    let digest = blake3::hash(
        encode(&(
            analysis.decisions_scanned,
            analysis.status,
            &analysis.error,
            &analysis.observations,
        ))?
        .as_bytes(),
    )
    .to_hex()
    .to_string();
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    let dataset = &commit.expected_dataset;
    active(&tx, dataset)?;
    let prior: Option<(String, String)> = tx.query_row(
        "SELECT payload_fingerprint,acceptance FROM tactical_analyses WHERE
         (dataset_id,source,game_id,learner,detector,analysis_version,game_fingerprint)=(?1,?2,?3,?4,?5,?6,?7)",
        params![dataset.0,analysis.source,analysis.game_id,analysis.learner,analysis.detector,
            analysis.analysis_version,analysis.game_fingerprint], |r| Ok((r.get(0)?,r.get(1)?))
    ).optional().map_err(database_error)?;
    if let Some((fingerprint, payload)) = prior {
        if fingerprint != digest {
            return Err(conflict("analysis identity reused for different results"));
        }
        let mut saved: TacticalAcceptance = decode(&payload)?;
        saved.replayed = true;
        saved.new_request_targets.clear();
        return Ok(saved);
    }
    let game: Option<(String, String, String)> = tx
        .query_row(
            "SELECT coalesce(mainline_signature,''),lower(white),lower(black) FROM games
         WHERE (dataset_id,source,id)=(?1,?2,?3)",
            params![dataset.0, analysis.source, analysis.game_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(database_error)?;
    let Some((fingerprint, white, black)) = game else {
        return Err(invalid("analysis game is absent"));
    };
    if fingerprint != analysis.game_fingerprint
        || (white == analysis.learner) == (black == analysis.learner)
    {
        return Err(conflict(
            "analysis no longer matches the accepted game and learner",
        ));
    }
    let previous = load_snapshot(&tx)?;
    let next = stage_commit(&previous, commit)?;
    if previous.active_attempt != next.active_attempt {
        return Err(invalid("analysis must preserve the active Study attempt"));
    }
    save_snapshot(&tx, Some(&previous), &next)?;
    let mut acceptance = TacticalAcceptance {
        revision: next.revision,
        replayed: false,
        new_observations: 0,
        new_requests: 0,
        new_request_targets: Vec::new(),
    };
    if analysis.status == TacticalAnalysisStatus::Complete {
        // Retain audit rows while letting a complete scan replace the current
        // findings, including a changed game under the same detector version.
        tx.execute(
            "UPDATE tactical_observations SET current=0 WHERE
            (dataset_id,source,game_id,learner,detector)=(?1,?2,?3,?4,?5)",
            params![
                next.dataset.0,
                analysis.source,
                analysis.game_id,
                analysis.learner,
                analysis.detector
            ],
        )
        .map_err(database_error)?;
    }
    for observation in &analysis.observations {
        let inserted = tx.execute(
            "INSERT INTO tactical_observations(dataset_id,source,game_id,learner,detector,ply,motif,
                analysis_version,outcome,details_json,observed_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(dataset_id,source,game_id,learner,detector,ply,motif) DO NOTHING",
            params![next.dataset.0,analysis.source,analysis.game_id,analysis.learner,analysis.detector,
                observation.ply,observation.motif,analysis.analysis_version,observation.outcome,
                observation.details_json,analysis.analyzed_at_ms]
        ).map_err(database_error)?;
        acceptance.new_observations += inserted;
        let id: u64 = tx
            .query_row(
                "SELECT id FROM tactical_observations WHERE
             (dataset_id,source,game_id,learner,detector,ply,motif)=(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    next.dataset.0,
                    analysis.source,
                    analysis.game_id,
                    analysis.learner,
                    analysis.detector,
                    observation.ply,
                    observation.motif
                ],
                |r| r.get(0),
            )
            .map_err(database_error)?;
        tx.execute("UPDATE tactical_observations SET analysis_version=?2,outcome=?3,details_json=?4,current=1 WHERE id=?1",
            params![id,analysis.analysis_version,observation.outcome,observation.details_json]).map_err(database_error)?;
        if let Some(puzzle) = &observation.train {
            let item = next
                .catalog
                .items
                .get(&puzzle.target)
                .filter(|item| {
                    item.active()
                        && !item.definition.is_repertoire()
                        && item.version == puzzle.version
                })
                .ok_or_else(|| {
                    invalid("training request needs an active current-version tactic")
                })?;
            if !next.learning[&item.target].introduced {
                return Err(invalid(
                    "training request requires Learning's introduction transition",
                ));
            }
            let inserted_request = tx.execute(
                "INSERT INTO tactical_requests(observation_id,dataset_id,target_id,version,accepted_at_ms)
                 VALUES(?1,?2,?3,?4,?5) ON CONFLICT(observation_id) DO NOTHING",
                params![id,next.dataset.0,puzzle.target.0,puzzle.version.0.to_be_bytes().as_slice(),analysis.analyzed_at_ms]
            ).map_err(database_error)?;
            acceptance.new_requests += inserted_request;
            if inserted_request != 0 {
                acceptance.new_request_targets.push(puzzle.target.clone());
            }
            // An unissued request may follow a revised accepted solution. An issued
            // or fulfilled request retains the exact content it was bound to.
            tx.execute(
                "UPDATE tactical_requests SET target_id=?2,version=?3,cancelled=0
                WHERE observation_id=?1 AND attempt_id IS NULL AND fulfilled=0",
                params![
                    id,
                    puzzle.target.0,
                    puzzle.version.0.to_be_bytes().as_slice()
                ],
            )
            .map_err(database_error)?;
        } else {
            tx.execute(
                "UPDATE tactical_requests SET cancelled=1 WHERE observation_id=?1
                AND attempt_id IS NULL AND fulfilled=0",
                [id],
            )
            .map_err(database_error)?;
        }
    }
    if analysis.status == TacticalAnalysisStatus::Complete {
        // A complete revised scan can retract an old finding. Failed scans cannot.
        tx.execute(
            "UPDATE tactical_requests SET cancelled=1 WHERE attempt_id IS NULL AND fulfilled=0
            AND observation_id IN (SELECT id FROM tactical_observations WHERE
                (dataset_id,source,game_id,learner,detector)=(?1,?2,?3,?4,?5) AND current=0)",
            params![
                next.dataset.0,
                analysis.source,
                analysis.game_id,
                analysis.learner,
                analysis.detector
            ],
        )
        .map_err(database_error)?;
    }
    acceptance.new_request_targets.sort();
    acceptance.new_request_targets.dedup();
    tx.execute("INSERT INTO tactical_analyses(dataset_id,source,game_id,learner,detector,analysis_version,
        game_fingerprint,status,analyzed_at_ms,decisions_scanned,payload_fingerprint,error,acceptance)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![next.dataset.0,analysis.source,analysis.game_id,analysis.learner,analysis.detector,
            analysis.analysis_version,analysis.game_fingerprint,status_name(analysis.status),
            analysis.analyzed_at_ms,analysis.decisions_scanned,digest,analysis.error,encode(&acceptance)?]
    ).map_err(database_error)?;
    tx.commit().map_err(database_error)?;
    Ok(acceptance)
}

pub(super) fn pending_games(
    connection: &Connection,
    dataset: &DatasetId,
    detector: &str,
    version: &str,
    after: Option<&TacticalGameCursor>,
    limit: usize,
) -> Result<Vec<TacticalGame>, StoreError> {
    let tx = Transaction::new_unchecked(connection, TransactionBehavior::Deferred)
        .map_err(database_error)?;
    active(&tx, dataset)?;
    let mut query = tx.prepare(
        "WITH owners AS (SELECT source,game_id,lower(json_extract(summary,'$.learner')) AS learner,
            max(ordinal) AS ordinal FROM game_comparisons WHERE dataset_id=?1 GROUP BY source,game_id,learner)
         SELECT g.source,g.id,g.url,g.white,g.black,g.start_ms,g.end_ms,g.fetched_ms,g.pgn,
            coalesce(g.mainline_signature,''),g.played_date_ms,o.learner,o.ordinal
         FROM owners o JOIN games g ON (g.dataset_id,g.source,g.id)=(?1,o.source,o.game_id)
         WHERE o.learner IS NOT NULL AND (lower(g.white)=o.learner)!=(lower(g.black)=o.learner)
           AND (?4 IS NULL OR o.ordinal<?4)
           AND NOT EXISTS(SELECT 1 FROM tactical_analyses a WHERE
             (a.dataset_id,a.source,a.game_id,a.learner,a.detector,a.analysis_version,a.game_fingerprint)=
             (g.dataset_id,g.source,g.id,o.learner,?2,?3,coalesce(g.mainline_signature,'')))
         ORDER BY o.ordinal DESC LIMIT ?5"
    ).map_err(database_error)?;
    query
        .query_map(
            params![
                dataset.0,
                detector,
                version,
                after.map(|cursor| cursor.ordinal),
                limit.clamp(1, 256)
            ],
            |r| {
                let game = game_evidence::game_row(r)?;
                let learner: String = r.get(11)?;
                Ok(TacticalGame {
                    cursor: TacticalGameCursor {
                        ordinal: r.get(12)?,
                        source: game.source.clone(),
                        game_id: game.id.clone(),
                        learner: learner.clone(),
                    },
                    game,
                    learner,
                })
            },
        )
        .map_err(database_error)?
        .collect::<Result<_, _>>()
        .map_err(database_error)
}

pub(super) fn has_pending_request_for_attempt(
    connection: &Connection,
    dataset: &DatasetId,
    attempt: &AttemptId,
) -> Result<bool, StoreError> {
    let tx = Transaction::new_unchecked(connection, TransactionBehavior::Deferred)
        .map_err(database_error)?;
    active(&tx, dataset)?;
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM tactical_requests WHERE
        dataset_id=?1 AND attempt_id=?2 AND fulfilled=0 AND cancelled=0)",
        params![dataset.0, attempt.0],
        |r| r.get(0),
    )
    .map_err(database_error)
}

pub(super) fn pending_requests(
    connection: &Connection,
    dataset: &DatasetId,
) -> Result<Vec<TacticalRequest>, StoreError> {
    active(connection, dataset)?;
    let mut query = connection
        .prepare_cached(
            "SELECT r.id,r.target_id,r.version,r.accepted_at_ms,r.observation_id
        FROM tactical_requests r JOIN current_content c ON
            (c.dataset_id,c.target_id,c.version)=(r.dataset_id,r.target_id,r.version)
        WHERE r.dataset_id=?1 AND r.fulfilled=0 AND r.cancelled=0 AND r.attempt_id IS NULL
        ORDER BY r.id DESC",
        )
        .map_err(database_error)?;
    let raw = query
        .query_map([&dataset.0], |r| {
            Ok((
                r.get(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get(3)?,
                r.get(4)?,
            ))
        })
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    raw.into_iter()
        .map(|(id, target, version, accepted_at_ms, observation_id)| {
            Ok(TacticalRequest {
                id,
                target: TargetId(target),
                version: ContentVersion(decode_counter(version)?),
                accepted_at_ms,
                observation_id,
            })
        })
        .collect()
}

pub(super) fn issue(
    connection: &mut Connection,
    commit: Commit,
    scope: StudyScope<'_>,
    request_id: u64,
) -> Result<Revision, StoreError> {
    if commit
        .writes
        .iter()
        .any(|write| !matches!(write, Write::Issue(_) | Write::Active(_)))
    {
        return Err(invalid(
            "immediate issuance may only issue and activate one attempt",
        ));
    }
    let issued: Vec<_> = commit
        .writes
        .iter()
        .filter_map(|write| {
            if let Write::Issue(record) = write {
                Some(record)
            } else {
                None
            }
        })
        .collect();
    let [record] = issued.as_slice() else {
        return Err(invalid("immediate recall requires exactly one new attempt"));
    };
    if !matches!(record.mode, ActivityMode::Test | ActivityMode::Practice) {
        return Err(invalid("immediate recall must use blind Test or Practice"));
    }
    let record = (*record).clone();
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    let state = load_study::load_study(&tx, scope)?;
    if state.active_attempt.is_some() {
        return Err(conflict(
            "finish the active attempt before immediate recall",
        ));
    }
    let previous = load_study::staging_image(state);
    let next = stage_commit(&previous, commit)?;
    if next.active_attempt.as_ref() != Some(&record.attempt.id) {
        return Err(invalid("immediate attempt must become active"));
    }
    save_snapshot(&tx, Some(&previous), &next)?;
    let changed = tx
        .execute(
            "UPDATE tactical_requests SET attempt_id=?2 WHERE id=?1 AND dataset_id=?3
        AND target_id=?4 AND version=?5 AND fulfilled=0 AND cancelled=0 AND attempt_id IS NULL",
            params![
                request_id,
                record.attempt.id.0,
                next.dataset.0,
                record.attempt.target.0,
                record.attempt.content_version.0.to_be_bytes().as_slice()
            ],
        )
        .map_err(database_error)?;
    if changed != 1 {
        return Err(conflict("immediate recall request changed before issuance"));
    }
    tx.commit().map_err(database_error)?;
    Ok(next.revision)
}

pub(super) fn settle_attempts(
    tx: &Transaction<'_>,
    previous: &Snapshot,
    next: &Snapshot,
) -> Result<(), StoreError> {
    for (id, record) in &next.attempts {
        if previous.attempts.get(id) == Some(record) {
            continue;
        }
        match record.attempt.status {
            AttemptStatus::Complete => {
                tx.execute(
                    "UPDATE tactical_requests SET fulfilled=1
                WHERE dataset_id=?1 AND attempt_id=?2 AND fulfilled=0",
                    params![next.dataset.0, id.0],
                )
                .map_err(database_error)?;
            }
            AttemptStatus::Abandoned => {
                tx.execute(
                    "UPDATE tactical_requests SET attempt_id=NULL
                WHERE dataset_id=?1 AND attempt_id=?2 AND fulfilled=0",
                    params![next.dataset.0, id.0],
                )
                .map_err(database_error)?;
            }
            AttemptStatus::Active => {}
        }
    }
    Ok(())
}

pub(super) fn statistics(
    connection: &Connection,
    dataset: &DatasetId,
    detector: &str,
    version: &str,
) -> Result<TacticalStatistics, StoreError> {
    let tx = Transaction::new_unchecked(connection, TransactionBehavior::Deferred)
        .map_err(database_error)?;
    active(&tx, dataset)?;
    let mut stats = TacticalStatistics::default();
    let mut query = tx
        .prepare(
            "SELECT a.status,count(*),sum(a.decisions_scanned) FROM tactical_analyses a
        JOIN games g ON (g.dataset_id,g.source,g.id)=(a.dataset_id,a.source,a.game_id)
        WHERE a.dataset_id=?1 AND a.detector=?2 AND a.analysis_version=?3
          AND a.game_fingerprint=coalesce(g.mainline_signature,'')
          AND (lower(g.white)=a.learner)!=(lower(g.black)=a.learner) GROUP BY a.status",
        )
        .map_err(database_error)?;
    let rows = query
        .query_map(params![dataset.0, detector, version], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u64>(1)?,
                r.get::<_, u64>(2)?,
            ))
        })
        .map_err(database_error)?;
    for row in rows {
        let (status, count, scanned) = row.map_err(database_error)?;
        match status.as_str() {
            "complete" => {
                stats.completed_games = count;
                stats.decisions_scanned += scanned;
            }
            "failed" => stats.failed_games = count,
            "timed_out" => stats.timed_out_games = count,
            "unsupported" => stats.unsupported_games = count,
            _ => return Err(invalid("unknown tactical status")),
        }
    }
    stats.pending_games=tx.query_row("WITH owners AS (
        SELECT source,game_id,lower(json_extract(summary,'$.learner')) AS learner FROM game_comparisons
        WHERE dataset_id=?1 GROUP BY source,game_id,learner)
        SELECT count(*) FROM owners o JOIN games g ON (g.dataset_id,g.source,g.id)=(?1,o.source,o.game_id)
        WHERE o.learner IS NOT NULL AND (lower(g.white)=o.learner)!=(lower(g.black)=o.learner)
        AND NOT EXISTS(SELECT 1 FROM tactical_analyses a WHERE
          (a.dataset_id,a.source,a.game_id,a.learner,a.detector,a.analysis_version,a.game_fingerprint)=
          (g.dataset_id,g.source,g.id,o.learner,?2,?3,coalesce(g.mainline_signature,'')))",
        params![dataset.0,detector,version],|r|r.get(0)).map_err(database_error)?;
    let mut outcomes = tx
        .prepare(
            "SELECT outcome,count(*) FROM tactical_observations WHERE
        dataset_id=?1 AND detector=?2 AND analysis_version=?3 AND current=1 GROUP BY outcome",
        )
        .map_err(database_error)?;
    stats.outcomes = outcomes
        .query_map(params![dataset.0, detector, version], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
        })
        .map_err(database_error)?
        .collect::<Result<_, _>>()
        .map_err(database_error)?;
    stats.observations = stats.outcomes.values().sum();
    stats.pending_requests=tx.query_row("SELECT count(*) FROM tactical_requests r JOIN tactical_observations o ON o.id=r.observation_id
        WHERE r.dataset_id=?1 AND o.detector=?2 AND r.fulfilled=0 AND r.cancelled=0",
        params![dataset.0,detector],|r|r.get(0)).map_err(database_error)?;
    Ok(stats)
}

pub(super) fn observations(
    connection: &Connection,
    dataset: &DatasetId,
    detector: &str,
    version: &str,
    before: Option<u64>,
    limit: usize,
) -> Result<TacticalObservationPage, StoreError> {
    let tx = Transaction::new_unchecked(connection, TransactionBehavior::Deferred)
        .map_err(database_error)?;
    active(&tx, dataset)?;
    let limit = limit.clamp(1, 256);
    let mut query=tx.prepare("SELECT o.id,o.source,o.game_id,o.learner,o.detector,o.analysis_version,o.observed_at_ms,
        o.ply,o.motif,o.outcome,o.details_json,r.id,r.target_id,r.version,coalesce(r.fulfilled,0),coalesce(r.cancelled,0)
        FROM tactical_observations o LEFT JOIN tactical_requests r ON r.observation_id=o.id
        WHERE o.dataset_id=?1 AND o.detector=?2 AND o.analysis_version=?3 AND o.current=1
          AND (?4 IS NULL OR o.id<?4) ORDER BY o.id DESC LIMIT ?5").map_err(database_error)?;
    let mut rows = query
        .query(params![dataset.0, detector, version, before, limit + 1])
        .map_err(database_error)?;
    let mut observations = Vec::new();
    while let Some(r) = rows.next().map_err(database_error)? {
        let request_id: Option<u64> = r.get(11).map_err(database_error)?;
        let target: Option<String> = r.get(12).map_err(database_error)?;
        let version: Option<Vec<u8>> = r.get(13).map_err(database_error)?;
        let cancelled: bool = r.get(15).map_err(database_error)?;
        let train = match (target, version, cancelled) {
            (Some(target), Some(version), false) => Some(TacticalPuzzleRef {
                target: TargetId(target),
                version: ContentVersion(decode_counter(version)?),
            }),
            _ => None,
        };
        observations.push(StoredTacticalObservation {
            id: r.get(0).map_err(database_error)?,
            source: r.get(1).map_err(database_error)?,
            game_id: r.get(2).map_err(database_error)?,
            learner: r.get(3).map_err(database_error)?,
            detector: r.get(4).map_err(database_error)?,
            analysis_version: r.get(5).map_err(database_error)?,
            observed_at_ms: r.get(6).map_err(database_error)?,
            observation: TacticalObservation {
                ply: r.get(7).map_err(database_error)?,
                motif: r.get(8).map_err(database_error)?,
                outcome: r.get(9).map_err(database_error)?,
                details_json: r.get(10).map_err(database_error)?,
                train,
            },
            // Cancelled unissued requests remain in the audit tables, but are
            // not a live or fulfilled puzzle link in the current report.
            request_id: request_id.filter(|_| !cancelled),
            fulfilled: !cancelled && r.get::<_, bool>(14).map_err(database_error)?,
        });
    }
    let next_before = (observations.len() > limit).then(|| observations[limit - 1].id);
    observations.truncate(limit);
    Ok(TacticalObservationPage {
        observations,
        next_before,
    })
}

#[cfg(test)]
mod tests;
