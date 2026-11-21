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
mod repertoire_run;
mod saved_review;
pub use saved_review::{
    ReviewCommandKey, ReviewPosition, ReviewWrite, SavedReview, SavedReviewContext,
    TacticExclusionRead,
};
mod repertoire_activity;
mod snapshot;
mod sqlite_store;
mod store;
mod store_error;
mod tactic_page;
pub use tactic_page::{TACTIC_PAGE_SIZE, TacticFilter, TacticListRow, TacticPage, TacticPageQuery};
mod diagnostics;
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
pub use repertoire_activity::{
    RepertoireActivity, RepertoireActivityCommand, RepertoireActivityRead, RepertoireActivityResult,
};
pub use repertoire_run::{RepertoireRun, RepertoireRunStep};
pub use snapshot::Snapshot;
pub use sqlite_store::{SqliteStore, TacticPayoffPublication};
pub use store::{
    ContentReceipt, ContentSource, RepertoireDocument, RepertoireReset, Store, StudyOrderingWrite,
    StudyScope,
};
pub use store_error::StoreError;
pub use stored_attempt::{
    AcceptanceKind, ActivityAcceptance, ActivityMode, IssuedTacticAttribution, RevealedDecision,
    StoredAttempt,
};
pub use write::Write;

mod game_model;
pub use game_model::{
    BookProjection, ModelAuthority, ModelInputStamp, ModelTargets, ProjectionBook, ProjectionScope,
    ProjectionStatus, REACH_MODEL_KEY, model_lines, model_targets,
};
mod game_evidence;
pub use game_evidence::{
    AcceptedGameChoice, AcceptedGames, ChoiceAttribution, ComparedGame, ComparedRelevance,
    CorrectionGame, CorrectionGameLookup, EncounterKey, EncounterRepair, GameChoice,
    GameChoiceTarget, GameChoices, GameCommit, GameComparisonBasis, GameEncounters, GameFailureKey,
    GameHistory, GameHistoryEntry, GameLibraryEntry, GameLibraryPage, GameObservation, GameReceipt,
    GameRecord, GameSummary, GameTargetRelevance, GroupedOpponentGap, LegacyGameMiss, PgnTiming,
    PortableChoiceFormat, PortableGameEvidence, RepertoireDisagreement, ResolvedGameChoice,
};

mod content_state;
mod study_state;
pub use content_state::ContentState;
pub use study_state::{StudyState, TargetCoverage};

mod tactical_analysis;
pub use tactical_analysis::{
    AnalysisGameProgress, StoredTacticalObservation, TacticGameLookup, TacticGameOrigin,
    TacticGameRelation, TacticalAcceptance, TacticalAcceptanceBasis, TacticalAnalysis,
    TacticalAnalysisStatus, TacticalArchiveCoverage, TacticalArchiveMetadata, TacticalCommit,
    TacticalDecisionDetails, TacticalDecisionRow, TacticalGame, TacticalGameCursor,
    TacticalObservation, TacticalObservationPage, TacticalPositionRun, TacticalPositionTask,
    TacticalPuzzleRef, TacticalReportGeneration, TacticalRequest, TacticalRequestAddress,
    TacticalRequestScope, TacticalStatistics, TacticalTargetChange, TacticalTrainingLink,
    TacticalTrainingStatus,
};

mod coverage;
pub use coverage::{CoverageSummary, ForecastGap, SavedCoverage};
