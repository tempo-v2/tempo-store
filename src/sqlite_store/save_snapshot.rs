//! Persist the validated snapshot inside the caller's open write transaction.

use super::database_error;
use super::row_values::{encode, encode_receipt, row_index};
use crate::{AssessmentKey, ReceiptResult, Snapshot, StoreError};
use rusqlite::{Transaction, params};
use tempo_chess::ContentKind;

pub(super) fn save_snapshot(
    transaction: &Transaction<'_>,
    previous: Option<&Snapshot>,
    next: &Snapshot,
) -> Result<(), StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.save_snapshot.save_snapshot");
    save_history(transaction, previous, next)?;

    let replace_catalog =
        previous.is_none_or(|old| old.dataset != next.dataset || old.catalog != next.catalog);
    if replace_catalog {
        if let Some(old) = previous.filter(|old| old.dataset == next.dataset) {
            super::game_model::catalog_changed(
                transaction,
                &next.dataset,
                &old.catalog,
                &next.catalog,
            )?;
        }
        save_catalog(transaction, next)?;
        if previous.is_some() {
            transaction
                .execute(
                    "UPDATE store_state SET content_revision=content_revision+1",
                    [],
                )
                .map_err(database_error)?;
        }
    }
    for (target, state) in &next.learning {
        if replace_catalog || previous.and_then(|old| old.learning.get(target)) != Some(state) {
            transaction
                .execute(
                    "INSERT INTO learning VALUES (?1, ?2)
                     ON CONFLICT(target_id) DO UPDATE SET state = excluded.state",
                    params![target.0, encode(state)?],
                )
                .map_err(database_error)?;
        }
    }
    for (id, record) in &next.attempts {
        let old = previous.and_then(|old| old.attempts.get(id));
        if old == Some(record) {
            continue;
        }
        let attempt = &record.attempt;
        let payload = encode(record)?;
        if old.is_some() {
            transaction
                .execute(
                    "UPDATE attempts SET record=?2 WHERE id=?1",
                    params![id.0, payload],
                )
                .map_err(database_error)?;
        } else {
            transaction
                .execute(
                    "INSERT INTO attempts VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        id.0,
                        attempt.dataset.0,
                        attempt.target.0,
                        attempt.content_version.0.to_be_bytes().as_slice(),
                        payload
                    ],
                )
                .map_err(database_error)?;
        }
    }

    transaction
        .execute(
            "INSERT INTO store_state (singleton, dataset_id, revision, active_attempt_id, policy) VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(singleton) DO UPDATE SET dataset_id = excluded.dataset_id,
             revision = excluded.revision, active_attempt_id = excluded.active_attempt_id,
             policy = excluded.policy",
            params![
                next.dataset.0,
                next.revision.0.to_be_bytes().as_slice(),
                next.active_attempt.as_ref().map(|id| &id.0),
                encode(&next.policy)?,
            ],
        )
        .map_err(database_error)?;

    if let Some(old) = previous {
        if old.dataset != next.dataset {
            super::repertoire_runs::save(transaction, &old.dataset, None)?;
        } else if old.repertoire_run != next.repertoire_run {
            super::repertoire_runs::save(transaction, &next.dataset, next.repertoire_run.as_ref())?;
        }
    }
    save_outcomes(transaction, previous, next)?;
    if let Some(previous) = previous {
        super::tactical_analysis::settle_attempts(transaction, previous, next)?;
    }
    Ok(())
}

fn save_history(
    transaction: &Transaction<'_>,
    previous: Option<&Snapshot>,
    next: &Snapshot,
) -> Result<(), StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.save_snapshot.save_history");
    for dataset in &next.known_datasets {
        if previous.is_none_or(|old| !old.known_datasets.contains(dataset)) {
            transaction
                .execute("INSERT INTO datasets VALUES (?1)", [&dataset.0])
                .map_err(database_error)?;
        }
    }
    let history_count = previous.map_or(0, |old| old.content_history.len());
    for (ordinal, entry) in next.content_history.iter().enumerate().skip(history_count) {
        transaction
            .execute(
                "INSERT INTO content_history VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    row_index(ordinal)?,
                    entry.dataset.0,
                    entry.item.target.0,
                    entry.item.version.0.to_be_bytes().as_slice(),
                    encode(&entry.item)?,
                ],
            )
            .map_err(database_error)?;
    }
    Ok(())
}

fn save_catalog(transaction: &Transaction<'_>, next: &Snapshot) -> Result<(), StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.save_snapshot.save_catalog");
    transaction
        .execute_batch(
            "DELETE FROM line_targets;
             DELETE FROM repertoire_lines;
             DELETE FROM learning;
             DELETE FROM current_content;",
        )
        .map_err(database_error)?;
    for item in next.catalog.items.values() {
        let (repertoire, position) = match &item.definition.kind {
            ContentKind::Repertoire {
                repertoire,
                position,
            } => (Some(repertoire), Some(&position.0)),
            ContentKind::Tactic { .. } => (None, None),
        };
        transaction
            .execute(
                "INSERT INTO current_content VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    item.target.0,
                    next.dataset.0,
                    item.version.0.to_be_bytes().as_slice(),
                    repertoire,
                    position,
                    encode(item)?,
                ],
            )
            .map_err(database_error)?;
        super::tactical_targets::index_item(transaction, item)?;
        super::tactical_targets::release_independent_tactic(transaction, &next.dataset, item)?;
    }
    for (ordinal, line) in next.catalog.lines.iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO repertoire_lines VALUES (?1, ?2, ?3)",
                params![row_index(ordinal)?, line.id, line.repertoire],
            )
            .map_err(database_error)?;
        let mut insert = transaction
            .prepare_cached("INSERT INTO line_targets VALUES (?1, ?2, ?3, ?4)")
            .map_err(database_error)?;
        for (ordinal, target) in line.targets.iter().enumerate() {
            insert
                .execute(params![
                    line.id,
                    row_index(ordinal)?,
                    line.repertoire,
                    target.0
                ])
                .map_err(database_error)?;
        }
    }
    Ok(())
}

fn save_outcomes(
    transaction: &Transaction<'_>,
    previous: Option<&Snapshot>,
    next: &Snapshot,
) -> Result<(), StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.save_snapshot.save_outcomes");
    let assessment_count = previous.map_or(0, |old| old.assessments.len());
    for record in next.assessments.iter().skip(assessment_count) {
        let (attempt, provider, event) = match &record.key {
            AssessmentKey::Human(id) => (Some(&id.0), None, None),
            AssessmentKey::External { provider, event_id } => {
                (None, Some(provider), Some(&event_id.0))
            }
        };
        transaction
            .execute(
                "INSERT INTO assessments VALUES (NULL, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    record.dataset.0,
                    record.target.0,
                    record.content_version.0.to_be_bytes().as_slice(),
                    attempt,
                    provider,
                    event,
                    encode(record)?,
                ],
            )
            .map_err(database_error)?;
    }
    for (id, receipt) in &next.receipts {
        if previous.is_none_or(|old| !old.receipts.contains_key(id)) {
            let attempt = match &receipt.result {
                ReceiptResult::Study(result) => Some(&result.attempt.id.0),
                ReceiptResult::External { .. } | ReceiptResult::TacticsAllowance { .. } => None,
            };
            transaction
                .execute(
                    "INSERT INTO receipts VALUES (?1, ?2, ?3, ?4)",
                    params![id.0, attempt, receipt.command, encode_receipt(&receipt.result)?],
                )
                .map_err(database_error)?;
        }
    }
    Ok(())
}
