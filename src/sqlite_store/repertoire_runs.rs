//! Bounded storage for the active run and its current-version recall evidence.
use super::database_error;
use super::row_values::{decode, decode_counter, encode, read_selected};
use crate::{AssessmentRecord, RepertoireRun, RepertoireRunStep, StoreError};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use tempo_types::{AttemptId, ContentVersion, DatasetId, Grade, TargetId};

pub(super) fn load(db: &Connection, dataset: &str) -> Result<Option<RepertoireRun>, StoreError> {
    let rows: Vec<_> = read_selected(
        db,
        "SELECT repertoire_id, source_id, target_id, version, attempt_id, mode, lead_in, skipped_warmup
         FROM repertoire_run_steps WHERE dataset_id=?1 ORDER BY ordinal",
        [dataset],
        |row| {
            let step = RepertoireRunStep {
                target: TargetId(row.get(2).map_err(database_error)?),
                version: ContentVersion(decode_counter(row.get(3).map_err(database_error)?)?),
                issued: row.get::<_, Option<String>>(4).map_err(database_error)?.map(AttemptId),
                mode: decode(&row.get::<_, String>(5).map_err(database_error)?)?,
                lead_in: row.get::<_, Option<String>>(6).map_err(database_error)?
                    .as_deref().map(decode).transpose()?,
            };
            Ok((
                row.get::<_, String>(0).map_err(database_error)?,
                row.get::<_, String>(1).map_err(database_error)?,
                row.get::<_, bool>(7).map_err(database_error)?,
                step,
            ))
        },
    )?;
    let Some((repertoire, source_id, skipped_warmup, _)) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(RepertoireRun {
        repertoire: repertoire.clone(),
        source_id: source_id.clone(),
        skipped_warmup: *skipped_warmup,
        steps: rows.into_iter().map(|(_, _, _, step)| step).collect(),
    }))
}

pub(super) fn save(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    run: Option<&RepertoireRun>,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM repertoire_run_steps WHERE dataset_id=?1",
        [&dataset.0],
    )
    .map_err(database_error)?;
    if let Some(run) = run {
        for (ordinal, step) in run.steps.iter().enumerate() {
            tx.execute(
                "INSERT INTO repertoire_run_steps VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    dataset.0,
                    ordinal,
                    run.repertoire,
                    run.source_id,
                    step.target.0,
                    step.version.0.to_be_bytes().as_slice(),
                    step.issued.as_ref().map(|id| &id.0),
                    encode(&step.mode)?,
                    step.lead_in.as_ref().map(encode).transpose()?,
                    run.skipped_warmup
                ],
            )
            .map_err(database_error)?;
        }
    }
    Ok(())
}

pub(super) fn clear_repertoire(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    repertoire: &str,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM repertoire_run_steps WHERE dataset_id=?1 AND repertoire_id=?2",
        params![dataset.0, repertoire],
    )
    .map_err(database_error)?;
    Ok(())
}

pub(super) fn latest_test(
    db: &Connection,
    dataset: &DatasetId,
    target: &TargetId,
    version: ContentVersion,
) -> Result<Option<Grade>, StoreError> {
    let record = db.query_row(
        "SELECT h.record FROM assessments h JOIN attempts a ON a.id=h.attempt_id
         WHERE h.dataset_id=?1 AND h.target_id=?2 AND h.version=?3 AND h.attempt_id IS NOT NULL
           AND json_extract(h.record, '$.transition.effect') NOT IN ('practice_only', 'stale_positive_ignored')
           AND COALESCE(json_extract(a.record, '$.mode'), 'test') = 'test'
         ORDER BY h.ordinal DESC LIMIT 1",
        params![dataset.0, target.0, version.0.to_be_bytes().as_slice()],
        |row| row.get::<_, String>(0),
    ).optional().map_err(database_error)?;
    record
        .as_deref()
        .map(decode::<AssessmentRecord>)
        .transpose()
        .map(|record| record.map(|r| r.input.grade))
}
