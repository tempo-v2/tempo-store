//! SQLite test probes. Authorizer observations measure statement preparation.
//! The row counters separately measure actual writes, including no-op rewrites.

use super::SqliteStore;
use rusqlite::{
    Connection,
    hooks::{AuthAction, AuthContext, Authorization},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

#[derive(Debug, Default)]
pub(super) struct SqlAccess {
    pub allowed: BTreeSet<String>,
    pub denied: BTreeSet<String>,
}

/// Run the real adapter operation under a table/column access policy.
/// Use a disposable Store and install any fixture rows before entering the probe.
pub(super) fn probe_access<T>(
    store: &mut SqliteStore,
    deny: impl for<'a> Fn(AuthAction<'a>) -> bool + Send + 'static,
    operation: impl FnOnce(&mut SqliteStore) -> T,
) -> (T, SqlAccess) {
    use AuthAction::{Delete, Insert, Read, Update};
    let observed = Arc::new(Mutex::new(SqlAccess::default()));
    let recorded = Arc::clone(&observed);
    // Cached statements prepared under a different policy cannot provide
    // evidence about what SQLite authorized for this operation.
    store.connection.flush_prepared_statement_cache();
    store
        .connection
        .authorizer(Some(move |context: AuthContext<'_>| {
            if matches!(
                context.action,
                Read { .. } | Insert { .. } | Update { .. } | Delete { .. }
            ) {
                let denied = deny(context.action);
                let mut observed = recorded.lock().unwrap();
                if denied {
                    observed.denied.insert(format!("{context:?}"));
                    return Authorization::Deny;
                }
                observed.allowed.insert(format!("{context:?}"));
            }
            Authorization::Allow
        }));
    let result = operation(store);
    store
        .connection
        .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    store.connection.flush_prepared_statement_cache();
    (
        result,
        Arc::try_unwrap(observed).unwrap().into_inner().unwrap(),
    )
}

pub(super) fn record_row_writes(connection: &Connection, tables: &[&str]) {
    connection
        .execute_batch("CREATE TEMP TABLE operation_write_probe(name TEXT NOT NULL);")
        .unwrap();
    for table in tables {
        assert!(
            table
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        );
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            connection
                .execute_batch(&format!(
                    "CREATE TEMP TRIGGER track_{table}_{operation} AFTER {operation} ON main.{table}
                     BEGIN INSERT INTO operation_write_probe VALUES('{table}'); END;"
                ))
                .unwrap();
        }
    }
}

pub(super) fn row_write_counts(connection: &Connection) -> BTreeMap<String, u64> {
    let mut query = connection
        .prepare("SELECT name, count(*) FROM operation_write_probe GROUP BY name")
        .unwrap();
    query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
