//! Verify model publication against the current dataset and per-book authority.
use super::{database_error, game_relevance, row_values};
use crate::store_error::{conflict, invalid};
use crate::{
    BookProjection, ModelInputStamp, ModelTargets, ProjectionBook, ProjectionScope,
    ProjectionStatus, REACH_MODEL_KEY, StoreError, model_lines, model_targets,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use tempo_chess::{Catalog, ContentItem};
use tempo_types::DatasetId;

pub(super) fn catalog_changed(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    previous: &Catalog,
    next: &Catalog,
) -> Result<(), StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.game_model.catalog_changed");
    let repertoires: Vec<String> = row_values::read_selected(
        tx,
        "SELECT repertoire_id FROM game_model_inputs WHERE dataset_id=?1 AND state<>'unavailable'",
        [&dataset.0],
        |row| row.get(0).map_err(database_error),
    )?;
    for id in repertoires {
        if model_targets(previous, &id) != model_targets(next, &id)
            || model_lines(previous, &id) != model_lines(next, &id)
        {
            tx.execute(
                "UPDATE game_model_inputs SET state='unavailable',input_key=NULL
                 WHERE dataset_id=?1 AND repertoire_id=?2",
                params![dataset.0, id],
            )
            .map_err(database_error)?;
        }
    }
    Ok(())
}

fn staged_targets(
    db: &Connection,
    dataset: &DatasetId,
    repertoire: &str,
) -> Result<ModelTargets, StoreError> {
    row_values::read_selected(
        db,
        "SELECT item FROM current_content WHERE dataset_id=?1 AND repertoire=?2
           AND json_extract(item,'$.status')='active'",
        params![dataset.0, repertoire],
        |row| {
            let item: ContentItem = row_values::text_value(row, 0)?;
            Ok((item.target, (item.version, item.definition)))
        },
    )
}

pub(super) fn set_verified(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    repertoire: &str,
    stamp: &ModelInputStamp,
) -> Result<bool, StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.game_model.set_verified");
    if stamp.key.is_empty() || stamp.targets != staged_targets(tx, dataset, repertoire)? {
        return Err(invalid("model input does not match the staged repertoire"));
    }
    let changed = tx.execute(
        "INSERT INTO game_model_inputs (dataset_id,repertoire_id,state,input_key) VALUES (?1,?2,'verified',?3)
         ON CONFLICT(dataset_id,repertoire_id) DO UPDATE SET state='verified',input_key=excluded.input_key
         WHERE state<>'verified' OR input_key<>excluded.input_key",
        params![dataset.0, repertoire, stamp.key],
    ).map_err(database_error)?;
    Ok(changed != 0)
}

pub(super) fn books(
    db: &Connection,
    dataset: &DatasetId,
    scope: ProjectionScope,
    repertoire: Option<&str>,
) -> Result<Vec<ProjectionBook>, StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.game_model.books");
    let tx =
        Transaction::new_unchecked(db, TransactionBehavior::Deferred).map_err(database_error)?;
    game_relevance::check_dataset(&tx, dataset)?;
    let current: Vec<(String, String, String)> = row_values::read_selected(
        &tx,
        "SELECT input.repertoire_id,source.pgn,input.input_key
         FROM game_model_inputs AS input
         JOIN repertoire_documents AS doc USING(dataset_id,repertoire_id)
         JOIN content_sources AS source ON source.id=doc.source_id
         LEFT JOIN game_model_projection AS result
           ON (result.dataset_id,result.repertoire_id,result.input_key)
            = (input.dataset_id,input.repertoire_id,input.input_key) AND result.model_key=?2
         WHERE input.dataset_id=?1 AND input.state='verified'
           AND NOT EXISTS (SELECT 1 FROM repertoire_suspensions AS suspended
               WHERE (suspended.dataset_id,suspended.repertoire_id)=(input.dataset_id,input.repertoire_id))
           AND (?3 OR result.input_key IS NULL OR result.complete=0)
           AND (?4 IS NULL OR input.repertoire_id=?4)
         ORDER BY input.repertoire_id",
        params![
            dataset.0,
            REACH_MODEL_KEY,
            scope == ProjectionScope::AllVerified, repertoire
        ],
        |row| {
            Ok((
                row.get(0).map_err(database_error)?,
                row.get(1).map_err(database_error)?,
                row.get(2).map_err(database_error)?,
            ))
        },
    )?;
    let books = current
        .into_iter()
        .map(|(repertoire_id, pgn, input_key)| {
            let targets = staged_targets(&tx, dataset, &repertoire_id)?;
            Ok(ProjectionBook {
                repertoire_id,
                pgn,
                input_key,
                targets,
            })
        })
        .collect::<Result<_, StoreError>>()?;
    crate::diagnostics::timed("sqlite.commit", || tx.commit()).map_err(database_error)?;
    Ok(books)
}

pub(super) fn status(db: &Connection, dataset: &DatasetId) -> Result<ProjectionStatus, StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.game_model.status");
    db.query_row(
        "WITH active_books AS (
           SELECT DISTINCT repertoire AS repertoire_id FROM current_content
           WHERE dataset_id=?1 AND repertoire IS NOT NULL AND json_extract(item,'$.status')='active'
           UNION SELECT repertoire_id FROM game_model_inputs WHERE dataset_id=?1 AND state='verified'
         )
         SELECT count(*) FILTER (WHERE result.complete=1),
                count(*) FILTER (WHERE result.complete=0),
                count(*) FILTER (WHERE input.state IS NULL OR input.state='unavailable'),
                count(*) FILTER (WHERE input.state='needs_bootstrap'
                    OR (input.state='verified' AND result.input_key IS NULL))
         FROM active_books
         LEFT JOIN game_model_inputs AS input
           ON input.dataset_id=?1 AND input.repertoire_id=active_books.repertoire_id
         LEFT JOIN game_model_projection AS result
           ON (result.dataset_id,result.repertoire_id,result.input_key)
              = (input.dataset_id,input.repertoire_id,input.input_key) AND result.model_key=?2
         WHERE NOT EXISTS (SELECT 1 FROM repertoire_suspensions AS suspended
             WHERE suspended.dataset_id=?1 AND suspended.repertoire_id=active_books.repertoire_id)",
        params![dataset.0, REACH_MODEL_KEY],
        |row| Ok(ProjectionStatus {
            dataset_id: dataset.clone(), complete_current: row.get(0)?,
            partial_current: row.get(1)?, unavailable: row.get(2)?, pending: row.get(3)?,
        }),
    ).map_err(database_error)
}

pub(super) fn current_input(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    repertoire: &str,
    input_key: &str,
) -> Result<bool, StoreError> {
    tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM game_model_inputs WHERE dataset_id=?1 AND repertoire_id=?2
           AND state='verified' AND input_key=?3
           AND NOT EXISTS (SELECT 1 FROM repertoire_suspensions WHERE dataset_id=?1 AND repertoire_id=?2))",
            params![dataset.0, repertoire, input_key],
            |row| row.get(0),
        )
        .map_err(database_error)
}

pub(super) fn replace(
    db: &mut Connection,
    dataset: &DatasetId,
    projection: BookProjection<'_>,
) -> Result<(), StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.game_model.replace");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    game_relevance::check_dataset(&tx, dataset)?;
    if projection.model != REACH_MODEL_KEY {
        return Err(invalid("unsupported projection algorithm"));
    }
    if !current_input(&tx, dataset, projection.repertoire_id, projection.input_key)? {
        return Err(conflict("the repertoire's model input changed"));
    }
    let targets = staged_targets(&tx, dataset, projection.repertoire_id)?;
    if (projection.complete && projection.reach.len() != targets.len())
        || projection
            .reach
            .iter()
            .any(|(target, mass)| !targets.contains_key(target) || *mass > 1_000_000)
    {
        return Err(invalid(
            "projected reach does not match the active repertoire",
        ));
    }
    let complete: Option<bool> = tx
        .query_row(
            "SELECT complete FROM game_model_projection
         WHERE dataset_id=?1 AND repertoire_id=?2 AND model_key=?3 AND input_key=?4",
            params![
                dataset.0,
                projection.repertoire_id,
                projection.model,
                projection.input_key
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    if complete == Some(true) && !projection.complete {
        return Ok(());
    }
    tx.execute(
        "DELETE FROM game_model_projection WHERE dataset_id=?1 AND repertoire_id=?2 AND model_key=?3",
        params![dataset.0, projection.repertoire_id, projection.model],
    ).map_err(database_error)?;
    tx.execute(
        "INSERT INTO game_model_projection (dataset_id,repertoire_id,model_key,input_key,complete)
         VALUES (?1,?2,?3,?4,?5)",
        params![
            dataset.0,
            projection.repertoire_id,
            projection.model,
            projection.input_key,
            projection.complete
        ],
    )
    .map_err(database_error)?;
    for (target, millionths) in projection.reach {
        tx.execute(
            "INSERT INTO game_model_reach (dataset_id,repertoire_id,model_key,input_key,target_id,millionths)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![dataset.0, projection.repertoire_id, projection.model, projection.input_key, target.0, millionths],
        ).map_err(database_error)?;
    }
    crate::diagnostics::timed("sqlite.commit", || tx.commit()).map_err(database_error)
}
