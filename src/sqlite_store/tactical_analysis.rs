//! SQL persistence for accepted tactical results and exact occurrence recalls.
const PERSONAL_DECISION_KIND: &str = tempo_chess::TacticalKind::PersonalTactics.code();

use super::{
    database_error, game_evidence, load_study,
    row_values::{decode, decode_counter, encode},
    save_snapshot::save_snapshot,
    tactical_targets,
};
use crate::{
    store_error::{conflict, invalid},
    transaction::stage_commit,
    *,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, collections::BTreeSet};
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
        TacticalAnalysisStatus::Inconclusive => "inconclusive",
        TacticalAnalysisStatus::Failed => "failed",
        TacticalAnalysisStatus::TimedOut => "timed_out",
        TacticalAnalysisStatus::Unsupported => "unsupported",
    }
}

fn has_position_findings(status: TacticalAnalysisStatus) -> bool {
    matches!(
        status,
        TacticalAnalysisStatus::Complete | TacticalAnalysisStatus::Inconclusive
    )
}

// Old receipts serialize just the flattened acceptance and use the full
// observation digest. A retained request did not adopt the new proposal.
// Replay verifies detector evidence independently of any changes in how the
// catalog resolves that proposal. Keep the original serialized field name.
#[derive(Deserialize, Serialize)]
struct TacticalAcceptanceReceipt {
    #[serde(flatten)]
    acceptance: TacticalAcceptance,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retained_findings: Vec<usize>,
    #[serde(default)]
    pinned_bindings: bool,
}

fn payload_fingerprint(
    analysis: &TacticalAnalysis,
    retained_findings: &[usize],
) -> Result<String, StoreError> {
    let mut observations = Cow::Borrowed(analysis.observations.as_slice());
    for &index in retained_findings {
        let finding = observations
            .to_mut()
            .get_mut(index)
            .ok_or_else(|| conflict("analysis identity reused for different results"))?;
        if finding.train.take().is_none() {
            return Err(conflict("analysis identity reused for different results"));
        }
    }
    // Acceptance time is delivery metadata, not part of the immutable analysis result.
    Ok(blake3::hash(
        encode(&(
            analysis.decisions_scanned,
            analysis.status,
            &analysis.error,
            &*observations,
        ))?
        .as_bytes(),
    )
    .to_hex()
    .to_string())
}

pub(super) fn accept(
    connection: &mut Connection,
    commit: TacticalCommit,
    analysis: &TacticalAnalysis,
) -> Result<TacticalAcceptance, StoreError> {
    if [
        &analysis.source,
        &analysis.game_id,
        &analysis.learner,
        &analysis.report,
        &analysis.analysis_version,
    ]
    .iter()
    .any(|value| value.trim().is_empty())
        || analysis.learner != analysis.learner.trim().to_lowercase()
    {
        return Err(invalid("expected an addressed tactical analysis"));
    }
    if !has_position_findings(analysis.status)
        && (!analysis.observations.is_empty()
            || !commit.targets.is_empty()
            || analysis.decisions_scanned != 0)
    {
        return Err(invalid(
            "failed analysis cannot apply observations or training",
        ));
    }
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    let dataset = &commit.expected_dataset;
    let (current_dataset, current_revision) = tactical_targets::revision(&tx)?;
    if &current_dataset != dataset {
        return Err(conflict("tactical analysis dataset changed"));
    }
    let position_id = analysis.position.map_or(0, |run| run.id);
    let deeper = analysis.position.is_some_and(|run| run.deeper);
    let ply = analysis.position.map(|run| run.ply);
    let occurrence_scope = if ply.is_some() {
        "o.ply=?6"
    } else {
        "?6 IS NULL"
    };
    if let Some(ply) = ply
        && analysis
            .observations
            .iter()
            .any(|observation| observation.ply != ply)
    {
        return Err(invalid("analysis result addresses another position"));
    }
    let prior: Option<(String, String)> = tx.query_row(
        "SELECT payload_fingerprint,acceptance FROM tactical_analyses WHERE
         (dataset_id,source,game_id,learner,report,analysis_version,game_fingerprint,position_id,deeper)=(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![dataset.0,analysis.source,analysis.game_id,analysis.learner,analysis.report,
            analysis.analysis_version,analysis.game_fingerprint,position_id,deeper], |r| Ok((r.get(0)?,r.get(1)?))
    ).optional().map_err(database_error)?;
    if let Some((fingerprint, payload)) = prior {
        let receipt: TacticalAcceptanceReceipt = decode(&payload)?;
        if fingerprint != payload_fingerprint(analysis, &receipt.retained_findings)? {
            // Older application versions omitted the proposal when a Decision's
            // issued puzzle differed. Reconstruct that digest shape only for
            // old receipts and still verify every detector-evidence field.
            let mut legacy_omissions = receipt.retained_findings;
            if receipt.pinned_bindings {
                return Err(conflict("analysis identity reused for different results"));
            }
            for (index, observation) in analysis.observations.iter().enumerate() {
                if let Some(proposal) = &observation.train
                    && observation.request_scope == TacticalRequestScope::Decision
                    && binding_for(&tx, dataset, analysis, observation)?
                        .is_some_and(|binding| binding.puzzle != *proposal)
                {
                    legacy_omissions.push(index);
                }
            }
            if fingerprint != payload_fingerprint(analysis, &legacy_omissions)? {
                return Err(conflict("analysis identity reused for different results"));
            }
        }
        let mut saved = receipt.acceptance;
        saved.replayed = true;
        saved.new_request_targets.clear();
        return Ok(saved);
    }
    if current_revision != commit.expected_revision {
        return Err(conflict("tactical analysis basis changed"));
    }
    let mut keys = BTreeSet::new();
    let mut proposed_at_ply = std::collections::BTreeMap::new();
    for observation in &analysis.observations {
        let producer = observation_producer(analysis, observation);
        if observation.motif.is_empty()
            || observation.outcome.is_empty()
            || !keys.insert((observation.ply, &observation.motif, producer))
            || !serde_json::from_str::<serde_json::Value>(&observation.details_json)
                .is_ok_and(|value| value.is_object())
        {
            return Err(invalid("invalid or duplicate tactical observation"));
        }
        if observation.train.is_some() {
            let decision = observation.request_scope == TacticalRequestScope::Decision;
            if proposed_at_ply
                .insert(observation.ply, decision)
                .is_some_and(|prior_decision| decision || prior_decision)
            {
                return Err(invalid(
                    "overlapping training proposals for a tactical decision",
                ));
            }
        }
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
    if let Some(run) = analysis.position {
        let analyses: String = tx
            .query_row(
                "SELECT analyses FROM tactical_positions WHERE id=?1",
                [run.id],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        let members: Vec<tempo_chess::DetectorIdentity> = decode(&analyses)?;
        if analysis.observations.iter().any(|row| {
            row.producer
                .as_ref()
                .is_none_or(|producer| !members.contains(producer))
        }) {
            return Err(invalid(
                "result producer is outside the queued analysis set",
            ));
        }
        let changed = tx.execute(
            "UPDATE tactical_positions SET status=?1 WHERE id=?2 AND deeper=?3 AND ply=?4
                AND status='pending' AND game_fingerprint=?5 AND analysis_version=?6 AND enrollment_id IN (
                    SELECT id FROM tactical_enrollment WHERE dataset_id=?7 AND source=?8 AND game_id=?9 AND learner=?10
                )",
            params![if analysis.status == TacticalAnalysisStatus::Complete { "complete" } else { "inconclusive" },
                run.id, run.deeper, run.ply, analysis.game_fingerprint, analysis.analysis_version,
                dataset.0, analysis.source, analysis.game_id, analysis.learner],
        ).map_err(database_error)?;
        if changed != 1 {
            return Err(conflict("position is no longer pending"));
        }
    }
    let mut bindings = Vec::with_capacity(analysis.observations.len());
    let mut new_targets = BTreeSet::new();
    for observation in &analysis.observations {
        let binding = if let Some(proposal) = &observation.train {
            let binding = binding_for(&tx, dataset, analysis, observation)?;
            if binding.is_none() {
                new_targets.insert(&proposal.target);
            }
            binding
        } else {
            None
        };
        bindings.push(binding);
    }
    let mut changed_content = false;
    for (target, change) in &commit.targets {
        if !new_targets.contains(target) {
            return Err(conflict(
                "tactical writes require a new training binding for their target",
            ));
        }
        changed_content |= tactical_targets::apply_change(&tx, dataset, target, change)?;
    }
    // Preserve the Study revision fence used for acceptances carrying proposals.
    let revision = if analysis.observations.iter().any(|row| row.train.is_some()) {
        let revision = Revision(
            current_revision
                .0
                .checked_add(1)
                .ok_or_else(|| invalid("store revision exhausted"))?,
        );
        tx.execute(
            "UPDATE store_state SET revision=?1,content_revision=content_revision+?2 WHERE singleton=1",
            params![revision.0.to_be_bytes().as_slice(), changed_content],
        ).map_err(database_error)?;
        revision
    } else {
        current_revision
    };
    let mut acceptance = TacticalAcceptance {
        revision,
        replayed: false,
        new_observations: 0,
        new_requests: 0,
        new_request_targets: Vec::new(),
    };
    if has_position_findings(analysis.status) {
        // Retain audit rows while letting a complete scan replace the current
        // findings, including a changed game under the same report version.
        tx.execute(
            &format!("UPDATE tactical_observations AS o SET current=0 WHERE
            (dataset_id,source,game_id,learner,report)=(?1,?2,?3,?4,?5) AND current=1 AND {occurrence_scope}"),
            params![
                dataset.0,
                analysis.source,
                analysis.game_id,
                analysis.learner,
                analysis.report,
                ply
            ],
        )
        .map_err(database_error)?;
    }
    let mut preserved_requests = BTreeSet::new();
    let mut retained_findings = Vec::new();
    for (index, (observation, binding)) in analysis.observations.iter().zip(bindings).enumerate() {
        let producer_id = observation
            .producer
            .as_ref()
            .map_or(analysis.report.as_str(), |producer| producer.id.as_str());
        let producer = observation.producer.as_ref().map(encode).transpose()?;
        let inserted = tx.execute(
            "INSERT INTO tactical_observations(dataset_id,source,game_id,learner,report,ply,motif,
                analysis_version,outcome,details_json,observed_at_ms,producer_id,producer) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(dataset_id,source,game_id,learner,report,ply,motif,producer_id) DO NOTHING",
            params![dataset.0,analysis.source,analysis.game_id,analysis.learner,analysis.report,
                observation.ply,observation.motif,analysis.analysis_version,observation.outcome,
                observation.details_json,analysis.analyzed_at_ms,producer_id,producer]
        ).map_err(database_error)?;
        acceptance.new_observations += inserted;
        let id: u64 = tx
            .query_row(
                "SELECT id FROM tactical_observations WHERE
             (dataset_id,source,game_id,learner,report,ply,motif,producer_id)=(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    dataset.0,
                    analysis.source,
                    analysis.game_id,
                    analysis.learner,
                    analysis.report,
                    observation.ply,
                    observation.motif, producer_id
                ],
                |r| r.get(0),
            )
            .map_err(database_error)?;
        if inserted == 0 {
            tx.execute("UPDATE tactical_observations SET analysis_version=?2,outcome=?3,details_json=?4,producer=?5,current=1 WHERE id=?1",
                params![id,analysis.analysis_version,observation.outcome,observation.details_json,producer]).map_err(database_error)?;
        }
        if let Some(binding) = binding {
            // A newer report may present a different answer. Preserve the
            // original observation address and cancellation state, as well as
            // the accepted target/version/attempt.
            let superseded = observation.request_scope == TacticalRequestScope::Decision
                && binding.issued_or_fulfilled
                && observation.train.as_ref() != Some(&binding.puzzle);
            if superseded && analysis.status == TacticalAnalysisStatus::Complete {
                tx.execute(
                    "UPDATE tactical_requests SET cancelled=1 WHERE id=?1 AND fulfilled=0 AND cancelled=0",
                    [binding.id],
                )
                .map_err(database_error)?;
            }
            preserved_requests.insert(binding.id);
            retained_findings.push(index);
            continue;
        }
        if let Some(puzzle) = &observation.train {
            tactical_targets::require_target(&tx, dataset, &puzzle.target, puzzle.version)?;
            let inserted_request = tx.execute(
                "INSERT INTO tactical_requests(observation_id,dataset_id,target_id,version,accepted_at_ms)
                 VALUES(?1,?2,?3,?4,?5) ON CONFLICT(observation_id) DO NOTHING",
                params![id,dataset.0,puzzle.target.0,puzzle.version.0.to_be_bytes().as_slice(),analysis.analyzed_at_ms]
            ).map_err(database_error)?;
            acceptance.new_requests += inserted_request;
            if inserted_request != 0 {
                acceptance.new_request_targets.push(puzzle.target.clone());
            }
        } else if analysis.status == TacticalAnalysisStatus::Complete {
            tx.execute(
                "UPDATE tactical_requests SET cancelled=1 WHERE observation_id=?1
                AND fulfilled=0 AND cancelled=0",
                [id],
            )
            .map_err(database_error)?;
        }
    }
    if analysis.status == TacticalAnalysisStatus::Complete {
        // Retiring future issuance never changes the issued attempt or its receipts.
        let mut prior = tx
            .prepare(&format!(
                "SELECT r.id FROM tactical_observations o
             CROSS JOIN tactical_requests r ON o.id=r.observation_id
             WHERE (o.dataset_id,o.source,o.game_id,o.learner,o.report)=(?1,?2,?3,?4,?5)
               AND o.current=0 AND {occurrence_scope} AND r.fulfilled=0 AND r.cancelled=0"
            ))
            .map_err(database_error)?;
        let ids = prior
            .query_map(
                params![
                    dataset.0,
                    analysis.source,
                    analysis.game_id,
                    analysis.learner,
                    analysis.report,
                    ply
                ],
                |row| row.get::<_, u64>(0),
            )
            .map_err(database_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        for request in ids {
            if !preserved_requests.contains(&request) {
                tx.execute(
                    "UPDATE tactical_requests SET cancelled=1 WHERE id=?1",
                    [request],
                )
                .map_err(database_error)?;
            }
        }
    }
    acceptance.new_request_targets.sort();
    acceptance.new_request_targets.dedup();
    let digest = payload_fingerprint(analysis, &retained_findings)?;
    let receipt = TacticalAcceptanceReceipt {
        acceptance,
        retained_findings,
        pinned_bindings: true,
    };
    tx.execute("INSERT INTO tactical_analyses(dataset_id,source,game_id,learner,report,analysis_version,
        game_fingerprint,status,analyzed_at_ms,decisions_scanned,payload_fingerprint,error,acceptance,position_id,deeper)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![dataset.0,analysis.source,analysis.game_id,analysis.learner,analysis.report,
            analysis.analysis_version,analysis.game_fingerprint,status_name(analysis.status),
            analysis.analyzed_at_ms,analysis.decisions_scanned,digest,analysis.error,encode(&receipt)?,position_id,deeper]
    ).map_err(database_error)?;
    tx.commit().map_err(database_error)?;
    Ok(receipt.acceptance)
}

pub(super) fn pending_games(
    connection: &Connection,
    dataset: &DatasetId,
    report: &str,
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
             (a.dataset_id,a.source,a.game_id,a.learner,a.report,a.analysis_version,a.game_fingerprint)=
             (g.dataset_id,g.source,g.id,o.learner,?2,?3,coalesce(g.mainline_signature,'')))
         ORDER BY o.ordinal DESC LIMIT ?5"
    ).map_err(database_error)?;
    query
        .query_map(
            params![
                dataset.0,
                report,
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
        dataset_id=?1 AND attempt_id=?2 AND fulfilled=0)",
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

pub(super) fn issue_daily(
    connection: &mut Connection,
    commit: Commit,
    scope: StudyScope<'_>,
    immediate_request: Option<u64>,
    ordering: StudyOrderingWrite,
) -> Result<Revision, StoreError> {
    let record = commit.daily_issued_attempt()?;
    if immediate_request.is_some()
        && !matches!(record.mode, ActivityMode::Test | ActivityMode::Practice)
    {
        return Err(invalid("immediate recall must use blind Test or Practice"));
    }
    let binding = immediate_request.map(|request| (request, record.attempt.id.clone()));
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    let state = load_study::load_study(&tx, scope)?;
    if state.active_attempt.is_some() {
        return Err(conflict("finish the active attempt before daily issuance"));
    }
    let previous = load_study::staging_image(state);
    let next = stage_commit(&previous, commit)?;
    save_snapshot(&tx, Some(&previous), &next)?;
    if let Some((request_id, id)) = binding {
        let record = &next.attempts[&id];
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
    }
    let ordering_sql = match ordering {
        StudyOrderingWrite::Preserve => None,
        StudyOrderingWrite::OweRegularReview => {
            Some("INSERT INTO tactical_study_turn VALUES(?1) ON CONFLICT DO NOTHING")
        }
        StudyOrderingWrite::SatisfyRegularReview => {
            Some("DELETE FROM tactical_study_turn WHERE dataset_id=?1")
        }
    };
    if let Some(sql) = ordering_sql {
        tx.execute(sql, [&next.dataset.0]).map_err(database_error)?;
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
                    "UPDATE tactical_requests SET fulfilled=1,cancelled=0
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

const RETRYABLE_POSITIONS: &str = "
    p.status='inconclusive' AND p.deeper=0 AND p.analysis_version=?2
    AND EXISTS(
        SELECT 1 FROM tactical_enrollment e
        JOIN games g ON (g.dataset_id,g.source,g.id)=(e.dataset_id,e.source,e.game_id)
        WHERE e.id=p.enrollment_id AND e.dataset_id=?1
          AND p.game_fingerprint=coalesce(g.mainline_signature,'')
    )";

pub(super) fn statistics(
    connection: &Connection,
    dataset: &DatasetId,
    report: &str,
    version: &str,
) -> Result<TacticalStatistics, StoreError> {
    let tx = Transaction::new_unchecked(connection, TransactionBehavior::Deferred)
        .map_err(database_error)?;
    active(&tx, dataset)?;
    let mut stats = TacticalStatistics::default();
    let mut query = tx
        .prepare(
            "SELECT CASE WHEN a.status='inconclusive' THEN 'complete' ELSE a.status END AS result,count(DISTINCT CASE WHEN a.status IN ('failed','unsupported','timed_out')
                OR NOT EXISTS(
                    SELECT 1 FROM tactical_enrollment enrolled JOIN tactical_positions todo ON todo.enrollment_id=enrolled.id
                    WHERE (enrolled.dataset_id,enrolled.source,enrolled.game_id,enrolled.learner)=
                          (a.dataset_id,a.source,a.game_id,a.learner)
                      AND (todo.analysis_version,todo.game_fingerprint)=(a.analysis_version,a.game_fingerprint)
                      AND todo.status='pending'
                ) THEN a.source || char(0) || a.game_id || char(0) || a.learner END),sum(a.decisions_scanned) FROM tactical_analyses a
        JOIN games g ON (g.dataset_id,g.source,g.id)=(a.dataset_id,a.source,a.game_id)
        WHERE a.dataset_id=?1 AND a.report=?2 AND a.analysis_version=?3
          AND (a.position_id=0 OR a.deeper=(SELECT deeper FROM tactical_positions WHERE id=a.position_id))
          AND a.game_fingerprint=coalesce(g.mainline_signature,'')
          AND (lower(g.white)=a.learner)!=(lower(g.black)=a.learner) GROUP BY result",
        )
        .map_err(database_error)?;
    let rows = query
        .query_map(params![dataset.0, report, version], |r| {
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
    stats.pending_games = tx
        .query_row(
            "SELECT count(*) FROM tactical_enrollment e WHERE e.dataset_id=?1 AND
           (e.expanded<>?2 OR EXISTS(SELECT 1 FROM tactical_positions p
                  WHERE p.enrollment_id=e.id AND p.analysis_version=?2 AND p.status='pending'))",
            params![dataset.0, version],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    stats.pending_positions = tx.query_row(
        "SELECT count(*) FROM tactical_positions p JOIN tactical_enrollment e ON e.id=p.enrollment_id
         WHERE e.dataset_id=?1 AND p.analysis_version=?2 AND p.status='pending'",
        params![dataset.0, version], |row| row.get(0),
    ).map_err(database_error)?;
    stats.inconclusive_positions = tx.query_row(
        "SELECT count(*) FROM tactical_positions p JOIN tactical_enrollment e ON e.id=p.enrollment_id
         WHERE e.dataset_id=?1 AND p.analysis_version=?2 AND p.status='inconclusive'",
        params![dataset.0, version], |row| row.get(0),
    ).map_err(database_error)?;
    stats.retryable_positions = tx
        .query_row(
            &format!("SELECT count(*) FROM tactical_positions p WHERE {RETRYABLE_POSITIONS}"),
            params![dataset.0, version],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    let mut outcomes = tx
        .prepare(
            "SELECT outcome,count(*) FROM tactical_observations WHERE
        dataset_id=?1 AND report=?2 AND analysis_version=?3 AND current=1
        AND motif<>?4 GROUP BY outcome",
        )
        .map_err(database_error)?;
    stats.outcomes = outcomes
        .query_map(
            params![dataset.0, report, version, PERSONAL_DECISION_KIND],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)),
        )
        .map_err(database_error)?
        .collect::<Result<_, _>>()
        .map_err(database_error)?;
    stats.observations = stats.outcomes.values().sum();
    stats.pending_requests=tx.query_row("SELECT count(*) FROM tactical_requests r JOIN tactical_observations o ON o.id=r.observation_id
        WHERE r.dataset_id=?1 AND o.report=?2 AND r.fulfilled=0 AND r.cancelled=0",
        params![dataset.0,report],|r|r.get(0)).map_err(database_error)?;
    Ok(stats)
}

pub(super) fn observations(
    connection: &Connection,
    dataset: &DatasetId,
    report: &str,
    version: &str,
    before: Option<u64>,
    limit: usize,
) -> Result<TacticalObservationPage, StoreError> {
    let tx = Transaction::new_unchecked(connection, TransactionBehavior::Deferred)
        .map_err(database_error)?;
    active(&tx, dataset)?;
    let limit = limit.clamp(1, 256);
    let mut query=tx.prepare("SELECT o.id,o.source,o.game_id,o.learner,o.report,o.analysis_version,o.observed_at_ms,
        o.ply,o.motif,o.outcome,o.details_json,r.id,r.target_id,r.version,coalesce(r.fulfilled,0),coalesce(r.cancelled,0),o.producer
        FROM tactical_observations o LEFT JOIN tactical_requests r ON r.id=coalesce(
            (SELECT id FROM tactical_requests WHERE observation_id=o.id),
            (SELECT request.id FROM tactical_requests request
             JOIN tactical_observations basis ON basis.id=request.observation_id
             WHERE (basis.dataset_id,basis.source,basis.game_id,basis.learner,basis.report,basis.ply)
                   =(o.dataset_id,o.source,o.game_id,o.learner,o.report,o.ply)
               AND basis.motif=?6
             ORDER BY (request.attempt_id IS NOT NULL OR request.fulfilled=1) DESC,request.id LIMIT 1)
        )
        WHERE o.dataset_id=?1 AND o.report=?2 AND o.analysis_version=?3 AND o.current=1
          AND o.motif<>?6
          AND (?4 IS NULL OR o.id<?4) ORDER BY o.id DESC LIMIT ?5").map_err(database_error)?;
    let mut rows = query
        .query(params![
            dataset.0,
            report,
            version,
            before,
            limit + 1,
            PERSONAL_DECISION_KIND
        ])
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
            report: r.get(4).map_err(database_error)?,
            analysis_version: r.get(5).map_err(database_error)?,
            observed_at_ms: r.get(6).map_err(database_error)?,
            observation: TacticalObservation {
                producer: r
                    .get::<_, Option<String>>(16)
                    .map_err(database_error)?
                    .as_deref()
                    .map(decode)
                    .transpose()?,
                ply: r.get(7).map_err(database_error)?,
                motif: r.get(8).map_err(database_error)?,
                outcome: r.get(9).map_err(database_error)?,
                details_json: r.get(10).map_err(database_error)?,
                train,
                request_scope: TacticalRequestScope::Finding,
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

pub(super) fn enroll(
    db: &Connection,
    dataset: &DatasetId,
    source: &str,
    game: &str,
    learner: &str,
    imported: bool,
) -> Result<(), StoreError> {
    db.execute(
        "INSERT INTO tactical_enrollment(dataset_id,source,game_id,learner,priority)
         SELECT dataset_id,source,id,lower(?4),?5 FROM games
         WHERE (dataset_id,source,id)=(?1,?2,?3) AND (lower(white)=lower(?4))!=(lower(black)=lower(?4))
         ON CONFLICT DO NOTHING",
        params![dataset.0, source, game, learner, imported],
    ).map_err(database_error)?;
    Ok(())
}

impl SqliteStore {
    pub fn enroll_verified_archive(&self, dataset: &DatasetId) -> Result<(), StoreError> {
        active(&self.connection, dataset)?;
        self.connection
            .execute(
                "INSERT INTO tactical_enrollment(dataset_id,source,game_id,learner,priority)
             SELECT g.dataset_id,g.source,g.id,lower(json_extract(c.summary,'$.learner')),0
             FROM games g JOIN game_comparisons c ON c.ordinal=(
                SELECT max(a.ordinal) FROM game_comparisons a
                WHERE (a.dataset_id,a.source,a.game_id)=(g.dataset_id,g.source,g.id))
             WHERE g.dataset_id=?1 AND
                (lower(g.white)=lower(json_extract(c.summary,'$.learner')))!=
                (lower(g.black)=lower(json_extract(c.summary,'$.learner')))
             ON CONFLICT DO NOTHING",
                [&dataset.0],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn queue_game_analysis(
        &self,
        dataset: &DatasetId,
        source: &str,
        game: &str,
        learner: &str,
    ) -> Result<(), StoreError> {
        active(&self.connection, dataset)?;
        let learner = learner.trim().to_lowercase();
        let verified: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM game_comparisons
             WHERE (dataset_id,source,game_id)=(?1,?2,?3) AND lower(json_extract(summary,'$.learner'))=?4)",
            params![dataset.0, source, game, learner], |row| row.get(0),
        ).map_err(database_error)?;
        if !verified {
            return Err(invalid(
                "the selected participant was not verified for this game",
            ));
        }
        enroll(&self.connection, dataset, source, game, &learner, false)
    }

    pub fn next_analysis_enrollment(
        &self,
        dataset: &DatasetId,
        version: &str,
    ) -> Result<Option<TacticalGame>, StoreError> {
        active(&self.connection, dataset)?;
        self.connection.query_row(
            "SELECT g.source,g.id,g.url,g.white,g.black,g.start_ms,g.end_ms,g.fetched_ms,g.pgn,
                 coalesce(g.mainline_signature,''),g.played_date_ms,e.learner,e.id
             FROM tactical_enrollment e JOIN games g ON (g.dataset_id,g.source,g.id)=(e.dataset_id,e.source,e.game_id)
             WHERE e.dataset_id=?1 AND e.expanded<>?2 ORDER BY e.priority DESC,e.id DESC LIMIT 1",
            params![dataset.0,version], queued_game,
        ).optional().map_err(database_error)
    }

    pub fn expand_analysis_enrollment(
        &self,
        dataset: &DatasetId,
        game: &TacticalGame,
        version: &str,
        analyses: &[tempo_chess::DetectorIdentity],
        plies: &[u32],
    ) -> Result<(), StoreError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        active(&tx, dataset)?;
        let analyses = encode(&analyses)?;
        for ply in plies {
            tx.execute(
                "INSERT INTO tactical_positions(enrollment_id,game_fingerprint,ply,analysis_version,analyses)
                 VALUES(?1,?2,?3,?4,?5) ON CONFLICT DO NOTHING",
                params![game.cursor.ordinal, game.game.mainline_signature, ply, version, analyses],
            ).map_err(database_error)?;
        }
        tx.execute(
            "UPDATE tactical_enrollment SET expanded=?3 WHERE id=?1 AND dataset_id=?2",
            params![game.cursor.ordinal, dataset.0, version],
        )
        .map_err(database_error)?;
        tx.commit().map_err(database_error)
    }

    pub fn next_position_analysis(
        &self,
        dataset: &DatasetId,
        version: &str,
        continuing: Option<i64>,
    ) -> Result<Option<TacticalPositionTask>, StoreError> {
        self.connection.query_row(
            "SELECT g.source,g.id,g.url,g.white,g.black,g.start_ms,g.end_ms,g.fetched_ms,g.pgn,
                 coalesce(g.mainline_signature,''),g.played_date_ms,e.learner,e.id,p.id,p.ply,p.deeper,p.analyses
             FROM tactical_enrollment e JOIN tactical_positions p ON p.enrollment_id=e.id
             JOIN games g ON (g.dataset_id,g.source,g.id)=(e.dataset_id,e.source,e.game_id)
             WHERE e.dataset_id=?1 AND p.analysis_version=?2 AND p.status='pending'
             AND p.game_fingerprint=coalesce(g.mainline_signature,'')
             AND e.id=(SELECT candidate.id FROM tactical_enrollment candidate
                 WHERE candidate.dataset_id=?1 AND EXISTS(
                     SELECT 1 FROM tactical_positions position
                     JOIN games source ON (source.dataset_id,source.source,source.id)=
                         (candidate.dataset_id,candidate.source,candidate.game_id)
                     WHERE position.enrollment_id=candidate.id AND position.analysis_version=?2
                         AND position.status='pending' AND position.game_fingerprint=coalesce(source.mainline_signature,'')
                 ) ORDER BY coalesce(candidate.id=?3,0) DESC,candidate.priority DESC,candidate.id DESC LIMIT 1)
             ORDER BY p.ply LIMIT 1",
            params![dataset.0,version,continuing], |row| Ok((
                queued_game(row)?,
                TacticalPositionRun { id: row.get(13)?, ply: row.get(14)?, deeper: row.get(15)? },
                row.get::<_, String>(16)?,
            )),
        ).optional().map_err(database_error)?.map(|(game, run, analyses)|
            Ok(TacticalPositionTask { game, run, analyses: decode(&analyses)? })
        ).transpose()
    }

    pub fn recent_analysis_games(
        &self,
        dataset: &DatasetId,
        version: &str,
    ) -> Result<Vec<AnalysisGameProgress>, StoreError> {
        let mut query = self.connection.prepare_cached(
            "SELECT e.source,e.game_id,g.white,g.black,
                count(p.id) FILTER(WHERE p.status='complete'),
                count(p.id) FILTER(WHERE p.status='pending'),
                count(p.id) FILTER(WHERE p.status='inconclusive'), e.expanded<>?2
             FROM tactical_enrollment e JOIN games g ON (g.dataset_id,g.source,g.id)=(e.dataset_id,e.source,e.game_id)
             LEFT JOIN tactical_positions p ON p.enrollment_id=e.id AND p.analysis_version=?2
               AND p.game_fingerprint=coalesce(g.mainline_signature,'')
             WHERE e.id IN (SELECT id FROM tactical_enrollment WHERE dataset_id=?1 ORDER BY id DESC LIMIT 30)
             GROUP BY e.id ORDER BY e.id DESC",
        ).map_err(database_error)?;
        query
            .query_map(params![dataset.0, version], |row| {
                Ok(AnalysisGameProgress {
                    source: row.get(0)?,
                    game_id: row.get(1)?,
                    white: row.get(2)?,
                    black: row.get(3)?,
                    checked: row.get(4)?,
                    pending: row.get(5)?,
                    inconclusive: row.get(6)?,
                    preparing: row.get(7)?,
                })
            })
            .map_err(database_error)?
            .collect::<Result<_, _>>()
            .map_err(database_error)
    }

    pub fn analysis_paused(&self, dataset: &DatasetId) -> Result<bool, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT paused FROM tactical_pause WHERE dataset_id=?1",
                [&dataset.0],
                |r| r.get(0),
            )
            .optional()
            .map_err(database_error)?
            .unwrap_or(false))
    }

    pub fn set_analysis_paused(&self, dataset: &DatasetId, paused: bool) -> Result<(), StoreError> {
        active(&self.connection, dataset)?;
        self.connection.execute(
            "INSERT INTO tactical_pause VALUES(?1,?2) ON CONFLICT(dataset_id) DO UPDATE SET paused=excluded.paused",
            params![dataset.0,paused],
        ).map_err(database_error)?;
        Ok(())
    }

    pub fn retry_position_analysis(
        &self,
        dataset: &DatasetId,
        version: &str,
    ) -> Result<(), StoreError> {
        active(&self.connection, dataset)?;
        self.connection.execute(
            &format!("UPDATE tactical_positions AS p SET deeper=1,status='pending' WHERE {RETRYABLE_POSITIONS}"),
            params![dataset.0, version],
        ).map_err(database_error)?;
        Ok(())
    }
}

fn queued_game(row: &rusqlite::Row<'_>) -> rusqlite::Result<TacticalGame> {
    let game = game_evidence::game_row(row)?;
    let learner: String = row.get(11)?;
    Ok(TacticalGame {
        cursor: TacticalGameCursor {
            ordinal: row.get(12)?,
            source: game.source.clone(),
            game_id: game.id.clone(),
            learner: learner.clone(),
        },
        game,
        learner,
    })
}

pub(super) fn lead_in(
    connection: &Connection,
    dataset: &DatasetId,
    request: u64,
) -> Result<Option<tempo_chess::OpponentLeadIn>, StoreError> {
    let encoded: Option<String> = connection
        .query_row(
            "SELECT json_extract(o.details_json,'$.opponent_lead_in')
         FROM tactical_requests r JOIN tactical_observations o ON o.id=r.observation_id
         WHERE r.dataset_id=?1 AND r.id=?2",
            params![dataset.0, request],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    encoded.as_deref().map(decode).transpose()
}

impl SqliteStore {
    /// Read both report epochs and verify the requested dataset in one indexed read.
    pub fn tactical_report_generation(
        &self,
        dataset: &DatasetId,
    ) -> Result<TacticalReportGeneration, StoreError> {
        self.connection
            .query_row(
                "SELECT evidence,archive FROM tactical_report_generation
                 WHERE singleton=1 AND ?1=(SELECT dataset_id FROM store_state WHERE singleton=1)",
                [&dataset.0],
                |row| {
                    Ok(TacticalReportGeneration {
                        evidence: row.get(0)?,
                        archive: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(database_error)?
            .ok_or_else(|| conflict("tactical analysis dataset changed"))
    }

    /// Capture inputs under one database snapshot. Keep projection and computation
    /// outside the callback so a reporting read holds the lock only for capture.
    pub fn with_tactical_report_snapshot<T, E>(
        &self,
        dataset: &DatasetId,
        read: impl FnOnce(&Self, TacticalReportGeneration) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<StoreError>,
    {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let generation = self.tactical_report_generation(dataset)?;
        let result = read(self, generation)?;
        transaction.commit().map_err(database_error)?;
        Ok(result)
    }

    /// Read verified players and raw games for rebuilding the archive facts.
    pub fn tactical_archive_metadata(
        &self,
        dataset: &DatasetId,
    ) -> Result<Vec<TacticalArchiveMetadata>, StoreError> {
        active(&self.connection, dataset)?;
        let mut query = self.connection.prepare(
            "WITH latest AS (
                SELECT max(ordinal) AS ordinal FROM game_comparisons WHERE dataset_id=?1 GROUP BY source,game_id
             )
             SELECT g.source,g.id,lower(json_extract(c.summary,'$.learner')),g.white,g.pgn,
                    g.start_ms,g.end_ms
             FROM latest JOIN game_comparisons c USING(ordinal)
             JOIN games g ON (g.dataset_id,g.source,g.id)=(c.dataset_id,c.source,c.game_id)
             WHERE (lower(g.white)=lower(json_extract(c.summary,'$.learner')))!=
                   (lower(g.black)=lower(json_extract(c.summary,'$.learner')))"
        ).map_err(database_error)?;
        query
            .query_map([&dataset.0], |row| {
                Ok(TacticalArchiveMetadata {
                    source: row.get(0)?,
                    id: row.get(1)?,
                    learner: row.get(2)?,
                    white: row.get(3)?,
                    pgn: row.get(4)?,
                    start_ms: row.get(5)?,
                    end_ms: row.get(6)?,
                })
            })
            .map_err(database_error)?
            .collect::<Result<_, _>>()
            .map_err(database_error)
    }

    /// Read coverage without loading archive PGNs.
    pub fn tactical_archive_coverage(
        &self,
        dataset: &DatasetId,
        report: &str,
        version: &str,
    ) -> Result<Vec<TacticalArchiveCoverage>, StoreError> {
        active(&self.connection, dataset)?;
        let mut query = self.connection.prepare(
            "WITH latest AS (
                SELECT max(ordinal) AS ordinal FROM game_comparisons WHERE dataset_id=?1 GROUP BY source,game_id
             )
             SELECT g.source,g.id,lower(json_extract(c.summary,'$.learner')),
                    coalesce(e.expanded=?2 AND NOT EXISTS (
                        SELECT 1 FROM tactical_positions p
                        WHERE p.enrollment_id=e.id AND p.analysis_version=?2
                          AND p.game_fingerprint=g.mainline_signature AND p.status<>'complete'
                          AND (p.status='pending' OR NOT EXISTS (
                              SELECT 1 FROM tactical_observations o
                              WHERE (o.dataset_id,o.source,o.game_id,o.learner,o.ply,o.analysis_version)
                                   =(e.dataset_id,e.source,e.game_id,e.learner,p.ply,p.analysis_version)
                                AND o.current=1 AND o.report=?3 AND o.motif=?4
                                AND json_extract(o.details_json,'$.assessment.analysis.status')='complete'
                                AND o.outcome<>'uncertain'
                          ))
                    ),0)
             FROM latest JOIN game_comparisons c USING(ordinal)
             JOIN games g ON (g.dataset_id,g.source,g.id)=(c.dataset_id,c.source,c.game_id)
             LEFT JOIN tactical_enrollment e ON (e.dataset_id,e.source,e.game_id,e.learner)
                  =(g.dataset_id,g.source,g.id,lower(json_extract(c.summary,'$.learner')))
             WHERE (lower(g.white)=lower(json_extract(c.summary,'$.learner')))!=
                   (lower(g.black)=lower(json_extract(c.summary,'$.learner')))"
        ).map_err(database_error)?;
        query
            .query_map(
                params![dataset.0, version, report, PERSONAL_DECISION_KIND],
                |row| {
                    Ok(TacticalArchiveCoverage {
                        source: row.get(0)?,
                        id: row.get(1)?,
                        learner: row.get(2)?,
                        fully_analyzed: row.get(3)?,
                    })
                },
            )
            .map_err(database_error)?
            .collect::<Result<_, _>>()
            .map_err(database_error)
    }

    pub fn tactical_archive(
        &self,
        dataset: &DatasetId,
        report: &str,
        version: &str,
    ) -> Result<Vec<TacticalArchiveGame>, StoreError> {
        active(&self.connection, dataset)?;
        let mut query = self.connection.prepare(
            "WITH latest AS (
                SELECT max(ordinal) AS ordinal FROM game_comparisons WHERE dataset_id=?1 GROUP BY source,game_id
             )
             SELECT g.source,g.id,lower(json_extract(c.summary,'$.learner')),g.white,g.pgn,
                    g.start_ms,g.end_ms,coalesce(e.expanded=?2 AND NOT EXISTS (
                        SELECT 1 FROM tactical_positions p
                        WHERE p.enrollment_id=e.id AND p.analysis_version=?2
                          AND p.game_fingerprint=g.mainline_signature AND p.status<>'complete'
                          AND (p.status='pending' OR NOT EXISTS (
                              SELECT 1 FROM tactical_observations o
                              WHERE (o.dataset_id,o.source,o.game_id,o.learner,o.ply,o.analysis_version)
                                   =(e.dataset_id,e.source,e.game_id,e.learner,p.ply,p.analysis_version)
                                AND o.current=1 AND o.report=?3 AND o.motif=?4
                                AND json_extract(o.details_json,'$.assessment.analysis.status')='complete'
                                AND o.outcome<>'uncertain'
                          ))
                    ),0)
             FROM latest JOIN game_comparisons c USING(ordinal)
             JOIN games g ON (g.dataset_id,g.source,g.id)=(c.dataset_id,c.source,c.game_id)
             LEFT JOIN tactical_enrollment e ON (e.dataset_id,e.source,e.game_id,e.learner)
                  =(g.dataset_id,g.source,g.id,lower(json_extract(c.summary,'$.learner')))
             WHERE (lower(g.white)=lower(json_extract(c.summary,'$.learner')))!=
                   (lower(g.black)=lower(json_extract(c.summary,'$.learner')))"
        ).map_err(database_error)?;
        query
            .query_map(
                params![dataset.0, version, report, PERSONAL_DECISION_KIND],
                |row| {
                    Ok(TacticalArchiveGame {
                        source: row.get(0)?,
                        id: row.get(1)?,
                        learner: row.get(2)?,
                        white: row.get(3)?,
                        pgn: row.get(4)?,
                        start_ms: row.get(5)?,
                        end_ms: row.get(6)?,
                        fully_analyzed: row.get(7)?,
                    })
                },
            )
            .map_err(database_error)?
            .collect::<Result<_, _>>()
            .map_err(database_error)
    }

    pub fn tactical_decisions(
        &self,
        dataset: &DatasetId,
        report: &str,
        version: &str,
    ) -> Result<Vec<TacticalDecisionRow>, StoreError> {
        active(&self.connection, dataset)?;
        let mut query = self.connection.prepare(
            "WITH latest AS (
                 SELECT source,game_id,max(ordinal) AS ordinal
                 FROM game_comparisons WHERE dataset_id=?1 GROUP BY source,game_id
             )
             SELECT o.id,o.source,o.game_id,o.ply,json_extract(details_json,'$.position'),outcome,
               (SELECT json_group_array(json_array(json_extract(value,'$.motif'),json_extract(value,'$.answer.uci')))
                FROM json_each(details_json,'$.assessment.personal.candidates')),
               (SELECT json_group_array(json_extract(value,'$.uci'))
                FROM json_each(details_json,'$.assessment.accepted_moves')), o.learner
             FROM tactical_observations o
             JOIN latest l ON (l.source,l.game_id)=(o.source,o.game_id)
             JOIN game_comparisons c ON c.ordinal=l.ordinal
             WHERE o.dataset_id=?1 AND report=?2 AND analysis_version=?3 AND current=1
               AND o.learner=lower(json_extract(c.summary,'$.learner'))
               AND o.motif=?4
             ORDER BY o.id DESC"
        ).map_err(database_error)?;
        let mut rows = query
            .query(params![dataset.0, report, version, PERSONAL_DECISION_KIND])
            .map_err(database_error)?;
        let mut decisions = vec![];
        while let Some(row) = rows.next().map_err(database_error)? {
            let outcome: String = row.get(5).map_err(database_error)?;
            decisions.push(TacticalDecisionRow {
                id: row.get(0).map_err(database_error)?,
                learner: row.get(8).map_err(database_error)?,
                source: row.get(1).map_err(database_error)?,
                game_id: row.get(2).map_err(database_error)?,
                ply: row.get(3).map_err(database_error)?,
                position: row.get(4).map_err(database_error)?,
                outcome: decode(&format!("\"{outcome}\""))?,
                candidates: decode(&row.get::<_, String>(6).map_err(database_error)?)?,
                accepted: decode(&row.get::<_, String>(7).map_err(database_error)?)?,
            });
        }
        Ok(decisions)
    }

    /// Read only the selected, current findings and their accepted training links.
    pub fn tactical_details(
        &self,
        dataset: &DatasetId,
        id: u64,
        version: &str,
    ) -> Result<Option<TacticalDecisionDetails>, StoreError> {
        active(&self.connection, dataset)?;
        self.connection.query_row(
            "SELECT o.details_json,r.id,r.fulfilled,r.cancelled,r.attempt_id IS NOT NULL
             FROM tactical_observations o LEFT JOIN tactical_requests r ON r.id=(
                 SELECT request.id FROM tactical_requests request
                 JOIN tactical_observations basis ON basis.id=request.observation_id
                 WHERE (basis.dataset_id,basis.source,basis.game_id,basis.learner,basis.report,basis.ply)
                       =(o.dataset_id,o.source,o.game_id,o.learner,o.report,o.ply)
                       AND (request.cancelled=0 OR request.attempt_id IS NOT NULL)
                 ORDER BY (request.attempt_id IS NOT NULL OR request.fulfilled=1) DESC,
                          (basis.id=?2) DESC,request.id LIMIT 1)
             WHERE o.dataset_id=?1 AND o.id=?2 AND o.analysis_version=?3 AND o.current=1
                AND o.motif=?4
                AND o.learner=(SELECT lower(json_extract(c.summary,'$.learner')) FROM game_comparisons c
                  WHERE (c.dataset_id,c.source,c.game_id)=(o.dataset_id,o.source,o.game_id)
                  ORDER BY c.ordinal DESC LIMIT 1)",
            params![dataset.0,id,version,PERSONAL_DECISION_KIND], |row| {
                let training = match row.get::<_, Option<u64>>(1)? {
                    Some(id) => {
                        let status = if row.get::<_, bool>(2)? {
                            TacticalTrainingStatus::Reviewed
                        } else if row.get::<_, bool>(3)? {
                            TacticalTrainingStatus::Superseded
                        } else if row.get::<_, bool>(4)? {
                            TacticalTrainingStatus::Issued
                        } else {
                            TacticalTrainingStatus::Queued
                        };
                        Some(TacticalTrainingLink { id, status })
                    }
                    None => None,
                };
                Ok(TacticalDecisionDetails { evidence_json: row.get(0)?, training })
            },
        ).optional().map_err(database_error)
    }
}

struct StoredTrainingBinding {
    id: u64,
    puzzle: TacticalPuzzleRef,
    issued_or_fulfilled: bool,
}

fn observation_producer<'a>(
    analysis: &'a TacticalAnalysis,
    observation: &'a TacticalObservation,
) -> &'a str {
    observation
        .producer
        .as_ref()
        .map_or(analysis.report.as_str(), |producer| producer.id.as_str())
}

fn binding_for(
    connection: &Connection,
    dataset: &DatasetId,
    analysis: &TacticalAnalysis,
    observation: &TacticalObservation,
) -> Result<Option<StoredTrainingBinding>, StoreError> {
    let producer_id = observation_producer(analysis, observation);
    let ply = observation.ply;
    let motif = &observation.motif;
    let address = match observation.request_scope {
        TacticalRequestScope::Finding => TacticalRequestAddress::Finding {
            ply,
            motif,
            producer_id,
        },
        TacticalRequestScope::Decision => TacticalRequestAddress::Decision {
            ply,
            motif,
            producer_id,
        },
    };
    stored_binding(
        connection,
        dataset,
        &analysis.source,
        &analysis.game_id,
        &analysis.learner,
        &analysis.report,
        address,
    )
}

pub(super) fn training_binding(
    connection: &Connection,
    dataset: &DatasetId,
    game: &TacticalGame,
    report: &str,
    address: TacticalRequestAddress<'_>,
) -> Result<Option<TacticalPuzzleRef>, StoreError> {
    active(connection, dataset)?;
    Ok(stored_binding(
        connection,
        dataset,
        &game.game.source,
        &game.game.id,
        &game.learner,
        report,
        address,
    )?
    .map(|stored| stored.puzzle))
}

fn stored_binding(
    connection: &Connection,
    dataset: &DatasetId,
    source: &str,
    game_id: &str,
    learner: &str,
    report: &str,
    address: TacticalRequestAddress<'_>,
) -> Result<Option<StoredTrainingBinding>, StoreError> {
    let (decision, ply, motif, producer) = match address {
        TacticalRequestAddress::Decision {
            ply,
            motif,
            producer_id,
        } => (true, ply, motif, producer_id),
        TacticalRequestAddress::Finding {
            ply,
            motif,
            producer_id,
        } => (false, ply, motif, producer_id),
    };
    // Earlier Decision acceptance could move a Finding request to the
    // whole-decision observation. Retain that binding if its exact Finding
    // address no longer has a request. Personal Decision is a stored kind,
    // independent of detector identity or display text.
    let row = connection
        .query_row(
            "SELECT r.id,r.target_id,r.version,(r.attempt_id IS NOT NULL OR r.fulfilled=1)
             FROM tactical_observations o CROSS JOIN tactical_requests r ON o.id=r.observation_id
             WHERE (o.dataset_id,o.source,o.game_id,o.learner,o.report,o.ply)=(?1,?2,?3,?4,?5,?6)
                 AND (?7 OR (o.motif,o.producer_id)=(?8,?9) OR o.motif=?10)
             ORDER BY (?7 AND (r.attempt_id IS NOT NULL OR r.fulfilled=1)) DESC,
                      ((o.motif,o.producer_id)=(?8,?9)) DESC,
                      (r.attempt_id IS NOT NULL OR r.fulfilled=1) DESC,r.id LIMIT 1",
            params![
                dataset.0,
                source,
                game_id,
                learner,
                report,
                ply,
                decision,
                motif,
                producer,
                PERSONAL_DECISION_KIND
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get(3)?,
                ))
            },
        )
        .optional()
        .map_err(database_error)?;
    row.map(|(id, target, version, issued_or_fulfilled)| {
        Ok(StoredTrainingBinding {
            id,
            puzzle: TacticalPuzzleRef {
                target: TargetId(target),
                version: ContentVersion(decode_counter(version)?),
            },
            issued_or_fulfilled,
        })
    })
    .transpose()
}
