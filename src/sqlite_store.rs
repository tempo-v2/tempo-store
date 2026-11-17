//! File-backed Store adapter. Keep SQL schema and row mapping with this adapter.

use crate::store_error::{conflict, invalid};
use crate::transaction::{initial_snapshot, stage_commit};
use crate::{Commit, Snapshot, Store, StoreError};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::{path::Path, time::Duration};
use tempo_chess::Catalog;
use tempo_learning::SchedulerSettings;
use tempo_types::{DatasetId, Revision};

mod load_snapshot;
mod row_values;
mod save_snapshot;

use load_snapshot::load_snapshot;
use save_snapshot::save_snapshot;

const SCHEMA_VERSION: i64 = 1;
const SCHEMA: &str = include_str!("sqlite_store/schema.sql");

/// An atomic, file-backed implementation of [`Store`].
///
/// Create a store with an initial dataset, then reopen it with [`Self::open`].
/// Both adapters share the same typed commit validation. SQLite persists each
/// accepted batch and its revision together, and independent connections read
/// from the file instead of keeping a private cached snapshot.
///
/// ```
/// use tempo_chess::Catalog;
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
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(database_error)?;
        save_snapshot(&transaction, None, &initial)?;
        transaction.commit().map_err(database_error)?;
        Ok(Self { connection })
    }

    /// Open an existing version-1 database. A missing file is an error.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = file_path(path.as_ref())?;
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(database_error)?;
        configure(&connection)?;
        let version = schema_version(&connection)?;
        if version != SCHEMA_VERSION {
            return Err(invalid(&format!(
                "unsupported SQLite store version {version}"
            )));
        }
        let store = Self { connection };
        store.snapshot()?;
        Ok(store)
    }
}

impl Store for SqliteStore {
    fn snapshot(&self) -> Result<Snapshot, StoreError> {
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
                .map_err(database_error)?;
        let snapshot = load_snapshot(&transaction)?;
        transaction.commit().map_err(database_error)?;
        Ok(snapshot)
    }

    fn commit(&mut self, commit: Commit) -> Result<Revision, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let previous = load_snapshot(&transaction)?;
        let next = stage_commit(&previous, commit)?;
        save_snapshot(&transaction, Some(&previous), &next)?;
        transaction.commit().map_err(database_error)?;
        Ok(next.revision)
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
