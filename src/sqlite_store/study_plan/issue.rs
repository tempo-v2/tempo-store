//! Apply only the selected prepared issue through a bounded SQLite CAS.

use super::{
    next_epoch, projection_state, read_ready_row,
    ready::{PreparedStudyCandidate, candidate_authority, compatible_ready_head},
    sqlite_count, work_kind_value,
};
use crate::{
    StoreError, StoredAttempt,
    sqlite_store::{
        SqliteStore, database_error,
        row_values::{decode_counter, encode},
    },
    store_error::invalid,
};
use rusqlite::params;
use tempo_study::AttemptStatus;
use tempo_types::{Revision, StudyDay};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreparedStudyIssue {
    Issued,
    Stale,
}

impl SqliteStore {
    /// Accept one exact issue selected through the addressed ready read.
    /// The caller supplies the freshly observed runtime day. Rank drift
    /// preserves existing order; a replaced head or membership does not.
    pub fn issue_prepared_study(
        &mut self,
        candidate: &PreparedStudyCandidate,
        mut record: StoredAttempt,
        today: StudyDay,
    ) -> Result<PreparedStudyIssue, StoreError> {
        let dataset = &candidate.dataset;
        let token = &candidate.entry.work;
        if today != token.day {
            return Ok(PreparedStudyIssue::Stale);
        }
        let obligation = &token.obligation;
        let attempt = &record.attempt;
        if &attempt.dataset != dataset
            || attempt.id.0.is_empty()
            || attempt.target != obligation.target
            || attempt.content_version != obligation.version
            || attempt.status != AttemptStatus::Active
            || attempt.revision != Revision(0)
            || attempt.terminal.is_some()
            || record.issued_at.day != today
            || record.issued_work.as_ref() != Some(token)
            || record.issued_miss_revision != obligation.revision
            || record.mode != candidate.mode()
            || record.first_introduction != candidate.first_introduction()
            || record.repertoire_warmup
            || record.acceptance.is_some()
            || record.revealed_decision.is_some()
            || !record.has_consistent_work_identity()
        {
            return Err(invalid("incompatible prepared Study issue"));
        }

        let tx = self.plan_write()?;
        if compatible_ready_head(&tx, dataset, candidate.generation, today)?
            != Some((candidate.membership, false))
        {
            return Ok(PreparedStudyIssue::Stale);
        }
        let generation = sqlite_count(candidate.generation.0)?;
        let version = obligation.version.0.to_be_bytes();
        let obligation_revision = obligation.revision.0.to_be_bytes();
        let key = params![
            generation,
            obligation.target.0,
            version.as_slice(),
            work_kind_value(obligation.kind),
            obligation_revision.as_slice(),
        ];
        let mut read = tx.prepare_cached(
            "SELECT target_id,version,work_kind,obligation_revision,priority_class,due_day,ordinal
             FROM study_plan_entries WHERE generation=?1 AND target_id=?2 AND version=?3
               AND work_kind=?4 AND obligation_revision=?5 AND state='ready'",
        ).map_err(database_error)?;
        let mut rows = read.query(key).map_err(database_error)?;
        let entry = rows
            .next()
            .map_err(database_error)?
            .map(|row| read_ready_row(row, today))
            .transpose()?;
        drop(rows);
        drop(read);
        if entry.as_ref() != Some(&candidate.entry)
            || candidate_authority(&tx, dataset, &candidate.entry)? != Some(candidate.authority)
        {
            return Ok(PreparedStudyIssue::Stale);
        }

        let revision: Vec<u8> = tx
            .query_row(
                "SELECT revision FROM store_state WHERE singleton=1 AND dataset_id=?1",
                [&dataset.0],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        let revision = decode_counter(revision)?;
        record.issued_revision = Some(Revision(revision));
        let issue_id = &record.attempt.id.0;
        tx.execute(
            "INSERT INTO attempts(id,dataset_id,target_id,version,record) VALUES (?1,?2,?3,?4,?5)",
            params![
                issue_id,
                dataset.0,
                obligation.target.0,
                version.as_slice(),
                encode(&record)?
            ],
        )
        .map_err(database_error)?;
        let pointer_changed = tx
            .execute(
                "UPDATE store_state SET active_attempt_id=?2,revision=?3
             WHERE singleton=1 AND dataset_id=?1 AND active_attempt_id IS NULL",
                params![
                    dataset.0,
                    issue_id,
                    next_epoch(revision)?.to_be_bytes().as_slice()
                ],
            )
            .map_err(database_error)?;
        let entry_changed = tx
            .execute(
                "UPDATE study_plan_entries SET state='issued'
             WHERE generation=?1 AND target_id=?2 AND version=?3
               AND work_kind=?4 AND obligation_revision=?5 AND state='ready'",
                key,
            )
            .map_err(database_error)?;
        if pointer_changed != 1 || entry_changed != 1 {
            return Ok(PreparedStudyIssue::Stale);
        }
        let epochs = projection_state(&tx, dataset)?
            .ok_or_else(|| invalid("missing Study projection authority"))?;
        // Occupying the active slot invalidates older staged builds. It does
        // not reorder the other ready rows, so membership is still servable.
        tx.execute(
            "UPDATE study_projection_state SET rank_revision=?2,summary_epoch=?3,summary_dirty=1
             WHERE dataset_id=?1",
            params![
                dataset.0,
                next_epoch(epochs.rank_revision.0)?.to_be_bytes().as_slice(),
                next_epoch(epochs.summary_epoch.0)?.to_be_bytes().as_slice(),
            ],
        )
        .map_err(database_error)?;
        tx.commit().map_err(database_error)?;
        Ok(PreparedStudyIssue::Issued)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AddressedResume, PreparedStudyNext, Store, test_support};
    use tempo_types::{
        ContentVersion, DatasetId, MissRevision, PlanBasis, PlanEntry, PlanEntryState,
        PlanGeneration, PlanPriority, WorkKind, WorkObligation, WorkToken,
    };

    const DAY: StudyDay = StudyDay(10);

    fn fixture() -> (tempfile::TempDir, SqliteStore, PlanBasis) {
        use tempo_learning::LearningPolicy;
        let directory = tempfile::tempdir().unwrap();
        let dataset = DatasetId::from("prepared-issue");
        let mut store = SqliteStore::create(
            directory.path().join("study.sqlite"),
            dataset.clone(),
            tempo_chess::demo_catalog(),
            test_support::policy().settings(),
        )
        .unwrap();
        let basis = store
            .observe_study_day(&dataset, DAY)
            .unwrap()
            .unwrap()
            .plan;
        (directory, store, basis)
    }

    fn teach(target: &str, ordinal: u64) -> PlanEntry {
        PlanEntry {
            work: WorkToken {
                day: DAY,
                obligation: WorkObligation {
                    target: target.into(),
                    version: ContentVersion(1),
                    kind: WorkKind::Teach,
                    revision: MissRevision(0),
                },
            },
            priority: PlanPriority::Teach,
            ordinal,
            state: PlanEntryState::Ready,
        }
    }

    fn publish(
        store: &mut SqliteStore,
        basis: &PlanBasis,
        entries: &[PlanEntry],
    ) -> PlanGeneration {
        use super::super::{PlanPublishResult, PlanStageResult};
        let count = entries.len() as u64;
        let generation = store.begin_study_plan(basis, count).unwrap().unwrap();
        if !entries.is_empty() {
            assert_eq!(
                store
                    .stage_study_plan(generation, basis, 0, entries)
                    .unwrap(),
                PlanStageResult::Staged {
                    next_ordinal: count
                },
            );
        }
        assert_eq!(
            store.publish_study_plan(generation, basis, DAY).unwrap(),
            PlanPublishResult::Published
        );
        generation
    }

    fn candidate(
        store: &SqliteStore,
        basis: &PlanBasis,
        head: PlanGeneration,
    ) -> Box<PreparedStudyCandidate> {
        match store
            .prepared_study_next(&basis.dataset, head, DAY)
            .unwrap()
        {
            PreparedStudyNext::Candidate(candidate) => candidate,
            outcome => panic!("expected an addressed candidate, got {outcome:?}"),
        }
    }

    #[test]
    fn prepared_next_uses_only_one_bounded_prefix_and_only_an_empty_index_proves_exhaustion() {
        let (_directory, mut store, basis) = fixture();
        let mut entries = (0..32)
            .map(|index| teach(&format!("missing-{index:02}"), index))
            .collect::<Vec<_>>();
        entries.push(teach("r1:p1", 32));
        let head = publish(&mut store, &basis, &entries);
        assert_eq!(
            store
                .ready_study_plan_entries(&basis.dataset, head, DAY)
                .unwrap()
                .unwrap()
                .len(),
            32
        );
        assert_eq!(
            store
                .prepared_study_next(&basis.dataset, head, DAY)
                .unwrap(),
            PreparedStudyNext::Preparing,
        );
        store
            .connection
            .execute(
                "UPDATE current_content SET item=json_set(item,'$.definition','malformed')
             WHERE target_id='t1'",
                [],
            )
            .unwrap();
        let head = publish(&mut store, &basis, &[teach("r1:p1", 0), teach("t1", 1)]);
        let selected = candidate(&store, &basis, head);
        assert_eq!(selected.definition().exercise.target.0, "r1:p1");
        let head = publish(&mut store, &basis, &[]);
        assert_eq!(
            store
                .prepared_study_next(&basis.dataset, head, DAY)
                .unwrap(),
            PreparedStudyNext::Exhausted,
        );
        assert_eq!(
            store
                .prepared_study_next(&basis.dataset, head, StudyDay(DAY.0 + 1))
                .unwrap(),
            PreparedStudyNext::Preparing,
        );
    }

    #[test]
    fn prepared_issue_accepts_rank_drift_and_fences_stage_without_spending() {
        use super::super::PlanPublishResult;
        let (_directory, mut store, basis) = fixture();
        let entry = teach("r1:p1", 0);
        let head = publish(&mut store, &basis, std::slice::from_ref(&entry));
        let selected = candidate(&store, &basis, head);
        let before = store.snapshot().unwrap();
        let mut record = test_support::issued_record(&before);
        record.issued_work = Some(entry.work);
        record.mode = crate::ActivityMode::Teach;
        record.first_introduction = true;
        store
            .connection
            .execute(
                "UPDATE study_projection_state SET rank_revision=?1",
                [1_u64.to_be_bytes().as_slice()],
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE store_state SET revision=?1",
                [20_u64.to_be_bytes().as_slice()],
            )
            .unwrap();
        let drift = store
            .study_projection_basis(&basis.dataset)
            .unwrap()
            .unwrap();
        let staging = store.begin_study_plan(&drift.plan, 0).unwrap().unwrap();
        assert_eq!(
            store
                .issue_prepared_study(&selected, record.clone(), DAY)
                .unwrap(),
            PreparedStudyIssue::Issued
        );
        assert_eq!(
            store
                .issue_prepared_study(&selected, record.clone(), DAY)
                .unwrap(),
            PreparedStudyIssue::Stale
        );
        assert_eq!(
            store.publish_study_plan(staging, &drift.plan, DAY).unwrap(),
            PlanPublishResult::Stale,
        );
        let after = store.snapshot().unwrap();
        assert_eq!(after.active_attempt, Some(record.attempt.id.clone()));
        assert_eq!(after.assessments, before.assessments);
        assert_eq!(after.receipts, before.receipts);
        assert_eq!(after.learning, before.learning);
        record.issued_revision = Some(Revision(20));
        assert_eq!(store.active_resume_state().unwrap().record, Some(record));
        assert_eq!(after.revision, Revision(21));
        let projected = store
            .study_projection_basis(&basis.dataset)
            .unwrap()
            .unwrap();
        assert_eq!(projected.plan.membership_epoch, drift.plan.membership_epoch);
        assert_eq!(
            projected.plan.rank_revision.0,
            drift.plan.rank_revision.0 + 1
        );
        assert_eq!(projected.summary.epoch.0, drift.summary.epoch.0 + 1);
        assert!(projected.summary_dirty);
        assert_eq!(
            store
                .ready_study_plan_entries(&basis.dataset, head, DAY)
                .unwrap(),
            Some(vec![])
        );
    }

    #[test]
    fn prepared_issue_rechecks_authority_and_rolls_back_an_interrupted_ready_transition() {
        let (_directory, mut store, basis) = fixture();
        let entry = teach("r1:p1", 0);
        let head = publish(&mut store, &basis, std::slice::from_ref(&entry));
        let selected = candidate(&store, &basis, head);
        let before = store.snapshot().unwrap();
        let mut record = test_support::issued_record(&before);
        record.issued_work = Some(entry.work);
        record.mode = crate::ActivityMode::Teach;
        record.first_introduction = true;

        store.connection.execute(
            "UPDATE learning SET state=json_set(state,'$.introduced',json('true')) WHERE target_id='r1:p1'",
            [],
        ).unwrap();
        assert_eq!(
            store
                .issue_prepared_study(&selected, record.clone(), DAY)
                .unwrap(),
            PreparedStudyIssue::Stale
        );
        store
            .connection
            .execute(
                "UPDATE learning SET state=?1 WHERE target_id='r1:p1'",
                [encode(&before.learning[&tempo_types::TargetId::from("r1:p1")]).unwrap()],
            )
            .unwrap();
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_issue BEFORE UPDATE OF state ON study_plan_entries
             BEGIN SELECT RAISE(ABORT,'fixture rejects issued state'); END;",
            )
            .unwrap();
        assert!(
            store
                .issue_prepared_study(&selected, record.clone(), DAY)
                .is_err()
        );
        assert_eq!(store.snapshot().unwrap(), before);
        assert_eq!(candidate(&store, &basis, head), selected);
        store
            .connection
            .execute_batch("DROP TRIGGER reject_issue")
            .unwrap();
        assert_eq!(
            store.issue_prepared_study(&selected, record, DAY).unwrap(),
            PreparedStudyIssue::Issued
        );
    }
}
