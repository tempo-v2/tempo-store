//! Indexed ready-prefix and exact candidate authority for prepared Next.

use super::{read_ready_row, sqlite_count};
use crate::sqlite_store::{
    SqliteStore, database_error,
    row_values::{decode, decode_counter, read_selected},
    study_content::{content_facts, current_definition},
};
use crate::{ActivityMode, CurrentStudyDefinition, StoreError, StudyLessonRead};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use tempo_chess::ContentStatus;
use tempo_learning::{DailyObligation, LearningState};
use tempo_types::{
    DatasetId, MembershipEpoch, PlanEntry, PlanGeneration, PlanPriority, StudyDay, TargetId,
};

const CANDIDATE_LIMIT: i64 = 32;
pub(super) const READY_PLAN_SQL: &str = "SELECT target_id,version,work_kind,obligation_revision,
                    priority_class,due_day,ordinal
             FROM study_plan_entries WHERE generation=?1 AND state='ready'
             ORDER BY priority_class,due_day,ordinal,target_id,version,work_kind,obligation_revision
             LIMIT ?2";

#[derive(Debug, PartialEq, Eq)]
pub enum PreparedStudyNext {
    Preparing,
    Exhausted,
    Candidate(Box<PreparedStudyCandidate>),
}

/// One selected version and its original addressed proof. Only Store can mint
/// a candidate after validating the named ready head.
#[derive(Debug, PartialEq, Eq)]
pub struct PreparedStudyCandidate {
    pub(super) dataset: DatasetId,
    pub(super) generation: PlanGeneration,
    pub(super) membership: MembershipEpoch,
    pub(super) entry: PlanEntry,
    pub(super) authority: CandidateAuthority,
    definition: CurrentStudyDefinition,
}

impl PreparedStudyCandidate {
    pub fn dataset_id(&self) -> &DatasetId {
        &self.dataset
    }

    pub fn entry(&self) -> &PlanEntry {
        &self.entry
    }

    pub fn definition(&self) -> &CurrentStudyDefinition {
        &self.definition
    }

    pub fn mode(&self) -> ActivityMode {
        self.authority.mode
    }

    pub fn first_introduction(&self) -> bool {
        self.authority.first_introduction
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CandidateAuthority {
    pub(super) mode: ActivityMode,
    pub(super) first_introduction: bool,
}

impl SqliteStore {
    /// Read at most 32 ordered ready rows from the named compatible head.
    pub fn ready_study_plan_entries(
        &self,
        dataset: &DatasetId,
        generation: PlanGeneration,
        day: StudyDay,
    ) -> Result<Option<Vec<PlanEntry>>, StoreError> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
            .map_err(database_error)?;
        let rows = if compatible_ready_head(&tx, dataset, generation, day)?.is_some() {
            Some(ready_entries(&tx, generation, day)?)
        } else {
            None
        };
        tx.commit().map_err(database_error)?;
        Ok(rows)
    }

    /// Validate one ready prefix and hydrate only the first compatible version.
    /// An inconclusive nonempty prefix cannot prove the plan is exhausted.
    pub fn prepared_study_next(
        &self,
        dataset: &DatasetId,
        generation: PlanGeneration,
        day: StudyDay,
    ) -> Result<PreparedStudyNext, StoreError> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
            .map_err(database_error)?;
        let result = match compatible_ready_head(&tx, dataset, generation, day)? {
            Some((membership, false)) => select_ready(&tx, dataset, generation, day, membership)?,
            _ => PreparedStudyNext::Preparing,
        };
        tx.commit().map_err(database_error)?;
        Ok(result)
    }
}

fn select_ready(
    connection: &Connection,
    dataset: &DatasetId,
    generation: PlanGeneration,
    day: StudyDay,
    membership: MembershipEpoch,
) -> Result<PreparedStudyNext, StoreError> {
    let entries = ready_entries(connection, generation, day)?;
    if entries.is_empty() {
        return Ok(PreparedStudyNext::Exhausted);
    }
    for entry in entries {
        let Some(authority) = candidate_authority(connection, dataset, &entry)? else {
            continue;
        };
        let lesson = if authority.mode.guided() {
            StudyLessonRead::Guided
        } else {
            StudyLessonRead::Omit
        };
        let obligation = &entry.work.obligation;
        let Some(definition) = current_definition(
            connection,
            dataset,
            &obligation.target,
            obligation.version,
            lesson,
        )?
        else {
            return Ok(PreparedStudyNext::Preparing);
        };
        return Ok(PreparedStudyNext::Candidate(Box::new(
            PreparedStudyCandidate {
                dataset: dataset.clone(),
                generation,
                membership,
                entry,
                authority,
                definition,
            },
        )));
    }
    Ok(PreparedStudyNext::Preparing)
}

fn ready_entries(
    connection: &Connection,
    generation: PlanGeneration,
    day: StudyDay,
) -> Result<Vec<PlanEntry>, StoreError> {
    read_selected(
        connection,
        READY_PLAN_SQL,
        params![sqlite_count(generation.0)?, CANDIDATE_LIMIT],
        |row| read_ready_row(row, day),
    )
}

/// Same current membership admits existing order through rank-only drift.
pub(super) fn compatible_ready_head(
    connection: &Connection,
    dataset: &DatasetId,
    generation: PlanGeneration,
    day: StudyDay,
) -> Result<Option<(MembershipEpoch, bool)>, StoreError> {
    let facts: Option<(Vec<u8>, bool)> = connection
        .query_row(
            "SELECT p.membership_epoch, s.active_attempt_id IS NOT NULL
         FROM store_state s JOIN study_projection_state p USING(dataset_id)
         JOIN study_plan_generations g
           ON (g.dataset_id,g.generation)=(p.dataset_id,p.head_generation)
         WHERE s.singleton=1 AND s.dataset_id=?1 AND g.generation=?2 AND g.phase='published'
           AND p.study_day=?3 AND g.study_day=?3 AND p.membership_epoch=g.membership_epoch",
            params![dataset.0, sqlite_count(generation.0)?, day.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(database_error)?;
    facts
        .map(|(epoch, active)| Ok((MembershipEpoch(decode_counter(epoch)?), active)))
        .transpose()
}

pub(super) fn candidate_authority(
    connection: &Connection,
    dataset: &DatasetId,
    entry: &PlanEntry,
) -> Result<Option<CandidateAuthority>, StoreError> {
    let work = &entry.work.obligation;
    let Some(content) = content_facts(connection, dataset, &work.target)? else {
        return Ok(None);
    };
    if content.version != work.version
        || content.status != ContentStatus::Active
        || content.suspended
        || content.excluded
        || content.qualification_held
    {
        return Ok(None);
    }
    let learning = read_learning(connection, &work.target)?;
    if learning.miss_revision != work.revision {
        return Ok(None);
    }
    let availability = learning.prepared_availability(work.version, entry.work.day);
    let mode = match entry.priority {
        PlanPriority::Correction if availability.obligation == DailyObligation::HumanCorrection => {
            ActivityMode::Repair
        }
        PlanPriority::DueTest(due)
            if availability.introduced_this_version
                && availability.scheduled_due == Some(due)
                && due <= entry.work.day =>
        {
            ActivityMode::Test
        }
        PlanPriority::Teach
            if !availability.introduced_this_version
                && availability.obligation == DailyObligation::None =>
        {
            ActivityMode::Teach
        }
        _ => return Ok(None),
    };
    Ok(Some(CandidateAuthority {
        mode,
        first_introduction: mode == ActivityMode::Teach && availability.first_introduced.is_none(),
    }))
}

fn read_learning(connection: &Connection, target: &TargetId) -> Result<LearningState, StoreError> {
    let state: String = connection
        .query_row(
            "SELECT state FROM learning WHERE target_id=?1",
            [&target.0],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    decode(&state)
}
