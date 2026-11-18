//! Store records for game ingestion. Study never reads raw games to select work.

use serde::{Deserialize, Serialize};
use tempo_types::{ContentVersion, TargetId};

#[derive(Clone, Debug)]
pub struct GameRecord {
    pub source: String,
    pub id: String,
    pub url: Option<String>,
    pub white: String,
    pub black: String,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub fetched_ms: u64,
    pub pgn: String,
}

#[derive(Clone, Debug)]
pub struct ComparedGame {
    pub game: GameRecord,
    pub id: String,
    pub summary: GameSummary,
    pub observations: Vec<GameObservation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GameSummary {
    pub source: String,
    pub game_id: String,
    pub url: Option<String>,
    pub repertoire_id: String,
    pub learner: String,
    pub occurrence_end_ms: Option<u64>,
    pub matches: usize,
    pub deviations: usize,
    pub first_deviation_ply: Option<usize>,
    pub coverage_gaps: usize,
    pub opponent_deviations: usize,
}

#[derive(Clone, Debug)]
pub struct GameObservation {
    pub ply: usize,
    pub target: TargetId,
    pub version: ContentVersion,
    pub played: String,
    pub prescribed: String,
}

#[derive(Default, Debug)]
pub struct GameCommit {
    pub new_games: usize,
    pub duplicates: usize,
}

#[derive(Default, Debug, Serialize)]
pub struct GameHistory {
    pub results: Vec<GameSummary>,
    pub last_successful_refresh_ms: Option<u64>,
}
