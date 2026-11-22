//! Convert row payloads without changing the public domain or wire types.

use super::database_error;
use crate::{AnswerResult, ReceiptResult, StoreError, store_error::invalid};
use rusqlite::{Connection, Params, Row, Transaction};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tempo_types::StudyDay;

pub(super) fn encode(value: &impl Serialize) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|error| invalid(&format!("encode SQLite value: {error}")))
}

pub(super) fn decode<T: DeserializeOwned>(value: &str) -> Result<T, StoreError> {
    serde_json::from_str(value).map_err(|error| invalid(&format!("decode SQLite value: {error}")))
}

pub(super) const STUDY_RECEIPT_VERSION: u32 = 1;

#[derive(Deserialize, Serialize)]
enum StoredStudyReceipt<T> {
    Study(VersionedStudyOutcome<T>),
    ResumeSkipped(VersionedStudyOutcome<StoredResumeSkipped>),
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredResumeSkipped {
    day: StudyDay,
    cutoff: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct VersionedStudyOutcome<T> {
    receipt_version: u32,
    outcome: T,
}

pub(super) fn encode_receipt(result: &ReceiptResult) -> Result<String, StoreError> {
    match result {
        ReceiptResult::Study(answer) => encode(&StoredStudyReceipt::Study(VersionedStudyOutcome {
            receipt_version: STUDY_RECEIPT_VERSION,
            outcome: answer,
        })),
        ReceiptResult::ResumeSkipped { day, cutoff } => {
            encode(&StoredStudyReceipt::<&AnswerResult>::ResumeSkipped(VersionedStudyOutcome {
                receipt_version: STUDY_RECEIPT_VERSION,
                outcome: StoredResumeSkipped { day: *day, cutoff: *cutoff },
            }))
        }
        _ => encode(result),
    }
}

pub(super) fn receipt_value(row: &Row<'_>, column: usize) -> Result<ReceiptResult, StoreError> {
    decode_receipt(&row.get::<_, String>(column).map_err(database_error)?)
}

pub(super) fn decode_receipt(encoded: &str) -> Result<ReceiptResult, StoreError> {
    let stored: Value = decode(encoded)?;
    if stored.get("ResumeSkipped").is_none()
        && stored.get("Study").and_then(|study| study.get("receipt_version")).is_none()
    {
        // The installed legacy format stores AnswerResult directly under Study.
        return decode(encoded);
    }
    let (version, result) = match decode::<StoredStudyReceipt<AnswerResult>>(encoded)? {
        StoredStudyReceipt::Study(receipt) => {
            (receipt.receipt_version, ReceiptResult::Study(receipt.outcome))
        }
        StoredStudyReceipt::ResumeSkipped(receipt) => (
            receipt.receipt_version,
            ReceiptResult::ResumeSkipped { day: receipt.outcome.day, cutoff: receipt.outcome.cutoff },
        ),
    };
    if version != STUDY_RECEIPT_VERSION {
        return Err(invalid("unsupported stored Study receipt version"));
    }
    Ok(result)
}

pub(super) fn decode_counter(value: Vec<u8>) -> Result<u64, StoreError> {
    let bytes = value
        .try_into()
        .map_err(|_| invalid("SQLite counter must contain eight bytes"))?;
    Ok(u64::from_be_bytes(bytes))
}

pub(super) fn row_index(value: usize) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| invalid("SQLite row order exceeds capacity"))
}

pub(super) fn read_rows<T, C: FromIterator<T>>(
    transaction: &Transaction<'_>,
    sql: &str,
    read: impl FnMut(&Row<'_>) -> Result<T, StoreError>,
) -> Result<C, StoreError> {
    read_selected(transaction, sql, [], read)
}

pub(super) fn read_selected<T, C: FromIterator<T>>(
    connection: &Connection,
    sql: &str,
    params: impl Params,
    read: impl FnMut(&Row<'_>) -> Result<T, StoreError>,
) -> Result<C, StoreError> {
    let mut statement = connection.prepare_cached(sql).map_err(database_error)?;
    let mut rows = statement.query(params).map_err(database_error)?;
    let mut read = read;
    std::iter::from_fn(|| {
        rows.next()
            .transpose()
            .map(|row| row.map_err(database_error).and_then(&mut read))
    })
    .collect()
}

pub(super) fn text_value<T: DeserializeOwned>(
    row: &Row<'_>,
    column: usize,
) -> Result<T, StoreError> {
    decode(&row.get::<_, String>(column).map_err(database_error)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_store::SqliteStore;
    use crate::{
        AssessmentKey, AssessmentRecord, Commit, Receipt, Store, StudyScope, Write,
        test_support::{issued_record, policy},
    };
    use rusqlite::params;
    use serde_json::json;
    use tempo_chess::{ExactSequenceGrader, demo_catalog};
    use tempo_learning::{
        AssessmentInput, AssessmentSource, LearningEffect, LearningPolicy, Transition,
    };
    use tempo_types::{Grade, TargetId};

    fn committed_miss(directory: &tempfile::TempDir) -> (SqliteStore, Receipt) {
        let schedule = policy();
        let mut store = SqliteStore::create(
            directory.path().join("study.sqlite"),
            "receipt-fixture".into(),
            demo_catalog(),
            schedule.settings(),
        )
        .unwrap();
        let initial = store.snapshot().unwrap();
        let mut issued = issued_record(&initial);
        store
            .commit(Commit {
                expected_dataset: initial.dataset,
                expected_revision: initial.revision,
                writes: vec![
                    Write::Issue(issued.clone()),
                    Write::Active(Some(issued.attempt.id.clone())),
                ],
            })
            .unwrap();
        let before = store.snapshot().unwrap();
        let target = issued.attempt.target.clone();
        let progress = tempo_study::submit(
            &ExactSequenceGrader,
            &before.catalog.items[&target].definition,
            &issued.attempt,
            issued.attempt.revision,
            &"d2d4".into(),
        )
        .unwrap();
        let input = AssessmentInput {
            grade: Grade::Again,
            accepted_at: issued.issued_at,
            source: AssessmentSource::Human {
                attempt_id: issued.attempt.id.clone(),
                issued_miss_revision: issued.issued_miss_revision,
            },
        };
        let transition = schedule.apply(&before.learning[&target], &input).unwrap();
        let receipt = Receipt {
            id: "submission-1".into(),
            command: r#"["answer",{"dataset_id":"receipt-fixture","attempt_id":"attempt-1","submission_id":"submission-1","expected_revision":0,"answer":{"uci":"d2d4"}}]"#.into(),
            result: ReceiptResult::Study(AnswerResult {
                attempt: progress.attempt.clone(),
                feedback: Some(progress.feedback),
                assessment: progress.assessment,
                learning_effect: Some(transition.effect),
            }),
        };
        let record = AssessmentRecord {
            key: AssessmentKey::Human(progress.attempt.id.clone()),
            dataset: before.dataset.clone(),
            target: target.clone(),
            content_version: progress.attempt.content_version,
            input,
            transition: transition.clone(),
        };
        issued.attempt = progress.attempt;
        store
            .commit(Commit {
                expected_dataset: before.dataset,
                expected_revision: before.revision,
                writes: vec![
                    Write::Progress(issued),
                    Write::Learning {
                        target,
                        state: transition.state,
                    },
                    Write::Assessment(record),
                    Write::Receipt(receipt.clone()),
                ],
            })
            .unwrap();
        (store, receipt)
    }

    fn replace_receipt_fixture(store: &SqliteStore, receipt: &Receipt, bytes: &str) {
        store
            .connection
            .execute(
                "UPDATE receipts SET result=?2 WHERE id=?1",
                params![receipt.id.0, bytes],
            )
            .unwrap();
    }

    #[test]
    fn persisted_contract_round_trips_study_receipts_without_rewriting_legacy_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let (store, receipt) = committed_miss(&directory);
        let read_bytes = |store: &SqliteStore| -> (String, String) {
            store
                .connection
                .query_row(
                    "SELECT command, result FROM receipts WHERE id=?1",
                    [&receipt.id.0],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap()
        };
        let (fingerprint, versioned) = read_bytes(&store);
        assert_eq!(fingerprint, receipt.command);
        let parsed: Value = decode(&versioned).unwrap();
        assert_eq!(parsed["Study"]["receipt_version"], 1);
        assert_eq!(parsed["Study"]["outcome"], json!(&receipt.result)["Study"]);
        let scope = StudyScope {
            submission: Some(&receipt.id),
            ..StudyScope::default()
        };
        assert_eq!(
            store.study_state(scope).unwrap().receipts[&receipt.id],
            receipt
        );
        let before = store.snapshot().unwrap();
        assert_eq!(before.receipts[&receipt.id], receipt);

        // Whitespace demonstrates that reading and subsequent writes do not
        // normalize old result bytes or regenerate the original fingerprint.
        let legacy = format!(" {}\n", encode(&receipt.result).unwrap());
        replace_receipt_fixture(&store, &receipt, &legacy);
        drop(store);
        let mut reopened = SqliteStore::open(directory.path().join("study.sqlite")).unwrap();
        assert_eq!(
            reopened.study_state(scope).unwrap().receipts[&receipt.id],
            receipt
        );
        assert_eq!(reopened.snapshot().unwrap(), before);

        let mut catalog = before.catalog;
        let item = catalog.items.get_mut(&TargetId::from("r1:p1")).unwrap();
        item.label = "Edited after original acceptance".into();
        reopened
            .commit(Commit {
                expected_dataset: before.dataset,
                expected_revision: before.revision,
                writes: vec![Write::Catalog(catalog)],
            })
            .unwrap();
        assert_eq!(read_bytes(&reopened), (fingerprint, legacy));
        let after = reopened.snapshot().unwrap();
        assert_eq!(after.assessments, before.assessments);
        assert_eq!(after.learning, before.learning);
        assert_eq!(after.receipts[&receipt.id], receipt);
    }

    #[test]
    fn persisted_contract_rejects_unknown_envelopes_and_leaves_other_families_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let (store, receipt) = committed_miss(&directory);
        let legacy: Value = decode(&encode(&receipt.result).unwrap()).unwrap();
        let mut wrong_version = legacy.clone();
        wrong_version["Study"]["receipt_version"] = json!(2);
        let mut null_version = legacy.clone();
        null_version["Study"]["receipt_version"] = Value::Null;
        let mut missing_outcome = legacy;
        missing_outcome["Study"]["receipt_version"] = json!(1);
        let duplicate_version = encode_receipt(&receipt.result).unwrap().replacen(
            r#""receipt_version":1"#,
            r#""receipt_version":2,"receipt_version":1"#,
            1,
        );
        let wrong = [
            wrong_version.to_string(),
            null_version.to_string(),
            missing_outcome.to_string(),
            duplicate_version,
        ];
        for bytes in wrong {
            replace_receipt_fixture(&store, &receipt, &bytes);
            assert!(
                store
                    .study_state(StudyScope {
                        submission: Some(&receipt.id),
                        ..StudyScope::default()
                    })
                    .is_err(),
                "{bytes}"
            );
            assert!(store.snapshot().is_err(), "{bytes}");
        }

        for result in [
            ReceiptResult::TacticsAllowance {
                reply: r#"{"schema_version":4}"#.into(),
            },
            ReceiptResult::External {
                target: "target".into(),
                transition: Transition {
                    state: Default::default(),
                    effect: LearningEffect::MissRecorded,
                },
            },
        ] {
            assert_eq!(encode_receipt(&result).unwrap(), encode(&result).unwrap());
            assert_eq!(
                decode_receipt(&encode_receipt(&result).unwrap()).unwrap(),
                result
            );
        }
    }

    #[test]
    fn persisted_contract_saved_reviews_reads_only_legacy_and_supported_versioned_feedback() {
        let directory = tempfile::tempdir().unwrap();
        let (store, receipt) = committed_miss(&directory);
        let ReceiptResult::Study(result) = &receipt.result else {
            unreachable!()
        };
        let attempt = &result.attempt;
        store.connection.execute(
            "UPDATE attempts SET record=json_remove(record,'$.issued_revision','$.revealed_decision')
             WHERE id=?1",
            [&attempt.id.0],
        ).unwrap();
        store.connection.execute(
            "INSERT INTO saved_reviews (id,dataset_id,target_id,version,decision_index,attempt_id,saved_at_ms)
             VALUES (1,?1,?2,?3,?4,?5,1)",
            params![
                attempt.dataset.0, attempt.target.0,
                attempt.content_version.0.to_be_bytes().as_slice(),
                attempt.state.decision_index, attempt.id.0,
            ],
        ).unwrap();

        let legacy = encode(&receipt.result).unwrap();
        let versioned = encode_receipt(&receipt.result).unwrap();
        let future = versioned.replace("\"receipt_version\":1", "\"receipt_version\":2");
        let null_version = versioned.replace("\"receipt_version\":1", "\"receipt_version\":null");
        let textual_version =
            versioned.replace(r#""receipt_version":1"#, r#""receipt_version":"1""#);
        for (bytes, expected) in [
            (legacy, true),
            (versioned, true),
            (future, false),
            (null_version, false),
            (textual_version, false),
        ] {
            replace_receipt_fixture(&store, &receipt, &bytes);
            let context = store
                .saved_review_context(&attempt.dataset, 1)
                .unwrap()
                .unwrap();
            assert_eq!(context.legacy_source_feedback, expected, "{bytes}");
        }
    }
}

impl crate::StudyReceiptRead for super::SqliteStore {
    fn study_receipt(
        &self,
        dataset: &tempo_types::DatasetId,
        id: &tempo_types::SubmissionId,
    ) -> Result<crate::StudyReceiptLookup, StoreError> {
        use crate::{Receipt, StudyReceiptLookup};
        let (active_dataset, command, encoded): (String, Option<String>, Option<String>) = self
            .connection
            .query_row(
                "SELECT s.dataset_id, r.command, r.result
                 FROM store_state s
                 LEFT JOIN receipts r ON r.id=?2 AND s.dataset_id=?1
                 WHERE s.singleton=1",
                rusqlite::params![dataset.0, id.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(database_error)?;
        if active_dataset != dataset.0 {
            return Ok(StudyReceiptLookup::WrongDataset);
        }
        match (command, encoded) {
            (Some(command), Some(encoded)) => Ok(StudyReceiptLookup::Found(Box::new(Receipt {
                id: id.clone(),
                command,
                result: decode_receipt(&encoded)?,
            }))),
            (None, None) => Ok(StudyReceiptLookup::Missing),
            _ => Err(invalid("incomplete Study receipt row")),
        }
    }
}
