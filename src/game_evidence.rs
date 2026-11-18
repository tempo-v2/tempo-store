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
    pub mainline_signature: String,
}

/// Indexed chronology for one local PGN identity. The normalized start is part
/// of that identity. End precision may improve when the played facts agree.
#[derive(Clone, Debug)]
pub struct PgnTiming {
    pub mainline_signature: Option<String>,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
}

impl PgnTiming {
    pub fn merge_end(&mut self, incoming: &Self) -> Result<(), crate::StoreError> {
        let same_mainline = self.mainline_signature == incoming.mainline_signature;
        let known_conflict = self
            .end_ms
            .zip(incoming.end_ms)
            .is_some_and(|(old, new)| old != new);
        let unverified_change =
            !same_mainline && (self.mainline_signature.is_some() || self.end_ms != incoming.end_ms);
        if self.start_ms != incoming.start_ms || unverified_change || known_conflict {
            return Err(crate::store_error::conflict(
                "a source game identity was delivered with different game facts",
            ));
        }
        self.end_ms = self.end_ms.or(incoming.end_ms);
        Ok(())
    }
}

impl GameRecord {
    pub fn precise_pgn_timing(&self) -> Option<PgnTiming> {
        let start_ms = self.start_ms.filter(|_| self.source == "pgn")?;
        Some(PgnTiming {
            mainline_signature: Some(self.mainline_signature.clone()),
            start_ms,
            end_ms: self.end_ms,
        })
    }
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
    #[serde(default)]
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

#[derive(Default, Debug, Serialize, Deserialize)]
pub struct GameCommit {
    pub new_games: usize,
    pub duplicates: usize,
}

#[derive(Debug)]
pub struct GameDeviation {
    pub target: TargetId,
    pub version: ContentVersion,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
}

#[derive(Debug)]
pub struct GameHistoryEntry {
    pub comparison_id: String,
    pub summary: GameSummary,
    pub deviations: Vec<GameDeviation>,
}

#[derive(Default, Debug)]
pub struct GameHistory {
    pub results: Vec<GameHistoryEntry>,
    pub last_successful_refresh_ms: Option<u64>,
}

/// The exact comparison delivery under a caller-owned immutable command ID.
pub struct GameReceipt<'a> {
    pub id: &'a str,
    pub fingerprint: &'a str,
    pub report: &'a str,
}

#[derive(Serialize, Deserialize)]
pub struct AcceptedGames {
    pub counts: GameCommit,
    pub report: String,
}
