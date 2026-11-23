//! Read the required fields of one current or archived Study content row.

use super::{
    SqliteStore, database_error,
    row_values::{decode, decode_counter},
};
use crate::{
    CurrentStudyDefinition, IssuedStudyDefinition, StoreError, StudyContentFacts,
    StudyContentFamily, StudyContentRead, StudyLessonRead,
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::sync::LazyLock;
use tempo_study::Exercise;
use tempo_types::{ContentVersion, DatasetId, TargetId};

impl StudyContentRead for SqliteStore {
    fn study_content_facts(
        &self,
        dataset: &DatasetId,
        target: &TargetId,
    ) -> Result<Option<StudyContentFacts>, StoreError> {
        content_facts(&self.connection, dataset, target)
    }

    fn current_study_definition(
        &self,
        dataset: &DatasetId,
        target: &TargetId,
        version: ContentVersion,
        lesson: StudyLessonRead,
    ) -> Result<Option<CurrentStudyDefinition>, StoreError> {
        current_definition(&self.connection, dataset, target, version, lesson)
    }

    fn issued_study_definition(
        &self,
        dataset: &DatasetId,
        target: &TargetId,
        version: ContentVersion,
        lesson: StudyLessonRead,
    ) -> Result<Option<IssuedStudyDefinition>, StoreError> {
        issued_definition(&self.connection, dataset, target, version, lesson)
    }
}

// Use one compact projection for addressed checks and the coherent batch read.
// Repertoire identity comes from normalization at write time, and tactic
// membership is the typed value populated by ingress and schema 23.
pub(super) const STUDY_CONTENT_FACT_COLUMNS: &str =
    "c.target_id, c.version, c.repertoire, c.item -> '$.status',
     c.item -> '$.tactic_policy_facts',
     EXISTS (SELECT 1 FROM repertoire_suspensions s
             WHERE s.dataset_id=c.dataset_id AND s.repertoire_id=c.repertoire),
     EXISTS (SELECT 1 FROM study_exclusions x
             WHERE x.dataset_id=c.dataset_id AND x.target_id=c.target_id),
     EXISTS (SELECT 1 FROM tactical_qualifications q
             WHERE (q.dataset_id,q.target_id,q.version)=(c.dataset_id,c.target_id,c.version)
               AND q.result IN ('pending','withheld'))";

pub(super) fn content_facts(
    connection: &Connection,
    dataset: &DatasetId,
    target: &TargetId,
) -> Result<Option<StudyContentFacts>, StoreError> {
    static ADDRESSED_SQL: LazyLock<String> = LazyLock::new(|| {
        format!(
            "SELECT {STUDY_CONTENT_FACT_COLUMNS}
             FROM current_content c WHERE c.dataset_id=?1 AND c.target_id=?2"
        )
    });
    let mut statement = connection
        .prepare_cached(&ADDRESSED_SQL)
        .map_err(database_error)?;
    let mut rows = statement
        .query(params![dataset.0, target.0])
        .map_err(database_error)?;
    rows.next()
        .map_err(database_error)?
        .map(read_content_facts)
        .transpose()
}

pub(super) fn read_content_facts(row: &Row<'_>) -> Result<StudyContentFacts, StoreError> {
    let repertoire: Option<String> = row.get(2).map_err(database_error)?;
    let family = match repertoire {
        Some(repertoire_id) => StudyContentFamily::Repertoire { repertoire_id },
        None => {
            let membership: Option<String> = row.get(4).map_err(database_error)?;
            StudyContentFamily::Tactic {
                membership: decode_optional(membership.as_deref())?,
            }
        }
    };
    Ok(StudyContentFacts {
        target: TargetId(row.get(0).map_err(database_error)?),
        version: ContentVersion(decode_counter(row.get(1).map_err(database_error)?)?),
        family,
        status: decode(&row.get::<_, String>(3).map_err(database_error)?)?,
        suspended: row.get(5).map_err(database_error)?,
        excluded: row.get(6).map_err(database_error)?,
        qualification_held: row.get(7).map_err(database_error)?,
    })
}

pub(super) fn current_definition(
    connection: &Connection,
    dataset: &DatasetId,
    target: &TargetId,
    version: ContentVersion,
    lesson: StudyLessonRead,
) -> Result<Option<CurrentStudyDefinition>, StoreError> {
    let row = connection
        .query_row(
            "SELECT item -> '$.definition', CASE WHEN ?4 THEN item -> '$.lesson' END,
                    CASE WHEN repertoire IS NOT NULL AND json_type(item,'$.label')='text'
                         THEN item ->> '$.label' END
             FROM current_content WHERE dataset_id=?1 AND target_id=?2 AND version=?3",
            params![
                dataset.0,
                target.0,
                version.0.to_be_bytes().as_slice(),
                lesson == StudyLessonRead::Guided,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(database_error)?;
    row.map(|(definition, lesson, repertoire_label)| {
        Ok(CurrentStudyDefinition {
            exercise: exercise(target, version, &definition)?,
            repertoire_label,
            lesson: decode_optional(lesson.as_deref())?,
        })
    })
    .transpose()
}

pub(super) fn issued_definition(
    connection: &Connection,
    dataset: &DatasetId,
    target: &TargetId,
    version: ContentVersion,
    lesson: StudyLessonRead,
) -> Result<Option<IssuedStudyDefinition>, StoreError> {
    let row = connection
        .query_row(
            "SELECT item -> '$.definition', CASE WHEN ?4 THEN item -> '$.lesson' END
             FROM content_history WHERE dataset_id=?1 AND target_id=?2 AND version=?3",
            params![
                dataset.0,
                target.0,
                version.0.to_be_bytes().as_slice(),
                lesson == StudyLessonRead::Guided,
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(database_error)?;
    row.map(|(definition, lesson)| {
        Ok(IssuedStudyDefinition {
            exercise: exercise(target, version, &definition)?,
            legacy_lesson: decode_optional(lesson.as_deref())?,
        })
    })
    .transpose()
}

fn exercise(
    target: &TargetId,
    version: ContentVersion,
    definition: &str,
) -> Result<Exercise<tempo_chess::ChessDefinition>, StoreError> {
    Ok(Exercise {
        target: target.clone(),
        version,
        definition: decode(definition)?,
    })
}

fn decode_optional<T: serde::de::DeserializeOwned>(
    value: Option<&str>,
) -> Result<Option<T>, StoreError> {
    match value {
        Some(json) => decode(json),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Commit, Store, Write};
    use rusqlite::hooks::AuthAction;
    use tempo_chess::{ContentStatus, RepertoireLesson, demo_catalog, plan_import};
    use tempo_learning::{DailyLearning, LearningPolicy};

    fn fixture(directory: &tempfile::TempDir) -> SqliteStore {
        SqliteStore::create(
            directory.path().join("study-content.sqlite"),
            "dataset".into(),
            demo_catalog(),
            DailyLearning::default().settings(),
        )
        .unwrap()
    }

    #[test]
    fn selected_definition_projection_does_not_decode_required_answers_or_unrelated_rows() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = fixture(&directory);
        let dataset: DatasetId = "dataset".into();
        let selected: TargetId = "r1:p1".into();
        store
            .connection
            .execute_batch(
                "UPDATE current_content SET item='{' WHERE target_id='r1:p2';
                 UPDATE content_history SET item='{' WHERE target_id='r1:p2';
                 UPDATE current_content
                 SET item=json_set(item,'$.definition',json('false'))
                 WHERE target_id='r1:p1';
                 UPDATE content_history
                 SET item=json_set(item,'$.verified_tactic',json('false'),'$.lesson',json('false'))
                 WHERE target_id='r1:p1';",
            )
            .unwrap();

        let (facts, access) = super::super::operation_probe::probe_access(
            &mut store,
            |action| {
                !matches!(
                    action,
                    AuthAction::Read {
                        table_name: "current_content"
                            | "repertoire_suspensions"
                            | "study_exclusions"
                            | "tactical_qualifications",
                        ..
                    }
                )
            },
            |store| store.study_content_facts(&dataset, &selected),
        );
        let facts = facts.unwrap().unwrap();
        assert_eq!(facts.version, ContentVersion(1));
        assert_eq!(facts.status, ContentStatus::Active);
        assert_eq!(
            facts.family,
            StudyContentFamily::Repertoire {
                repertoire_id: "r1".into()
            }
        );
        assert!(access.denied.is_empty(), "{access:?}");
        assert!(
            store
                .current_study_definition(&dataset, &selected, facts.version, StudyLessonRead::Omit)
                .is_err()
        );
        let (issued, access) = super::super::operation_probe::probe_access(
            &mut store,
            |action| {
                !matches!(
                    action,
                    AuthAction::Read {
                        table_name: "content_history",
                        ..
                    }
                )
            },
            |store| {
                store.issued_study_definition(
                    &dataset,
                    &selected,
                    facts.version,
                    StudyLessonRead::Omit,
                )
            },
        );
        assert!(access.denied.is_empty(), "{access:?}");
        let issued = issued.unwrap().unwrap();
        assert!(issued.legacy_lesson.is_none());
        assert!(
            store
                .issued_study_definition(
                    &dataset,
                    &selected,
                    facts.version,
                    StudyLessonRead::Guided
                )
                .is_err()
        );
        assert_eq!(issued.exercise.target, selected);
        assert_eq!(issued.exercise.definition.decisions[0].expected.uci, "e2e4");
    }

    #[test]
    fn selected_definition_separates_current_presentation_from_issued_versions() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = fixture(&directory);
        let selected: TargetId = "r1:p1".into();
        let before = store.snapshot().unwrap();
        let dataset = before.dataset;
        let version = before.catalog.items[&selected].version;
        let original = store
            .issued_study_definition(&dataset, &selected, version, StudyLessonRead::Guided)
            .unwrap()
            .unwrap();
        assert!(original.legacy_lesson.is_none());

        let mut incoming = before.catalog.clone();
        let metadata = incoming.items.get_mut(&selected).unwrap();
        metadata.label = "Renamed repertoire".into();
        let lesson = RepertoireLesson {
            san: "e4".into(),
            comments: vec!["Reach the central pawn structure.".into()],
            previous_move: None,
        };
        metadata.lesson = Some(lesson.clone());
        let rename = plan_import(&before.catalog, &incoming).unwrap();
        assert_eq!(rename.catalog.items[&selected].version, version);
        store
            .commit(Commit {
                expected_dataset: dataset.clone(),
                expected_revision: before.revision,
                writes: vec![Write::Catalog(rename.catalog)],
            })
            .unwrap();
        let (current, access) = super::super::operation_probe::probe_access(
            &mut store,
            |action| {
                !matches!(
                    action,
                    AuthAction::Read {
                        table_name: "current_content",
                        ..
                    }
                )
            },
            |store| {
                store.current_study_definition(
                    &dataset,
                    &selected,
                    version,
                    StudyLessonRead::Guided,
                )
            },
        );
        let current = current.unwrap().unwrap();
        assert!(access.denied.is_empty(), "{access:?}");
        assert_eq!(current.exercise, original.exercise);
        assert_eq!(
            current.repertoire_label.as_deref(),
            Some("Renamed repertoire")
        );
        assert_eq!(current.lesson, Some(lesson));

        let renamed = store.snapshot().unwrap();
        let mut incoming = renamed.catalog.clone();
        let item = incoming.items.get_mut(&selected).unwrap();
        item.definition.decisions[0].expected = "d2d4".into();
        let edited = plan_import(&renamed.catalog, &incoming).unwrap();
        let new_version = edited.catalog.items[&selected].version;
        assert_ne!(new_version, version);
        store
            .commit(Commit {
                expected_dataset: dataset.clone(),
                expected_revision: renamed.revision,
                writes: vec![Write::Catalog(edited.catalog)],
            })
            .unwrap();
        assert!(
            store
                .current_study_definition(&dataset, &selected, version, StudyLessonRead::Guided)
                .unwrap()
                .is_none()
        );
        let current = store
            .current_study_definition(&dataset, &selected, new_version, StudyLessonRead::Guided)
            .unwrap()
            .unwrap();
        assert_eq!(
            current.exercise.definition.decisions[0].expected.uci,
            "d2d4"
        );
        assert_eq!(
            store
                .issued_study_definition(&dataset, &selected, version, StudyLessonRead::Guided)
                .unwrap(),
            Some(original)
        );
        assert!(
            store
                .issued_study_definition(
                    &"other-dataset".into(),
                    &selected,
                    version,
                    StudyLessonRead::Guided
                )
                .unwrap()
                .is_none()
        );
        store
            .connection
            .execute(
                "UPDATE current_content SET item=json_set(item,'$.lesson',json('false'))
             WHERE dataset_id=?1 AND target_id=?2",
                params![dataset.0, selected.0],
            )
            .unwrap();
        assert!(
            store
                .current_study_definition(&dataset, &selected, new_version, StudyLessonRead::Omit)
                .unwrap()
                .unwrap()
                .lesson
                .is_none()
        );
        assert!(
            store
                .current_study_definition(&dataset, &selected, new_version, StudyLessonRead::Guided)
                .is_err()
        );
    }

    #[test]
    fn selected_definition_admission_uses_current_typed_facts() {
        let directory = tempfile::tempdir().unwrap();
        let store = fixture(&directory);
        let dataset: DatasetId = "dataset".into();
        let repertoire: TargetId = "r1:p1".into();
        let tactic: TargetId = "t1".into();
        let before = store
            .study_content_facts(&dataset, &tactic)
            .unwrap()
            .unwrap();
        assert!(matches!(
            &before.family,
            StudyContentFamily::Tactic {
                membership: Some(_)
            }
        ));
        assert!(!before.suspended);
        assert!(!before.excluded);
        assert!(!before.qualification_held);
        store
            .connection
            .execute_batch(
                "INSERT INTO repertoire_suspensions VALUES ('dataset','r1');
             INSERT INTO study_exclusions
             SELECT dataset_id,target_id,version FROM current_content WHERE target_id='t1';
             INSERT INTO tactical_qualifications
             SELECT dataset_id,target_id,version,'pending' FROM current_content WHERE target_id='t1'
             ON CONFLICT(dataset_id,target_id) DO UPDATE SET result='pending';",
            )
            .unwrap();
        let suspended = store
            .study_content_facts(&dataset, &repertoire)
            .unwrap()
            .unwrap();
        assert!(suspended.suspended);
        assert!(!suspended.excluded);
        let held = store
            .study_content_facts(&dataset, &tactic)
            .unwrap()
            .unwrap();
        assert_eq!(held.family, before.family);
        assert!(!held.suspended);
        assert!(held.excluded);
        assert!(held.qualification_held);
        assert!(
            store
                .study_content_facts(&"other-dataset".into(), &tactic)
                .unwrap()
                .is_none()
        );
    }
}
