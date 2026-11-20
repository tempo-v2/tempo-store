//! Durable tactical observations, independent of puzzle identity and report revision.
use crate::GameRecord;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tempo_types::{ContentVersion, Revision, TargetId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TacticalAnalysisStatus {
    Complete,
    Inconclusive,
    Failed,
    TimedOut,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TacticalPuzzleRef {
    pub target: TargetId,
    pub version: ContentVersion,
}

/// Identity of the Study request delivered by the application.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TacticalRequestScope {
    #[default]
    Finding,
    Decision,
}

impl TacticalRequestScope {
    fn is_finding(&self) -> bool {
        *self == Self::Finding
    }
}

/// `outcome` and `details_json` are the report's authoritative result, retained
/// for reporting. Store never interprets them to choose a learning action.
/// `train` is the application's explicit instruction to request blind recall.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TacticalObservation {
    pub producer: Option<tempo_chess::DetectorIdentity>,
    /// Zero-based pre-move timeline occurrence. A custom-FEN first move is 0.
    pub ply: u32,
    pub motif: String,
    pub outcome: String,
    pub details_json: String,
    pub train: Option<TacticalPuzzleRef>,
    #[serde(default, skip_serializing_if = "TacticalRequestScope::is_finding")]
    pub request_scope: TacticalRequestScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TacticalAnalysis {
    pub position: Option<TacticalPositionRun>,
    pub source: String,
    pub game_id: String,
    pub learner: String,
    pub report: String,
    pub analysis_version: String,
    /// The accepted game's stored mainline signature, unchanged by the caller.
    pub game_fingerprint: String,
    pub analyzed_at_ms: u64,
    pub decisions_scanned: u32,
    pub status: TacticalAnalysisStatus,
    pub error: Option<String>,
    pub observations: Vec<TacticalObservation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TacticalAcceptance {
    pub revision: Revision,
    pub replayed: bool,
    pub new_observations: usize,
    pub new_requests: usize,
    /// Targets of requests inserted by this acceptance. On replay this is empty:
    /// replay must not undo a later explicit Skip.
    #[serde(default)]
    pub new_request_targets: Vec<TargetId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TacticalGameCursor {
    pub ordinal: i64,
    pub source: String,
    pub game_id: String,
    pub learner: String,
}

pub struct TacticalGame {
    pub game: GameRecord,
    pub learner: String,
    pub cursor: TacticalGameCursor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TacticalRequest {
    pub id: u64,
    pub target: TargetId,
    pub version: ContentVersion,
    pub accepted_at_ms: u64,
    pub observation_id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredTacticalObservation {
    pub id: u64,
    pub source: String,
    pub game_id: String,
    pub learner: String,
    pub report: String,
    pub analysis_version: String,
    pub observed_at_ms: u64,
    /// In this read model, `train` identifies the noncancelled request's retained
    /// content. An issued or fulfilled request can precede a revised assessment.
    #[serde(flatten)]
    pub observation: TacticalObservation,
    pub request_id: Option<u64>,
    pub fulfilled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TacticalObservationPage {
    pub observations: Vec<StoredTacticalObservation>,
    pub next_before: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TacticalStatistics {
    pub completed_games: u64,
    pub failed_games: u64,
    pub timed_out_games: u64,
    pub unsupported_games: u64,
    pub pending_games: u64,
    pub pending_positions: u64,
    pub inconclusive_positions: u64,
    pub retryable_positions: u64,
    pub decisions_scanned: u64,
    pub observations: u64,
    pub outcomes: BTreeMap<String, u64>,
    pub pending_requests: u64,
}

/// An immutable attempt at a queued position. A completed retry has its own receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TacticalPositionRun {
    pub id: u64,
    pub ply: u32,
    pub deeper: bool,
}

pub struct TacticalPositionTask {
    pub analyses: Vec<tempo_chess::DetectorIdentity>,
    pub run: TacticalPositionRun,
    pub game: TacticalGame,
}

#[derive(Serialize)]
pub struct AnalysisGameProgress {
    pub source: String,
    pub game_id: String,
    pub white: String,
    pub black: String,
    pub checked: u64,
    pub pending: u64,
    pub inconclusive: u64,
    pub preparing: bool,
}

/// Source facts for a verified game. Tempo projects time and learner-relative result.
pub struct TacticalArchiveGame {
    pub source: String,
    pub id: String,
    pub learner: String,
    pub white: String,
    pub pgn: String,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub fully_analyzed: bool,
}

/// Small report input. Full engine evidence is loaded only for displayed positions.
pub struct TacticalDecisionRow {
    pub id: u64,
    pub learner: String,
    pub source: String,
    pub game_id: String,
    pub ply: u32,
    pub position: String,
    pub outcome: tempo_chess::TacticalOutcome,
    pub candidates: Vec<(tempo_chess::Motif, String)>,
    pub accepted: Vec<String>,
}

/// The currently reportable Study link for a turn, independent of its latest evaluation.
#[derive(Debug, Serialize)]
pub struct TacticalTrainingLink {
    pub id: u64,
    pub status: TacticalTrainingStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TacticalTrainingStatus {
    Queued,
    Issued,
    Superseded,
    Reviewed,
}

pub struct TacticalDecisionDetails {
    pub evidence_json: String,
    pub training: Option<TacticalTrainingLink>,
}
