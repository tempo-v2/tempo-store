//! One active attempt and its immutable correction binding, when known.

use crate::{
    AcceptanceKind, ActivityAcceptance, ActivityMode, AssessmentKey, AssessmentRecord, StoreError,
    sqlite_store::{
        database_error,
        row_values::{decode, decode_counter},
    },
    store_error::invalid,
};
use rusqlite::{OptionalExtension, Transaction, params};
use tempo_learning::{AssessmentSource, CorrectionLifecycle, LearningEffect};
use tempo_study::AttemptStatus;
use tempo_types::{
    AttemptId, ContentVersion, DatasetId, Grade, MissRevision, StudyDay, WorkKind, WorkToken,
};

/// Captured identity of the attempt currently occupying the Study activity slot.
#[derive(Debug, PartialEq, Eq)]
pub enum ActiveStudyWork {
    None,
    /// An earlier release did not attach the exact issued obligation.
    LegacyTokenless {
        attempt_id: AttemptId,
    },
    Issued {
        attempt_id: AttemptId,
        issued: WorkToken,
        /// The same Test's first accepted miss may already own a correction.
        correction: Option<WorkToken>,
    },
}

pub(super) fn capture_active(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
) -> Result<ActiveStudyWork, StoreError> {
    let (
        active,
        target,
        version,
        issued,
        status,
        terminal,
        acceptance,
        mode,
        issued_day,
        issued_miss_revision,
    ) = tx
        .query_row(
            "SELECT s.active_attempt_id, a.target_id, a.version,
                a.record -> '$.issued_work', a.record -> '$.attempt.status',
                a.record -> '$.attempt.terminal', a.record -> '$.acceptance',
                a.record -> '$.mode', a.record -> '$.issued_at.day',
                a.record -> '$.issued_miss_revision'
         FROM store_state s
         LEFT JOIN attempts a ON (a.dataset_id,a.id)=(s.dataset_id,s.active_attempt_id)
         WHERE s.singleton=1 AND s.dataset_id=?1",
            [&dataset.0],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        )
        .map_err(database_error)?;
    let Some(attempt_id) = active.map(AttemptId) else {
        return Ok(ActiveStudyWork::None);
    };
    if target.is_none() || version.is_none() {
        return Err(invalid("missing addressed active Study attempt"));
    }
    let Some(raw) = issued.as_deref().filter(|raw| *raw != "null") else {
        return Ok(ActiveStudyWork::LegacyTokenless { attempt_id });
    };
    let issued: WorkToken = decode(raw)?;
    let invalid_identity = || invalid("incompatible captured active Study work");
    let mode: ActivityMode = decode(mode.as_deref().ok_or_else(invalid_identity)?)?;
    let expected_kind = match mode {
        ActivityMode::Teach => WorkKind::Teach,
        ActivityMode::Test => WorkKind::Test,
        ActivityMode::Repair => WorkKind::Correction,
        ActivityMode::Practice => return Err(invalid_identity()),
    };
    if issued.obligation.kind != expected_kind
        || target.as_deref() != Some(issued.obligation.target.0.as_str())
        || ContentVersion(decode_counter(version.ok_or_else(invalid_identity)?)?)
            != issued.obligation.version
        || decode::<StudyDay>(issued_day.as_deref().ok_or_else(invalid_identity)?)? != issued.day
        || decode::<MissRevision>(
            issued_miss_revision
                .as_deref()
                .ok_or_else(invalid_identity)?,
        )? != issued.obligation.revision
        || decode::<AttemptStatus>(status.as_deref().ok_or_else(invalid_identity)?)?
            != AttemptStatus::Active
    {
        return Err(invalid_identity());
    }
    let terminal: Option<Grade> = decode(terminal.as_deref().unwrap_or("null"))?;
    let correction = if has_bound_correction(&issued, terminal) {
        let acceptance: ActivityAcceptance =
            decode(acceptance.as_deref().ok_or_else(invalid_identity)?)?;
        Some(capture_bound_correction(
            tx,
            dataset,
            &attempt_id,
            &issued,
            &acceptance,
        )?)
    } else {
        None
    };
    Ok(ActiveStudyWork::Issued {
        attempt_id,
        issued,
        correction,
    })
}

pub(in crate::sqlite_store::study_plan) fn has_bound_correction(
    issued: &WorkToken,
    terminal: Option<Grade>,
) -> bool {
    issued.obligation.kind == WorkKind::Test && terminal == Some(Grade::Again)
}

pub(in crate::sqlite_store::study_plan) fn capture_bound_correction(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    id: &AttemptId,
    issued: &WorkToken,
    acceptance: &ActivityAcceptance,
) -> Result<WorkToken, StoreError> {
    let raw = tx
        .query_row(
            "SELECT record FROM assessments
         WHERE dataset_id=?1 AND attempt_id=?2 AND target_id=?3 AND version=?4",
            params![
                dataset.0,
                id.0,
                issued.obligation.target.0,
                issued.obligation.version.0.to_be_bytes().as_slice()
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(database_error)?
        .ok_or_else(|| invalid("missing accepted assessment for active Study correction"))?;
    let record: AssessmentRecord = decode(&raw)?;
    let assessment = &record.input;
    let transitioned = &record.transition.state;
    if &record.dataset != dataset
        || record.key != AssessmentKey::Human(id.clone())
        || record.target != issued.obligation.target
        || record.content_version != issued.obligation.version
        || assessment.grade != Grade::Again
        || assessment.accepted_at != acceptance.at
        || !matches!(
            acceptance.kind,
            AcceptanceKind::WrongMove { .. } | AcceptanceKind::Revealed { .. }
        )
        || !matches!(
            &assessment.source,
            AssessmentSource::Human { attempt_id, issued_miss_revision }
            if attempt_id == id && *issued_miss_revision == issued.obligation.revision
        )
        || record.transition.effect != LearningEffect::MissRecorded
        || transitioned.miss_revision <= issued.obligation.revision
        || !transitioned.correction.as_ref().is_some_and(|correction| {
            correction.lifecycle == CorrectionLifecycle::HumanTest
                && correction.answer_version == issued.obligation.version
        })
    {
        return Err(invalid(
            "incompatible accepted assessment for active Study correction",
        ));
    }
    let mut obligation = issued.obligation.clone();
    obligation.kind = WorkKind::Correction;
    obligation.revision = transitioned.miss_revision;
    Ok(WorkToken {
        day: assessment.accepted_at.day,
        obligation,
    })
}
