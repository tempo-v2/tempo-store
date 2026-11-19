//! Typed persistence values and atomic in-memory and SQLite Store adapters.
//!
//! Read [`Snapshot`] for the consistent view, [`Write`] for one requested change,
//! and [`Commit`] for an atomic batch. The application composes those batches;
//! domain crates never receive a store or database handle.

mod answer_result;
mod assessment_key;
mod assessment_record;
mod chess_attempt;
mod commit;
mod content_history;
mod memory_store;
mod receipt;
mod receipt_result;
mod snapshot;
mod sqlite_store;
mod store;
mod store_error;
mod tactic_page;
pub use tactic_page::{TACTIC_PAGE_SIZE, TacticFilter, TacticListRow, TacticPage, TacticPageQuery};
mod stored_attempt;
mod transaction;
mod write;

#[cfg(test)]
mod test_support;

pub use answer_result::AnswerResult;
pub use assessment_key::AssessmentKey;
pub use assessment_record::AssessmentRecord;
pub use chess_attempt::ChessAttempt;
pub use commit::Commit;
pub use content_history::ContentHistory;
pub use memory_store::MemoryStore;
pub use receipt::Receipt;
pub use receipt_result::ReceiptResult;
pub use snapshot::Snapshot;
pub use sqlite_store::SqliteStore;
pub use store::{
    ContentReceipt, ContentSource, RepertoireDocument, RepertoireReset, Store, StudyScope,
};
pub use store_error::StoreError;
pub use stored_attempt::{AcceptanceKind, ActivityAcceptance, ActivityMode, StoredAttempt};
pub use write::Write;

mod game_evidence;
pub use game_evidence::{
    AcceptedGames, ComparedGame, GameChoice, GameChoiceTarget, GameCommit, GameFailureKey,
    GameHistory, GameHistoryEntry, GameObservation, GameReceipt, GameRecord, GameSummary,
    LegacyGameMiss, PgnTiming, PortableGameEvidence, RepertoireDisagreement, ResolvedGameChoice,
};
