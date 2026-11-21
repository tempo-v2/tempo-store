use super::row_values::{decode, decode_counter, read_selected, text_value};
use super::{database_error, load_study, save_snapshot, stage_commit};
use crate::{
    ChessAttempt, Commit, RevealedDecision, ReviewCommandKey, ReviewWrite, SavedReview,
    SavedReviewContext, StoreError, StoredAttempt, StudyScope, TacticExclusionRead, Write,
    store_error::{conflict, invalid},
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use tempo_study::AttemptStatus;
use tempo_types::{DatasetId, Revision};

fn check_dataset(connection: &Connection, dataset: &DatasetId) -> Result<(), StoreError> {
    let current: String = connection
        .query_row(
            "SELECT dataset_id FROM store_state WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    if current != dataset.0 {
        return Err(invalid(
            "The Study dataset changed. Reopen Study and try again.",
        ));
    }
    Ok(())
}

pub(super) fn page(
    connection: &Connection,
    dataset: &DatasetId,
    before: Option<i64>,
) -> Result<Vec<SavedReview>, StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.saved_reviews.page");
    let tx = connection.unchecked_transaction().map_err(database_error)?;
    check_dataset(&tx, dataset)?;
    read_selected(
        &tx,
        "SELECT r.id, json_extract(a.record, '$.repertoire_name'), r.decision_index, r.saved_at_ms,
             c.target_id IS NOT NULL AND c.repertoire IS NULL AND x.target_id IS NULL,
             x.target_id IS NOT NULL
         FROM saved_reviews r JOIN attempts a ON a.id=r.attempt_id
         LEFT JOIN current_content c ON (c.dataset_id,c.target_id)=(r.dataset_id,r.target_id)
         LEFT JOIN study_exclusions x ON (x.dataset_id,x.target_id)=(r.dataset_id,r.target_id)
         WHERE r.dataset_id=?1 AND r.id < ?2 ORDER BY r.id DESC LIMIT 31",
        params![dataset.0, before.unwrap_or(i64::MAX)],
        |row| {
            Ok(SavedReview {
                id: row.get(0).map_err(database_error)?,
                exercise_name: row.get(1).map_err(database_error)?,
                decision_index: row.get(2).map_err(database_error)?,
                saved_at_ms: row.get(3).map_err(database_error)?,
                can_remove_from_study: row.get(4).map_err(database_error)?,
                removed_from_study: row.get(5).map_err(database_error)?,
            })
        },
    )
}

pub(super) fn context(
    connection: &Connection,
    dataset: &DatasetId,
    id: i64,
) -> Result<Option<SavedReviewContext>, StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.saved_reviews.context");
    let tx = connection.unchecked_transaction().map_err(database_error)?;
    check_dataset(&tx, dataset)?;
    let rows: Vec<SavedReviewContext> = read_selected(
        &tx,
        "WITH mark AS (
             SELECT r.decision_index, r.attempt_id, r.dataset_id, r.target_id, r.version,
                    source.record AS source_record
             FROM saved_reviews r JOIN attempts source ON source.id=r.attempt_id
             WHERE r.dataset_id=?1 AND r.id=?2
         ), later_attempts AS (
             SELECT json_extract(later.record, '$.attempt.status') AS status,
                    json_extract(later.record, '$.revealed_decision.decision_index') AS revealed
             FROM mark JOIN attempts later
               ON later.dataset_id=mark.dataset_id AND later.target_id=mark.target_id
                 AND later.version=mark.version
             WHERE json_extract(later.record, '$.issued_revision') >
                   coalesce(json_extract(mark.source_record, '$.issued_revision'), -1)
               AND json_extract(later.record, '$.attempt.status') IN ('complete','abandoned')
         )
         SELECT mark.decision_index, mark.source_record, active.record,
                EXISTS(SELECT 1 FROM later_attempts WHERE status='complete'),
                (SELECT max(revealed) FROM later_attempts WHERE status='abandoned'),
                CASE WHEN json_extract(mark.source_record, '$.issued_revision') IS NULL
                     AND json_extract(mark.source_record, '$.revealed_decision') IS NULL
                     THEN EXISTS(SELECT 1 FROM receipts
                                 WHERE attempt_id=mark.attempt_id
                                   AND json_extract(result, '$.Study.attempt.state.decision_index')=mark.decision_index
                                   AND json_extract(result, '$.Study.feedback.correct')=0)
                     ELSE 0 END,
                EXISTS(SELECT 1 FROM study_exclusions x
                       WHERE (x.dataset_id,x.target_id)=(mark.dataset_id,mark.target_id))
         FROM mark JOIN store_state s ON s.singleton=1
         LEFT JOIN attempts active ON active.id=s.active_attempt_id AND active.target_id=mark.target_id",
        params![dataset.0, id],
        |row| {
            Ok(SavedReviewContext {
                decision_index: row.get(0).map_err(database_error)?,
                source: text_value(row, 1)?,
                active: row
                    .get::<_, Option<String>>(2)
                    .map_err(database_error)?
                    .as_deref()
                    .map(decode)
                    .transpose()?,
                later_completed: row.get(3).map_err(database_error)?,
                later_abandoned_reveal: row.get::<_, Option<usize>>(4).map_err(database_error)?
                    .map(|decision_index| RevealedDecision { decision_index }),
                legacy_source_feedback: row.get(5).map_err(database_error)?,
                excluded_from_study: row.get(6).map_err(database_error)?,
            })
        },
    )?;
    Ok(rows.into_iter().next())
}

pub(super) fn receipt(
    connection: &Connection,
    key: &ReviewCommandKey<'_>,
) -> Result<Option<i64>, StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.saved_reviews.receipt");
    let saved: Option<(String, i64)> = connection
        .query_row(
            "SELECT fingerprint, review_id FROM saved_review_receipts
             WHERE dataset_id=?1 AND submission_id=?2",
            params![key.dataset.0, key.submission],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(database_error)?;
    match saved {
        Some((fingerprint, id)) if fingerprint == key.fingerprint => Ok(Some(id)),
        Some(_) => Err(invalid(
            "This review command ID already has a different meaning.",
        )),
        None => Ok(None),
    }
}

pub(super) fn write(
    connection: &mut Connection,
    key: &ReviewCommandKey<'_>,
    change: ReviewWrite<'_>,
) -> Result<i64, StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.saved_reviews.write");
    let tx = crate::diagnostics::timed("sqlite.begin", || {
        connection.transaction_with_behavior(TransactionBehavior::Immediate)
    })
    .map_err(database_error)?;
    check_dataset(&tx, key.dataset)?;
    if let Some(id) = receipt(&tx, key)? {
        return Ok(id);
    }
    let id = match change {
        ReviewWrite::Save(position) => {
            let version = position.version.0.to_be_bytes();
            let existing = tx
                .query_row(
                    "SELECT id FROM saved_reviews
                 WHERE dataset_id=?1 AND target_id=?2 AND version=?3 AND decision_index=?4",
                    params![
                        key.dataset.0,
                        position.target.0,
                        version,
                        position.decision_index
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(database_error)?;
            if let Some(id) = existing {
                id
            } else {
                let count = tx
                    .execute(
                        "INSERT INTO saved_reviews
                       (dataset_id, target_id, version, decision_index, attempt_id, saved_at_ms)
                     SELECT dataset_id, target_id, version, ?4, id, ?6 FROM attempts
                     WHERE dataset_id=?1 AND target_id=?2 AND version=?3 AND id=?5",
                        params![
                            key.dataset.0,
                            position.target.0,
                            version,
                            position.decision_index,
                            position.attempt.0,
                            position.saved_at_ms
                        ],
                    )
                    .map_err(database_error)?;
                if count != 1 {
                    return Err(conflict(
                        "The marked exercise was removed before it could be saved.",
                    ));
                }
                tx.last_insert_rowid()
            }
        }
        ReviewWrite::Remove(id) => {
            tx.execute(
                "DELETE FROM saved_reviews WHERE dataset_id=?1 AND id=?2",
                params![key.dataset.0, id],
            )
            .map_err(database_error)?;
            id
        }
        ReviewWrite::ExcludeTactic {
            id,
            expected_revision,
            abandoned,
        } => {
            exclude_tactic(
                &tx,
                key.dataset,
                id,
                expected_revision,
                abandoned.map(|attempt| *attempt),
            )?;
            id
        }
    };
    tx.execute(
        "INSERT INTO saved_review_receipts VALUES (?1, ?2, ?3, ?4)",
        params![key.dataset.0, key.submission, key.fingerprint, id],
    )
    .map_err(database_error)?;
    crate::diagnostics::timed("sqlite.commit", || tx.commit()).map_err(database_error)?;
    Ok(id)
}

fn exclusion_target(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    id: i64,
) -> Result<String, StoreError> {
    tx.query_row(
        "SELECT r.target_id FROM saved_reviews r
         JOIN current_content c ON (c.dataset_id,c.target_id)=(r.dataset_id,r.target_id)
         WHERE r.dataset_id=?1 AND r.id=?2 AND c.repertoire IS NULL",
        params![dataset.0, id],
        |row| row.get(0),
    )
    .optional()
    .map_err(database_error)?
    .ok_or_else(|| invalid("This saved mark does not address a tactic in the collection."))
}

pub(super) fn exclusion_basis(
    connection: &Connection,
    key: &ReviewCommandKey<'_>,
    id: i64,
) -> Result<TacticExclusionRead, StoreError> {
    let tx = connection.unchecked_transaction().map_err(database_error)?;
    check_dataset(&tx, key.dataset)?;
    if let Some(id) = receipt(&tx, key)? {
        return Ok(TacticExclusionRead::Accepted(id));
    }
    let target = exclusion_target(&tx, key.dataset, id)?;
    let (revision, active): (Vec<u8>, Option<String>) = tx.query_row(
        "SELECT s.revision,a.record FROM store_state s
         LEFT JOIN attempts a ON a.id=s.active_attempt_id AND a.target_id=?1 AND a.dataset_id=s.dataset_id
         WHERE s.singleton=1",
        [target],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).map_err(database_error)?;
    Ok(TacticExclusionRead::Pending {
        revision: Revision(decode_counter(revision)?),
        active: active
            .as_deref()
            .map(decode::<StoredAttempt>)
            .transpose()?
            .map(|record| Box::new(record.attempt)),
    })
}

fn exclude_tactic(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    id: i64,
    expected_revision: Revision,
    abandoned: Option<ChessAttempt>,
) -> Result<(), StoreError> {
    let target = exclusion_target(tx, dataset, id)?;
    let changed = tx
        .execute(
            "INSERT INTO study_exclusions(dataset_id,target_id,marked_version)
         SELECT dataset_id,target_id,version FROM saved_reviews WHERE id=?1 AND dataset_id=?2
         ON CONFLICT(dataset_id,target_id) DO NOTHING",
            params![id, dataset.0],
        )
        .map_err(database_error)?;
    if changed == 0 {
        return Ok(());
    }
    let state = load_study::load_study(tx, StudyScope::default())?;
    if state.revision != expected_revision {
        return Err(conflict("Study changed while removing this tactic."));
    }
    let active = state
        .active_attempt
        .as_ref()
        .and_then(|id| state.attempts.get(id))
        .filter(|record| record.attempt.target.0 == target);
    let mut writes = Vec::new();
    match (active, abandoned) {
        (None, None) => {}
        (Some(record), Some(attempt))
            if attempt.id == record.attempt.id
                && attempt.status == AttemptStatus::Abandoned
                && attempt.terminal == record.attempt.terminal
                && attempt.state == record.attempt.state
                && attempt.prompt.is_none() =>
        {
            writes.extend([
                Write::Progress(StoredAttempt {
                    attempt,
                    ..record.clone()
                }),
                Write::Active(None),
            ]);
        }
        _ => {
            return Err(invalid(
                "Removing a tactic requires its prepared Study abandonment.",
            ));
        }
    }
    let commit = Commit {
        expected_dataset: dataset.clone(),
        expected_revision: state.revision,
        writes,
    };
    let previous = load_study::staging_image(state);
    let next = stage_commit(&previous, commit)?;
    save_snapshot(tx, Some(&previous), &next)
}
