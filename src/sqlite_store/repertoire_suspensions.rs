//! Persist repertoire participation and any accompanying Study write atomically.

use super::{database_error, load_study, row_values, save_snapshot::save_snapshot, study_plan};
use crate::{
    ChessAttempt, Commit, RepertoireActivity, RepertoireActivityCommand, RepertoireActivityRead,
    RepertoireActivityResult, StoreError, StudyScope, StudyState, Write,
    store_error::{conflict, invalid},
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::BTreeSet;
use tempo_study::AttemptStatus;
use tempo_types::Revision;

pub(super) fn read(db: &Connection, dataset: &str) -> Result<BTreeSet<String>, StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.repertoire_suspensions.read");
    row_values::read_selected(
        db,
        "SELECT repertoire_id FROM repertoire_suspensions WHERE dataset_id=?1",
        [dataset],
        |row| row.get(0).map_err(database_error),
    )
}

struct Receipt {
    key: String,
    acknowledgment: String,
}

impl Receipt {
    fn for_command(command: &RepertoireActivityCommand<'_>) -> Result<Self, StoreError> {
        Ok(Self {
            // This namespace is separate from signed PGN preview tokens in the same journal.
            key: format!("repertoire-activity:{}", command.command_id),
            acknowledgment: row_values::encode(&(
                command.dataset,
                command.repertoire,
                command.activity,
            ))?,
        })
    }
}

fn inspect(
    tx: &Transaction<'_>,
    state: &StudyState,
    command: &RepertoireActivityCommand<'_>,
    receipt: &Receipt,
) -> Result<RepertoireActivityRead, StoreError> {
    use RepertoireActivityRead::{Pending, Settled};
    use RepertoireActivityResult::{Applied, CommandConflict, StaleDataset, UnknownRepertoire};

    if &state.dataset != command.dataset {
        return Ok(Settled(StaleDataset));
    }
    let previous: Option<String> = tx
        .query_row(
            "SELECT reply FROM content_receipts WHERE request=?1",
            [&receipt.key],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    if let Some(previous) = previous {
        return Ok(Settled(if previous == receipt.acknowledgment {
            Applied
        } else {
            CommandConflict
        }));
    }
    let active = state
        .active_attempt
        .as_ref()
        .map(|id| &state.attempts[id].attempt);
    let (exists, active_matches): (bool, bool) = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM current_content WHERE dataset_id=?1 AND repertoire=?2),
                    EXISTS(SELECT 1 FROM current_content WHERE dataset_id=?1 AND repertoire=?2 AND target_id=?3)",
            params![command.dataset.0, command.repertoire, active.map(|attempt| &attempt.target.0)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(database_error)?;
    if !exists {
        return Ok(Settled(UnknownRepertoire));
    }
    let active = active
        .filter(|_| active_matches)
        .map(|attempt| Box::new(attempt.clone()));
    Ok(Pending {
        revision: state.revision,
        active,
    })
}

pub(super) fn context(
    db: &Connection,
    command: &RepertoireActivityCommand<'_>,
) -> Result<RepertoireActivityRead, StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.repertoire_suspensions.context");
    let tx =
        Transaction::new_unchecked(db, TransactionBehavior::Deferred).map_err(database_error)?;
    let state = load_study::load_study(&tx, StudyScope::default())?;
    inspect(&tx, &state, command, &Receipt::for_command(command)?)
}

pub(super) fn apply(
    db: &mut Connection,
    command: &RepertoireActivityCommand<'_>,
    expected_revision: Revision,
    abandoned: Option<ChessAttempt>,
) -> Result<RepertoireActivityResult, StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.repertoire_suspensions.apply");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    let state = load_study::load_study(&tx, StudyScope::default())?;
    let receipt = Receipt::for_command(command)?;
    let (revision, active) = match inspect(&tx, &state, command, &receipt)? {
        RepertoireActivityRead::Settled(result) => return Ok(result),
        RepertoireActivityRead::Pending { revision, active } => (revision, active),
    };
    if revision != expected_revision {
        return Err(conflict("Study changed while setting repertoire activity"));
    }
    let mut writes = Vec::new();
    let expected_abandonment = active
        .as_deref()
        .filter(|_| command.activity == RepertoireActivity::Suspended)
        .map(|attempt| &attempt.id);
    if expected_abandonment != abandoned.as_ref().map(|attempt| &attempt.id) {
        return Err(invalid("activity batch carries a different Study attempt"));
    }
    if let Some(attempt) = abandoned {
        let mut record = state.attempts[&attempt.id].clone();
        if attempt.status != AttemptStatus::Abandoned
            || attempt.terminal != record.attempt.terminal
            || attempt.state != record.attempt.state
        {
            return Err(invalid(
                "activity batch requires abandonment without changing the outcome or cursor",
            ));
        }
        record.attempt = attempt;
        writes.extend([Write::Progress(record), Write::Active(None)]);
    }
    let key = params![command.dataset.0, command.repertoire];
    let change = match command.activity {
        RepertoireActivity::Suspended => tx.execute(
            "INSERT INTO repertoire_suspensions VALUES (?1,?2) ON CONFLICT DO NOTHING",
            key,
        ),
        RepertoireActivity::Active => tx.execute(
            "DELETE FROM repertoire_suspensions WHERE dataset_id=?1 AND repertoire_id=?2",
            key,
        ),
    }
    .map_err(database_error)?;
    if change != 0 {
        if command.activity == RepertoireActivity::Suspended
            && state
                .repertoire_run
                .as_ref()
                .is_some_and(|run| run.repertoire == command.repertoire)
        {
            writes.push(Write::RepertoireRun(None));
        }
        let commit = Commit {
            expected_dataset: command.dataset.clone(),
            expected_revision,
            writes,
        };
        let previous = load_study::staging_image(state);
        let next = super::stage_commit(&previous, commit)?;
        save_snapshot(&tx, Some(&previous), &next)?;
        tx.execute(
            "INSERT INTO game_participation_revisions VALUES (?1,1)
             ON CONFLICT(dataset_id) DO UPDATE SET revision=revision+1",
            [&command.dataset.0],
        )
        .map_err(database_error)?;
        study_plan::mark_content_projections_dirty(&tx, command.dataset)?;
    }
    tx.execute(
        "INSERT INTO content_receipts VALUES (?1,?2)",
        params![receipt.key, receipt.acknowledgment],
    )
    .map_err(database_error)?;
    crate::diagnostics::timed("sqlite.commit", || tx.commit()).map_err(database_error)?;
    Ok(RepertoireActivityResult::Applied)
}
