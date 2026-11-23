//! Read one attempt and its current compatibility facts without loading Study history.

use super::{SqliteStore, database_error, row_values};
use crate::{AddressedResume, ResumeState, ResumeTarget, StoreError};
use tempo_types::{AttemptId, ContentVersion, DatasetId};

impl AddressedResume for SqliteStore {
    fn resume_state(&self, id: &AttemptId) -> Result<ResumeState, StoreError> {
        let (dataset, active, record, target) = self
            .connection
            .query_row(
                "SELECT s.dataset_id, s.active_attempt_id, a.record, c.version,
                        json_quote(json_extract(c.item, '$.status')),
                        EXISTS (SELECT 1 FROM study_exclusions x
                                WHERE x.dataset_id=s.dataset_id AND x.target_id=c.target_id),
                        EXISTS (SELECT 1 FROM repertoire_suspensions r
                                WHERE r.dataset_id=s.dataset_id AND r.repertoire_id=c.repertoire)
                 FROM store_state s
                 LEFT JOIN attempts a ON a.id=?1
                 LEFT JOIN current_content c ON c.target_id=a.target_id AND c.dataset_id=s.dataset_id
                 WHERE s.singleton=1",
                [&id.0],
                |row| {
                    let version: Option<Vec<u8>> = row.get(3)?;
                    let target = version
                        .map(|version| Ok::<_, rusqlite::Error>((version, row.get::<_, String>(4)?, row.get(5)?, row.get(6)?)))
                        .transpose()?;
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        target,
                    ))
                },
            )
            .map_err(database_error)?;
        let target = target
            .map(|(version, status, excluded, suspended)| {
                Ok::<_, StoreError>(ResumeTarget {
                    version: ContentVersion(row_values::decode_counter(version)?),
                    status: row_values::decode(&status)?,
                    excluded,
                    suspended,
                })
            })
            .transpose()?;
        Ok(ResumeState {
            dataset: DatasetId(dataset),
            active_attempt: active.map(AttemptId),
            record: record.as_deref().map(row_values::decode).transpose()?,
            target,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Commit, Store, Write, test_support};
    use rusqlite::hooks::AuthAction;
    use tempo_learning::LearningPolicy;

    #[test]
    fn addressed_resume_reads_neither_learning_nor_assessment_history() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = SqliteStore::create(
            directory.path().join("resume.sqlite"),
            "fixture".into(),
            tempo_chess::demo_catalog(),
            test_support::policy().settings(),
        )
        .unwrap();
        let snapshot = store.snapshot().unwrap();
        let issued = test_support::issued_record(&snapshot);
        let id = issued.attempt.id.clone();
        store
            .commit(Commit {
                expected_revision: snapshot.revision,
                expected_dataset: snapshot.dataset,
                writes: vec![
                    Write::Issue(issued.clone()),
                    Write::Active(Some(id.clone())),
                ],
            })
            .unwrap();
        store
            .connection
            .execute_batch(
                "UPDATE learning SET state='malformed';
             UPDATE store_state SET policy='malformed' WHERE singleton=1;",
            )
            .unwrap();

        let (result, access) = super::super::operation_probe::probe_access(
            &mut store,
            |action| {
                !matches!(action, AuthAction::Read {
                table_name: "store_state" | "attempts" | "current_content" | "study_exclusions" |
                    "repertoire_suspensions",
                column_name,
            } if column_name != "policy")
            },
            |store| store.resume_state(&id),
        );
        let state = result.unwrap();
        assert!(access.denied.is_empty());
        assert_eq!(state.active_attempt.as_ref(), Some(&id));
        assert_eq!(state.record.as_ref(), Some(&issued));
        assert_eq!(
            state.target.unwrap().version,
            issued.attempt.content_version
        );
        assert!(
            store
                .resume_state(&"missing".into())
                .unwrap()
                .record
                .is_none()
        );
    }
}
