//! Observational storage timings through the standard logging facade.
//! No paths, SQL, parameters, content, receipts or error text leave this module.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static SEQUENCE: AtomicU64 = AtomicU64::new(1);
const TARGET: &str = "tempo_store::diagnostics";

pub(crate) struct Operation {
    name: &'static str,
    id: u64,
    start: Instant,
    enabled: bool,
}

impl Operation {
    pub(crate) fn new(name: &'static str) -> Self {
        let enabled = log::log_enabled!(target: TARGET, log::Level::Debug);
        let id = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        if enabled {
            log::debug!(target: TARGET, operation = name, store_span_id = id; "store.start");
        }
        Self {
            name,
            id,
            start: Instant::now(),
            enabled,
        }
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if self.enabled {
            log::debug!(target: TARGET,
                operation = self.name, store_span_id = self.id,
                elapsed_ms = self.start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                outcome = if std::thread::panicking() { "unwinding" } else { "returned" };
                "store.end"
            );
        }
    }
}

pub(crate) fn sqlite_error(error: &rusqlite::Error) {
    if let rusqlite::Error::SqliteFailure(code, _) = error {
        log::warn!(target: TARGET,
            sqlite_code = code.extended_code & 0xff,
            sqlite_extended_code = code.extended_code;
            "sqlite.error"
        );
    } else {
        log::warn!(target: TARGET, "sqlite.adapter_error");
    }
}

pub(crate) fn timed<T>(operation: &'static str, execute: impl FnOnce() -> T) -> T {
    let _operation = Operation::new(operation);
    execute()
}
