//! File-backed Store adapter. Keep SQL schema and row mapping with this adapter.

use crate::store_error::{conflict, invalid};
use crate::transaction::{initial_snapshot, stage_commit};
use crate::{
    Commit, ContentReceipt, ContentSource, Receipt, ReceiptResult, Snapshot, Store, StoreError,
    StoredAttempt, StudyScope, Write,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use std::{path::Path, time::Duration};
use tempo_chess::{Catalog, ContentItem};
use tempo_learning::SchedulerSettings;
use tempo_types::{AttemptId, ContentVersion, DatasetId, Revision, StudyDay, TargetId};

mod load_snapshot;
mod load_study;
mod row_values;
mod save_snapshot;

use load_snapshot::load_snapshot;
use save_snapshot::save_snapshot;

const SCHEMA_VERSION: i64 = 3;
const SCHEMA: &str = include_str!("sqlite_store/schema.sql");
const DAILY_ALLOWANCE_SCHEMA: &str = include_str!("sqlite_store/daily_allowances.sql");

/// An atomic, file-backed implementation of [`Store`].
///
/// Create a store with an initial dataset, then reopen it with [`Self::open`].
/// Both adapters share the same typed commit validation. SQLite persists each
/// accepted batch and its revision together, and independent connections read
/// from the file instead of keeping a private cached snapshot.
///
/// ```
/// use tempo_chess::{Catalog, ContentItem};
/// use tempo_learning::{LearningPolicy, StandardLearning};
/// use tempo_store::{SqliteStore, Store};
///
/// let dir = tempfile::tempdir()?;
/// let path = dir.path().join("study.sqlite");
/// let policy: StandardLearning = StandardLearning::default();
/// let store = SqliteStore::create(&path, "local".into(), Catalog::default(), policy.settings())?;
/// let before = store.snapshot()?;
/// drop(store);
/// assert_eq!(SqliteStore::open(&path)?.snapshot()?, before);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct SqliteStore {
    connection: Connection,
}

impl SqliteStore {
    /// Initialize an empty database file with the supplied dataset and policy.
    /// A database with a schema or an existing Store version is left intact.
    pub fn create(
        path: impl AsRef<Path>,
        dataset: DatasetId,
        catalog: Catalog,
        policy: SchedulerSettings,
    ) -> Result<Self, StoreError> {
        Self::create_with_profile(path, dataset, catalog, policy, None)
    }

    /// Create a dataset and persist its application profile together.
    pub fn create_with_profile(
        path: impl AsRef<Path>,
        dataset: DatasetId,
        catalog: Catalog,
        policy: SchedulerSettings,
        profile: Option<&str>,
    ) -> Result<Self, StoreError> {
        let path = file_path(path.as_ref())?;
        let initial = initial_snapshot(dataset, catalog, policy)?;
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(database_error)?;
        configure(&connection)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let objects: i64 = transaction
            .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
            .map_err(database_error)?;
        if schema_version(&transaction)? != 0 || objects != 0 {
            return Err(conflict("database already has a schema"));
        }
        transaction.execute_batch(SCHEMA).map_err(database_error)?;
        transaction
            .execute_batch(DAILY_ALLOWANCE_SCHEMA)
            .map_err(database_error)?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(database_error)?;
        save_snapshot(&transaction, None, &initial)?;
        transaction
            .execute(
                "UPDATE store_state SET local_profile=?1 WHERE singleton=1",
                [profile],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)?;
        Ok(Self { connection })
    }

    /// List saved content sources for local provenance reconciliation.
    pub fn content_source_ids(&self) -> Result<std::collections::BTreeSet<String>, StoreError> {
        let mut query = self
            .connection
            .prepare("SELECT id FROM content_sources")
            .map_err(database_error)?;
        let rows = query
            .query_map([], |row| row.get(0))
            .map_err(database_error)?;
        rows.collect::<Result<_, _>>().map_err(database_error)
    }

    /// Read the local application's persisted settings representation.
    pub fn local_profile(&self) -> Result<Option<String>, StoreError> {
        self.connection
            .query_row(
                "SELECT local_profile FROM store_state WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(database_error)
    }

    /// Replace settings if another editor has not changed the profile in the meantime.
    pub fn update_local_profile(&self, expected: &str, profile: &str) -> Result<(), StoreError> {
        let changed = self
            .connection
            .execute(
                "UPDATE store_state SET local_profile=?1 WHERE singleton=1 AND local_profile=?2",
                params![profile, expected],
            )
            .map_err(database_error)?;
        if changed != 1 {
            return Err(conflict("study settings changed; reload before retrying"));
        }
        Ok(())
    }

    /// Open an existing database using the current schema. A missing file is an error.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = file_path(path.as_ref())?;
        let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(database_error)?;
        configure(&connection)?;
        if schema_version(&connection)? == 2 {
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(database_error)?;
            if schema_version(&tx)? == 2 {
                tx.execute_batch(DAILY_ALLOWANCE_SCHEMA)
                    .map_err(database_error)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)
                    .map_err(database_error)?;
            }
            tx.commit().map_err(database_error)?;
        }
        let version = schema_version(&connection)?;
        if version != SCHEMA_VERSION {
            return Err(invalid(&format!(
                "unsupported SQLite store version {version}"
            )));
        }
        let store = Self { connection };
        store.study_state(StudyScope::default())?;
        Ok(store)
    }
}

mod tactic_page;

impl Store for SqliteStore {
    fn tactic_page(
        &self,
        query: crate::TacticPageQuery<'_>,
    ) -> Result<crate::TacticPage, StoreError> {
        self.read_tactic_page(query)
    }

    fn snapshot(&self) -> Result<Snapshot, StoreError> {
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
                .map_err(database_error)?;
        let snapshot = load_snapshot(&transaction)?;
        transaction.commit().map_err(database_error)?;
        Ok(snapshot)
    }

    fn study_state(&self, scope: StudyScope<'_>) -> Result<Snapshot, StoreError> {
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
                .map_err(database_error)?;
        let state = load_study::load_study(&transaction, scope)?;
        transaction.commit().map_err(database_error)?;
        Ok(state)
    }

    fn stored_attempt(&self, id: &AttemptId) -> Result<Option<StoredAttempt>, StoreError> {
        let record: Option<String> = self
            .connection
            .query_row("SELECT record FROM attempts WHERE id=?1", [&id.0], |row| {
                row.get(0)
            })
            .optional()
            .map_err(database_error)?;
        record.as_deref().map(row_values::decode).transpose()
    }

    fn issued_item(
        &self,
        dataset: &DatasetId,
        target: &TargetId,
        version: ContentVersion,
    ) -> Result<Option<ContentItem>, StoreError> {
        let item: Option<String> = self.connection.query_row(
            "SELECT item FROM content_history WHERE dataset_id=?1 AND target_id=?2 AND version=?3",
            params![dataset.0, target.0, version.0.to_be_bytes().as_slice()], |row| row.get(0)
        ).optional().map_err(database_error)?;
        item.as_deref().map(row_values::decode).transpose()
    }

    fn tactics_extra(&self, dataset: &DatasetId, day: StudyDay) -> Result<usize, StoreError> {
        self.connection
            .query_row(
                "SELECT extra FROM daily_tactics_allowances WHERE dataset_id=?1 AND study_day=?2",
                params![dataset.0, day.0],
                |row| row.get(0),
            )
            .optional()
            .map(|extra| extra.unwrap_or(0))
            .map_err(database_error)
    }

    fn extend_tactics_allowance(
        &mut self,
        dataset: &DatasetId,
        revision: Revision,
        day: StudyDay,
        increase: usize,
        receipt: &Receipt,
    ) -> Result<(), StoreError> {
        if increase == 0
            || receipt.id.0.is_empty()
            || !matches!(receipt.result, ReceiptResult::TacticsAllowance { .. })
        {
            return Err(invalid("expected a positive allowance and its receipt"));
        }
        let next = revision
            .0
            .checked_add(1)
            .ok_or_else(|| invalid("store revision exhausted"))?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let changed = tx
            .execute(
                "UPDATE store_state SET revision=?1 WHERE singleton=1 AND dataset_id=?2
             AND revision=?3 AND active_attempt_id IS NULL",
                params![
                    next.to_be_bytes().as_slice(),
                    dataset.0,
                    revision.0.to_be_bytes().as_slice()
                ],
            )
            .map_err(database_error)?;
        if changed != 1 {
            return Err(conflict("study changed before the allowance was applied"));
        }
        tx.execute(
            "INSERT INTO daily_tactics_allowances VALUES (?1, ?2, ?3)
             ON CONFLICT(dataset_id, study_day) DO UPDATE SET extra=extra+excluded.extra",
            params![dataset.0, day.0, increase],
        )
        .map_err(database_error)?;
        tx.execute(
            "INSERT INTO receipts VALUES (?1, NULL, ?2, ?3)",
            params![
                receipt.id.0,
                receipt.command,
                row_values::encode(&receipt.result)?
            ],
        )
        .map_err(database_error)?;
        tx.commit().map_err(database_error)?;
        Ok(())
    }

    fn content_state(&self) -> Result<(Snapshot, Revision), StoreError> {
        let tx = Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
            .map_err(database_error)?;
        let state = load_study::load_study(
            &tx,
            StudyScope {
                catalog: true,
                ..StudyScope::default()
            },
        )?;
        let revision: u64 = tx
            .query_row(
                "SELECT content_revision FROM store_state WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        tx.commit().map_err(database_error)?;
        Ok((state, Revision(revision)))
    }

    fn commit_study(
        &mut self,
        commit: Commit,
        scope: StudyScope<'_>,
    ) -> Result<Revision, StoreError> {
        if commit
            .writes
            .iter()
            .any(|write| matches!(write, Write::Catalog(_) | Write::Restore { .. }))
        {
            return Err(invalid("content writes require a content transaction"));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let previous = load_study::load_study(&transaction, scope)?;
        let next = stage_commit(&previous, commit)?;
        save_snapshot(&transaction, Some(&previous), &next)?;
        transaction.commit().map_err(database_error)?;
        Ok(next.revision)
    }

    fn commit(&mut self, commit: Commit) -> Result<Revision, StoreError> {
        self.commit_content(commit, None, None)
    }

    fn content_receipt(&self, request: &str) -> Result<Option<String>, StoreError> {
        self.connection
            .query_row(
                "SELECT reply FROM content_receipts WHERE request=?1",
                [request],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)
    }

    fn commit_content(
        &mut self,
        commit: Commit,
        source: Option<ContentSource<'_>>,
        receipt: Option<ContentReceipt<'_>>,
    ) -> Result<Revision, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let revision = if commit.writes.is_empty() && receipt.is_some() {
            let current = load_study::load_study(&transaction, StudyScope::default())?;
            if commit.expected_dataset != current.dataset
                || commit.expected_revision != current.revision
            {
                return Err(conflict("snapshot changed; reload before retrying"));
            }
            current.revision
        } else {
            let previous = load_snapshot(&transaction)?;
            let next = stage_commit(&previous, commit)?;
            save_snapshot(&transaction, Some(&previous), &next)?;
            next.revision
        };
        if let Some(source) = source {
            transaction
                .execute(
                    "INSERT INTO content_sources VALUES (?1, ?2) ON CONFLICT DO NOTHING",
                    params![source.id, source.pgn],
                )
                .map_err(database_error)?;
        }
        if let Some(receipt) = receipt {
            transaction
                .execute(
                    "INSERT INTO content_receipts VALUES (?1, ?2)",
                    params![receipt.request, receipt.reply],
                )
                .map_err(database_error)?;
        }
        transaction.commit().map_err(database_error)?;
        Ok(revision)
    }
}

fn file_path(path: &Path) -> Result<&Path, StoreError> {
    if path.as_os_str().is_empty() || path == Path::new(":memory:") {
        return Err(invalid("SQLite store needs a file path"));
    }
    Ok(path)
}

fn configure(connection: &Connection) -> Result<(), StoreError> {
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(database_error)?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")
        .map_err(database_error)
}

fn schema_version(connection: &Connection) -> Result<i64, StoreError> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(database_error)
}

fn database_error(error: rusqlite::Error) -> StoreError {
    StoreError::Unavailable(format!("SQLite store: {error}"))
}

#[cfg(test)]
mod tests;
