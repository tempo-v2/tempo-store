//! Durable tactical observations, independent of puzzle identity and detector revision.
use crate::GameRecord;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tempo_types::{ContentVersion, Revision, TargetId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TacticalAnalysisStatus {
    Complete,
    Failed,
    TimedOut,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TacticalPuzzleRef {
    pub target: TargetId,
    pub version: ContentVersion,
}

/// `outcome` and `details_json` are the detector's authoritative result, retained
/// for reporting. Store never interprets them to choose a learning action.
/// `train` is the application's explicit instruction to request blind recall.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TacticalObservation {
    /// Zero-based pre-move timeline occurrence. A custom-FEN first move is 0.
    pub ply: u32,
    pub motif: String,
    pub outcome: String,
    pub details_json: String,
    pub train: Option<TacticalPuzzleRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TacticalAnalysis {
    pub source: String,
    pub game_id: String,
    pub learner: String,
    pub detector: String,
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
    pub detector: String,
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
    pub decisions_scanned: u64,
    pub observations: u64,
    pub outcomes: BTreeMap<String, u64>,
    pub pending_requests: u64,
}
