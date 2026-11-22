//! One-time normalization of current tactics during the v22-to-v23 upgrade.

use super::{
    database_error,
    row_values::{decode, decode_counter, encode, read_selected},
};
use crate::{StoreError, store_error::invalid};
use rusqlite::{Transaction, params};
use tempo_chess::{ContentItem, ContentKind, TacticPolicyFacts};

const PAGE_SIZE: i64 = 128;

pub(super) fn migrate(tx: &Transaction<'_>) -> Result<(), StoreError> {
    let mut cursor = None::<String>;
    loop {
        // Page by the existing primary index. Repertoire bodies are not read.
        let rows: Vec<(String, Vec<u8>, Option<String>)> = read_selected(
            tx,
            "SELECT target_id, version, CASE WHEN repertoire IS NULL THEN item END
             FROM current_content WHERE target_id >= ?1 AND (?2 OR target_id > ?1)
             ORDER BY target_id LIMIT ?3",
            params![
                cursor.as_deref().unwrap_or_default(),
                cursor.is_none(),
                PAGE_SIZE
            ],
            |row| {
                Ok((
                    row.get(0).map_err(database_error)?,
                    row.get(1).map_err(database_error)?,
                    row.get(2).map_err(database_error)?,
                ))
            },
        )?;
        if rows.is_empty() {
            return Ok(());
        }
        for (target, version, json) in rows {
            if let Some(json) = json {
                normalize(tx, &target, version, &json)?;
            }
            cursor = Some(target);
        }
    }
}

fn normalize(
    tx: &Transaction<'_>,
    target: &str,
    version: Vec<u8>,
    json: &str,
) -> Result<(), StoreError> {
    let item: ContentItem = decode(json)?;
    if target.is_empty()
        || !target.is_ascii()
        || item.target.0 != target
        || item.version.0 == 0
        || item.version.0 != decode_counter(version)?
        || !matches!(
            &item.definition.kind,
            ContentKind::Tactic { practice_start } if !practice_start.0.is_empty()
        )
    {
        return Err(invalid(
            "invalid current tactic identity during theme migration",
        ));
    }
    item.definition
        .validate()
        .map_err(|error| invalid(&error.to_string()))?;
    if item.tactic_policy_facts.is_none() {
        let facts =
            TacticPolicyFacts::from_legacy_metadata(item.provenance.iter().map(String::as_str));
        let updated = tx
            .execute(
                "UPDATE current_content
                 SET item=json_set(item, '$.tactic_policy_facts', json(?2))
                 WHERE target_id=?1",
                params![target, encode(&facts)?],
            )
            .map_err(database_error)?;
        if updated != 1 {
            return Err(invalid("current tactic was not normalized"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Commit, SqliteStore, Store, Write,
        test_support::{issued_record, policy, successful_answer_writes},
    };
    use rusqlite::Connection;
    use std::{collections::BTreeSet, path::Path};
    use tempo_chess::{ExactSequenceGrader, demo_catalog};
    use tempo_learning::LearningPolicy;
    use tempo_study::Grader;
    use tempo_types::TargetId;

    const MEMBERSHIP: &str = r#"tempo1:catalog-metadata:{"DeckId":"fork-focused-04","DeckPosition":9,"Difficulty":"focused","Motif":"fork"}"#;

    fn create_v22(path: &Path) -> SqliteStore {
        let mut catalog = demo_catalog();
        let normalized_empty = catalog.items[&TargetId::from("t1")]
            .tactic_policy_facts
            .clone();
        let tactic = catalog.items.get_mut(&TargetId::from("t1")).unwrap();
        tactic.tactic_policy_facts = None;
        tactic.provenance.push(MEMBERSHIP.into());
        let mut no_membership = tactic.clone();
        no_membership.target = "t2".into();
        no_membership.provenance.clear();
        let mut already_normalized = tactic.clone();
        already_normalized.target = "t3".into();
        already_normalized.tactic_policy_facts = normalized_empty;
        catalog
            .items
            .insert(no_membership.target.clone(), no_membership);
        catalog
            .items
            .insert(already_normalized.target.clone(), already_normalized);
        let mut store =
            SqliteStore::create(path, "dataset-a".into(), catalog, policy().settings()).unwrap();
        let initial = store.snapshot().unwrap();
        let issued = issued_record(&initial);
        store
            .commit(Commit {
                expected_dataset: initial.dataset,
                expected_revision: initial.revision,
                writes: vec![
                    Write::Issue(issued.clone()),
                    Write::Active(Some(issued.attempt.id)),
                ],
            })
            .unwrap();
        let issued = store.snapshot().unwrap();
        store
            .commit(Commit {
                expected_dataset: issued.dataset.clone(),
                expected_revision: issued.revision,
                writes: successful_answer_writes(&issued),
            })
            .unwrap();

        let answered = store.snapshot().unwrap();
        let mut pending = issued_record(&answered);
        pending.attempt.id = "prior-issued-tactic".into();
        pending.attempt.target = "t1".into();
        let tactic = &answered.catalog.items[&pending.attempt.target];
        let start = ExactSequenceGrader.start(&tactic.definition).unwrap();
        pending.attempt.state = start.state;
        pending.attempt.prompt = Some(start.prompt);
        pending.attempt.content_version = tactic.version;
        pending.issued_miss_revision = answered.learning[&pending.attempt.target].miss_revision;
        store
            .commit(Commit {
                expected_dataset: answered.dataset,
                expected_revision: answered.revision,
                writes: vec![
                    Write::Issue(pending.clone()),
                    Write::Active(Some(pending.attempt.id)),
                ],
            })
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE current_content SET item=json_set(item,'$.retained_extension',json('[1]'))
                 WHERE target_id='t1'",
                [],
            )
            .unwrap();
        store
            .connection
            .pragma_update(None, "user_version", 22)
            .unwrap();
        store
    }

    fn retained_rows(db: &Connection) -> Vec<Vec<String>> {
        [
            "SELECT json_array(id, record) FROM attempts ORDER BY id",
            "SELECT json_array(ordinal, item) FROM content_history ORDER BY ordinal",
            "SELECT json_array(id, command, result) FROM receipts ORDER BY id",
            "SELECT json_array(ordinal, record) FROM assessments ORDER BY ordinal",
            "SELECT json_array(target_id, state) FROM learning ORDER BY target_id",
            "SELECT json_array(active_attempt_id, hex(revision), content_revision) FROM store_state",
            "SELECT item FROM current_content WHERE repertoire IS NOT NULL OR target_id='t3'
             ORDER BY target_id",
        ]
        .into_iter()
        .map(|sql| read_selected(db, sql, [], |row| row.get(0).map_err(database_error)).unwrap())
        .collect()
    }

    #[test]
    fn tactic_policy_migration_preserves_retained_bytes_and_normalizes_current_targets() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("study.sqlite");
        let old = create_v22(&path);
        let retained = retained_rows(&old.connection);
        assert_eq!(retained[0].len(), 2);
        assert_eq!(retained[2].len(), 1);
        drop(old);

        let upgraded = SqliteStore::open(&path).unwrap();
        assert_eq!(
            super::super::schema_version(&upgraded.connection).unwrap(),
            23
        );
        let snapshot = upgraded.snapshot().unwrap();
        let items = &snapshot.catalog.items;
        assert_eq!(
            items[&TargetId::from("t1")]
                .tactic_policy_facts
                .as_ref()
                .unwrap()
                .theme_ids(),
            &BTreeSet::from(["fork".into()]),
        );
        for target in ["t2", "t3"] {
            assert!(
                items[&TargetId::from(target)]
                    .tactic_policy_facts
                    .as_ref()
                    .unwrap()
                    .theme_ids()
                    .is_empty()
            );
        }
        let extension: String = upgraded.connection.query_row(
            "SELECT json_extract(item,'$.retained_extension') FROM current_content WHERE target_id='t1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(extension, "[1]");
        assert_eq!(retained_rows(&upgraded.connection), retained);
        drop(upgraded);
        assert_eq!(
            SqliteStore::open(&path).unwrap().snapshot().unwrap(),
            snapshot
        );
    }

    #[test]
    fn tactic_policy_migration_rolls_back_a_malformed_required_row() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("study.sqlite");
        let old = create_v22(&path);
        old.connection
            .execute(
                "UPDATE current_content SET item='{}' WHERE target_id='t2'",
                [],
            )
            .unwrap();
        let current_rows = |db: &Connection| {
            read_selected::<String, Vec<_>>(
                db,
                "SELECT item FROM current_content ORDER BY target_id",
                [],
                |row| row.get(0).map_err(database_error),
            )
            .unwrap()
        };
        let current = current_rows(&old.connection);
        let retained = retained_rows(&old.connection);
        drop(old);

        assert!(SqliteStore::open(&path).is_err());
        let db = Connection::open(&path).unwrap();
        assert_eq!(super::super::schema_version(&db).unwrap(), 22);
        assert_eq!(current_rows(&db), current);
        assert_eq!(retained_rows(&db), retained);
    }
}
