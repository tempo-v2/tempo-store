//! Store the prepared order in bounded, fenced SQLite transactions.

mod authority;
pub use authority::{StudyDayAuthority, StudyProfilePlanEffect};
pub(super) use authority::{initialize_authority, migrate_authority, record_accepted_first_teach, record_issued_skip, record_prepared_learning_change};

mod capture;
pub use capture::{
    StudyPlanCapture, StudyPlanInputFacts, StudyPlanTargetFacts, StudyProfileFacts,
    StudyThemeSelection,
};

use super::{
    STUDY_PLAN_SCHEMA, SqliteStore, database_error,
    row_values::{decode_counter, read_selected},
};
use crate::{StoreError, store_error::invalid};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use tempo_types::{
    ContentVersion, DatasetId, MembershipEpoch, MissRevision, PlanBasis, PlanEntry, PlanEntryState,
    PlanGeneration, PlanHead, PlanPriority, RankRevision, StudyDay, SummaryBasis, SummaryEpoch,
    TargetId, WorkKind, WorkObligation, WorkToken,
};

/// One metadata update fits beside a full chunk in the 128-row write budget.
pub const PLAN_ENTRY_CHUNK: usize = 127;
const CANDIDATE_LIMIT: i64 = 32;
const READY_PLAN_SQL: &str = "SELECT target_id,version,work_kind,obligation_revision,
                    priority_class,due_day,ordinal
             FROM study_plan_entries WHERE generation=?1 AND state='ready'
             ORDER BY priority_class,due_day,ordinal,target_id,version,work_kind,obligation_revision
             LIMIT ?2";
const RECLAIM_PLAN_SQL: &str = "DELETE FROM study_plan_entries WHERE rowid IN (
                    SELECT rowid FROM study_plan_entries WHERE generation=?1
                    ORDER BY target_id,version,work_kind,obligation_revision LIMIT ?2
                 )";

#[derive(Debug, PartialEq, Eq)]
pub struct StudyProjectionBasis {
    pub plan: PlanBasis,
    pub summary: SummaryBasis,
    pub summary_dirty: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlanStageResult {
    Staged { next_ordinal: u64 },
    CursorMismatch { next_ordinal: u64 },
    Stale,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlanPublishResult {
    Published,
    AlreadyPublished,
    Incomplete,
    Stale,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlanReclaimResult {
    Pending,
    Reclaimed,
    Protected,
    Missing,
}

pub(super) fn initialize(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    transaction
        .execute_batch(STUDY_PLAN_SCHEMA)
        .map_err(database_error)?;
    transaction
        .execute(
            "INSERT INTO study_projection_state(dataset_id)
             SELECT dataset_id FROM store_state WHERE singleton=1",
            [],
        )
        .map_err(database_error)?;
    Ok(())
}

impl SqliteStore {
    /// Record the day observed by the runtime's Study calendar before an operation
    /// captures or uses projections. A retired dataset cannot change this basis.
    pub fn observe_study_day(
        &mut self,
        dataset: &DatasetId,
        day: StudyDay,
    ) -> Result<Option<StudyProjectionBasis>, StoreError> {
        let transaction = self.plan_write()?;
        let Some(mut state) = projection_state(&transaction, dataset)? else {
            return Ok(None);
        };
        if state.day != Some(day) {
            if state.day.is_some() {
                state.membership_epoch = MembershipEpoch(next_epoch(state.membership_epoch.0)?);
                state.summary_epoch = SummaryEpoch(next_epoch(state.summary_epoch.0)?);
            }
            authority::ensure_day(&transaction, dataset, day)?;
            state.day = Some(day);
            state.summary_dirty = true;
            transaction
                .execute(
                    "UPDATE study_projection_state
                     SET study_day=?2,membership_epoch=?3,summary_epoch=?4,summary_dirty=1
                     WHERE dataset_id=?1",
                    params![
                        dataset.0,
                        day.0,
                        state.membership_epoch.0.to_be_bytes().as_slice(),
                        state.summary_epoch.0.to_be_bytes().as_slice(),
                    ],
                )
                .map_err(database_error)?;
        }
        transaction.commit().map_err(database_error)?;
        Ok(state.basis(dataset))
    }

    /// Read plan and summary epochs together. The batch worker captures them in
    /// the same read transaction as the planning inputs before computing.
    pub fn study_projection_basis(
        &self,
        dataset: &DatasetId,
    ) -> Result<Option<StudyProjectionBasis>, StoreError> {
        Ok(projection_state(&self.connection, dataset)?.and_then(|state| state.basis(dataset)))
    }

    /// Allocate an unservable generation only for the originally captured basis.
    pub fn begin_study_plan(
        &mut self,
        basis: &PlanBasis,
        expected_entries: u64,
    ) -> Result<Option<PlanGeneration>, StoreError> {
        let expected = sqlite_count(expected_entries)?;
        let transaction = self.plan_write()?;
        if !basis_is_current(&transaction, basis)? {
            return Ok(None);
        }
        transaction
            .execute(
                "INSERT INTO study_plan_generations(
                    dataset_id,study_day,membership_epoch,rank_revision,expected_entries
                 ) VALUES (?1,?2,?3,?4,?5)",
                params![
                    basis.dataset.0,
                    basis.day.0,
                    basis.membership_epoch.0.to_be_bytes().as_slice(),
                    basis.rank_revision.0.to_be_bytes().as_slice(),
                    expected,
                ],
            )
            .map_err(database_error)?;
        let generation = read_generation(transaction.last_insert_rowid())?;
        transaction.commit().map_err(database_error)?;
        Ok(Some(generation))
    }

    /// Append a consecutive slice of the caller's original ordered proposal.
    /// The cursor result lets a builder distinguish a committed slice from
    /// one that never advanced, without applying a second set of inserts.
    pub fn stage_study_plan(
        &mut self,
        generation: PlanGeneration,
        basis: &PlanBasis,
        start_ordinal: u64,
        entries: &[PlanEntry],
    ) -> Result<PlanStageResult, StoreError> {
        let through = validate_chunk(basis, start_ordinal, entries)?;
        let id = sqlite_count(generation.0)?;
        let transaction = self.plan_write()?;
        if !basis_is_current(&transaction, basis)? {
            return Ok(PlanStageResult::Stale);
        }
        let Some(header) = generation_header(&transaction, &basis.dataset, id)? else {
            return Ok(PlanStageResult::Stale);
        };
        if header.basis != *basis || header.phase != "staging" {
            return Ok(PlanStageResult::Stale);
        }
        if header.staged_entries != start_ordinal {
            return Ok(PlanStageResult::CursorMismatch {
                next_ordinal: header.staged_entries,
            });
        }
        if through > header.expected_entries {
            return Err(invalid("Study plan chunk exceeds its declared entry count"));
        }

        {
            let mut insert = transaction
                .prepare_cached(
                    "INSERT INTO study_plan_entries(
                        generation,target_id,version,work_kind,obligation_revision,
                        priority_class,due_day,ordinal,state
                     ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                )
                .map_err(database_error)?;
            for entry in entries {
                let work = &entry.work.obligation;
                let (priority, due) = priority_columns(entry.priority);
                insert
                    .execute(params![
                        id,
                        work.target.0,
                        work.version.0.to_be_bytes().as_slice(),
                        work_kind_value(work.kind),
                        work.revision.0.to_be_bytes().as_slice(),
                        priority,
                        due,
                        entry.ordinal.to_be_bytes().as_slice(),
                        entry_state_value(entry.state),
                    ])
                    .map_err(database_error)?;
            }
        }
        transaction
            .execute(
                "UPDATE study_plan_generations SET staged_entries=?2 WHERE generation=?1",
                params![id, sqlite_count(through)?],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)?;
        Ok(PlanStageResult::Staged {
            next_ordinal: through,
        })
    }

    /// Publish a complete generation with a bounded compare-and-switch of
    /// metadata. The runtime passes its freshly observed day, not the build day.
    pub fn publish_study_plan(
        &mut self,
        generation: PlanGeneration,
        basis: &PlanBasis,
        today: StudyDay,
    ) -> Result<PlanPublishResult, StoreError> {
        if basis.day != today {
            return Ok(PlanPublishResult::Stale);
        }
        let id = sqlite_count(generation.0)?;
        let transaction = self.plan_write()?;
        let Some(head) = head_at_basis(&transaction, basis)? else {
            return Ok(PlanPublishResult::Stale);
        };
        if head.is_some_and(|head| head > generation) {
            return Ok(PlanPublishResult::Stale);
        }
        let Some(header) = generation_header(&transaction, &basis.dataset, id)? else {
            return Ok(PlanPublishResult::Stale);
        };
        if header.basis != *basis {
            return Ok(PlanPublishResult::Stale);
        }
        if header.phase == "published" && head == Some(generation) {
            return Ok(PlanPublishResult::AlreadyPublished);
        }
        if header.phase != "staging" {
            return Ok(PlanPublishResult::Stale);
        }
        if header.staged_entries != header.expected_entries {
            return Ok(PlanPublishResult::Incomplete);
        }
        transaction
            .execute(
                "UPDATE study_plan_generations SET phase='published' WHERE generation=?1",
                [id],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "UPDATE study_projection_state SET head_generation=?2 WHERE dataset_id=?1",
                params![basis.dataset.0, id],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)?;
        Ok(PlanPublishResult::Published)
    }

    /// Read the single published head. Staged and reclaiming rows are invisible.
    pub fn study_plan_head(&self, dataset: &DatasetId) -> Result<Option<PlanHead>, StoreError> {
        published_head(&self.connection, dataset)
    }

    /// Read at most 32 ordered candidate rows from the named compatible head.
    /// `None` means there is no usable matching head. An empty vector means the
    /// index has no ready rows under that head; caller still owns validity.
    pub fn ready_study_plan_entries(
        &self,
        dataset: &DatasetId,
        generation: PlanGeneration,
        today: StudyDay,
    ) -> Result<Option<Vec<PlanEntry>>, StoreError> {
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
                .map_err(database_error)?;
        let id = sqlite_count(generation.0)?;
        let usable: bool = transaction
            .query_row(
                "SELECT EXISTS(
               SELECT 1 FROM store_state s JOIN study_projection_state p USING(dataset_id)
               JOIN study_plan_generations g
                 ON (g.dataset_id,g.generation)=(p.dataset_id,p.head_generation)
               WHERE s.singleton=1 AND s.dataset_id=?1 AND g.generation=?2 AND g.phase='published'
                 AND p.study_day=?3 AND g.study_day=?3 AND p.membership_epoch=g.membership_epoch
             )",
                params![dataset.0, id, today.0],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        if !usable {
            return Ok(None);
        }
        let rows = read_selected(
            &transaction,
            READY_PLAN_SQL,
            params![id, CANDIDATE_LIMIT],
            |row| read_ready_row(row, today),
        )?;
        transaction.commit().map_err(database_error)?;
        Ok(Some(rows))
    }

    /// Reclaim a superseded or explicitly abandoned generation in short turns.
    /// The caller must have released any builder ownership of an abandoned stage.
    pub fn reclaim_study_plan(
        &mut self,
        dataset: &DatasetId,
        generation: PlanGeneration,
    ) -> Result<PlanReclaimResult, StoreError> {
        let id = sqlite_count(generation.0)?;
        let transaction = self.plan_write()?;
        let protected: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM study_projection_state
                 WHERE dataset_id=?1 AND head_generation=?2)",
                params![dataset.0, id],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        if protected {
            return Ok(PlanReclaimResult::Protected);
        }
        let Some(header) = generation_header(&transaction, dataset, id)? else {
            return Ok(PlanReclaimResult::Missing);
        };
        if header.phase != "reclaiming" {
            transaction
                .execute(
                    "UPDATE study_plan_generations SET phase='reclaiming' WHERE generation=?1",
                    [id],
                )
                .map_err(database_error)?;
        }
        let deleted = transaction
            .execute(RECLAIM_PLAN_SQL, params![id, PLAN_ENTRY_CHUNK])
            .map_err(database_error)?;
        let result = if deleted == 0 {
            transaction
                .execute(
                    "DELETE FROM study_plan_generations WHERE generation=?1",
                    [id],
                )
                .map_err(database_error)?;
            PlanReclaimResult::Reclaimed
        } else {
            PlanReclaimResult::Pending
        };
        transaction.commit().map_err(database_error)?;
        Ok(result)
    }

    fn plan_write(&mut self) -> Result<Transaction<'_>, StoreError> {
        self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)
    }
}

struct ProjectionState {
    day: Option<StudyDay>,
    membership_epoch: MembershipEpoch,
    rank_revision: RankRevision,
    summary_epoch: SummaryEpoch,
    summary_dirty: bool,
}

impl ProjectionState {
    fn basis(&self, dataset: &DatasetId) -> Option<StudyProjectionBasis> {
        let day = self.day?;
        Some(StudyProjectionBasis {
            plan: PlanBasis {
                dataset: dataset.clone(),
                day,
                membership_epoch: self.membership_epoch,
                rank_revision: self.rank_revision,
            },
            summary: SummaryBasis {
                dataset: dataset.clone(),
                day,
                epoch: self.summary_epoch,
            },
            summary_dirty: self.summary_dirty,
        })
    }
}

fn projection_state(
    connection: &Connection,
    dataset: &DatasetId,
) -> Result<Option<ProjectionState>, StoreError> {
    let row = connection
        .query_row(
            "SELECT p.study_day,p.membership_epoch,p.rank_revision,p.summary_epoch,
                    p.summary_dirty
             FROM store_state s JOIN study_projection_state p USING(dataset_id)
             WHERE s.singleton=1 AND s.dataset_id=?1",
            [&dataset.0],
            |row| {
                Ok((
                    row.get::<_, Option<i32>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, bool>(4)?,
                ))
            },
        )
        .optional()
        .map_err(database_error)?;
    row.map(|(day, membership, rank, summary, summary_dirty)| {
        Ok(ProjectionState {
            day: day.map(StudyDay),
            membership_epoch: MembershipEpoch(decode_counter(membership)?),
            rank_revision: RankRevision(decode_counter(rank)?),
            summary_epoch: SummaryEpoch(decode_counter(summary)?),
            summary_dirty,
        })
    })
    .transpose()
}

fn basis_is_current(connection: &Connection, basis: &PlanBasis) -> Result<bool, StoreError> {
    Ok(head_at_basis(connection, basis)?.is_some())
}

/// The outer option proves the basis still matches. The inner option is the
/// currently published generation, which may be absent.
fn head_at_basis(
    connection: &Connection,
    basis: &PlanBasis,
) -> Result<Option<Option<PlanGeneration>>, StoreError> {
    let row = connection
        .query_row(
            "SELECT p.head_generation
         FROM store_state s JOIN study_projection_state p USING(dataset_id)
         WHERE s.singleton=1 AND s.dataset_id=?1 AND p.study_day=?2
           AND p.membership_epoch=?3 AND p.rank_revision=?4",
            params![
                basis.dataset.0,
                basis.day.0,
                basis.membership_epoch.0.to_be_bytes().as_slice(),
                basis.rank_revision.0.to_be_bytes().as_slice()
            ],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()
        .map_err(database_error)?;
    row.map(|head| head.map(read_generation).transpose())
        .transpose()
}

fn published_head(
    connection: &Connection,
    dataset: &DatasetId,
) -> Result<Option<PlanHead>, StoreError> {
    let row = connection
        .query_row(
            "SELECT g.generation,g.study_day,g.membership_epoch,g.rank_revision
             FROM store_state s JOIN study_projection_state p USING(dataset_id)
             JOIN study_plan_generations g
               ON (g.dataset_id,g.generation)=(p.dataset_id,p.head_generation)
             WHERE s.singleton=1 AND s.dataset_id=?1 AND g.phase='published'",
            [&dataset.0],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(database_error)?;
    row.map(|(generation, day, membership, rank)| {
        Ok(PlanHead {
            generation: read_generation(generation)?,
            basis: PlanBasis {
                dataset: dataset.clone(),
                day: StudyDay(day),
                membership_epoch: MembershipEpoch(decode_counter(membership)?),
                rank_revision: RankRevision(decode_counter(rank)?),
            },
        })
    })
    .transpose()
}

struct GenerationHeader {
    basis: PlanBasis,
    expected_entries: u64,
    staged_entries: u64,
    phase: String,
}

fn generation_header(
    connection: &Connection,
    dataset: &DatasetId,
    generation: i64,
) -> Result<Option<GenerationHeader>, StoreError> {
    let row = connection
        .query_row(
            "SELECT study_day,membership_epoch,rank_revision,expected_entries,staged_entries,phase
             FROM study_plan_generations WHERE dataset_id=?1 AND generation=?2",
            params![dataset.0, generation],
            |row| {
                Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()
        .map_err(database_error)?;
    row.map(|(day, membership, rank, expected, staged, phase)| {
        Ok(GenerationHeader {
            basis: PlanBasis {
                dataset: dataset.clone(),
                day: StudyDay(day),
                membership_epoch: MembershipEpoch(decode_counter(membership)?),
                rank_revision: RankRevision(decode_counter(rank)?),
            },
            expected_entries: store_count(expected)?,
            staged_entries: store_count(staged)?,
            phase,
        })
    })
    .transpose()
}

fn validate_chunk(basis: &PlanBasis, start: u64, entries: &[PlanEntry]) -> Result<u64, StoreError> {
    if entries.is_empty() || entries.len() > PLAN_ENTRY_CHUNK {
        return Err(invalid("Study plan chunk must contain 1 to 127 entries"));
    }
    let mut ordinal = start;
    for entry in entries {
        if entry.work.day != basis.day || entry.ordinal != ordinal {
            return Err(invalid(
                "Study plan chunk must be consecutive within its captured day",
            ));
        }
        ordinal = ordinal
            .checked_add(1)
            .ok_or_else(|| invalid("Study plan ordinal overflow"))?;
    }
    Ok(ordinal)
}

fn priority_columns(priority: PlanPriority) -> (i64, Option<i32>) {
    match priority {
        PlanPriority::Correction => (0, None),
        PlanPriority::DueTest(day) => (1, Some(day.0)),
        PlanPriority::Teach => (2, None),
    }
}

fn work_kind_value(kind: WorkKind) -> &'static str {
    match kind {
        WorkKind::Teach => "teach",
        WorkKind::Test => "test",
        WorkKind::Correction => "correction",
    }
}

fn entry_state_value(state: PlanEntryState) -> &'static str {
    match state {
        PlanEntryState::Ready => "ready",
        PlanEntryState::Issued => "issued",
        PlanEntryState::Deferred => "deferred",
        PlanEntryState::Consumed => "consumed",
    }
}

fn read_ready_row(row: &rusqlite::Row<'_>, day: StudyDay) -> Result<PlanEntry, StoreError> {
    let kind = match row.get::<_, String>(2).map_err(database_error)?.as_str() {
        "teach" => WorkKind::Teach,
        "test" => WorkKind::Test,
        "correction" => WorkKind::Correction,
        _ => return Err(invalid("invalid stored Study plan work kind")),
    };
    let priority = match (
        row.get::<_, i64>(4).map_err(database_error)?,
        row.get::<_, Option<i32>>(5).map_err(database_error)?,
    ) {
        (0, None) => PlanPriority::Correction,
        (1, Some(due)) => PlanPriority::DueTest(StudyDay(due)),
        (2, None) => PlanPriority::Teach,
        _ => return Err(invalid("invalid stored Study plan priority")),
    };
    Ok(PlanEntry {
        work: WorkToken {
            day,
            obligation: WorkObligation {
                target: TargetId(row.get(0).map_err(database_error)?),
                version: ContentVersion(decode_counter(row.get(1).map_err(database_error)?)?),
                kind,
                revision: MissRevision(decode_counter(row.get(3).map_err(database_error)?)?),
            },
        },
        priority,
        ordinal: decode_counter(row.get(6).map_err(database_error)?)?,
        state: PlanEntryState::Ready,
    })
}

fn sqlite_count(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| invalid("Study plan count exceeds SQLite capacity"))
}

fn store_count(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| invalid("negative stored Study plan count"))
}

fn next_epoch(epoch: u64) -> Result<u64, StoreError> {
    epoch
        .checked_add(1)
        .ok_or_else(|| invalid("Study projection epoch overflow"))
}

fn read_generation(value: i64) -> Result<PlanGeneration, StoreError> {
    if value <= 0 {
        return Err(invalid("invalid stored Study plan generation"));
    }
    store_count(value).map(PlanGeneration)
}

#[cfg(test)]
pub(super) fn remove_schema_for_prior_version_fixture(connection: &Connection) {
    connection
        .execute_batch(
            "DROP TABLE study_deferrals;
             DROP TABLE study_day_authority;
             DROP TABLE study_projection_state;
             DROP TABLE study_plan_entries;
             DROP TABLE study_plan_generations;",
        )
        .unwrap();
}

#[cfg(test)]
mod tests;
