//! Store records for game ingestion. Study never reads raw games to select work.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tempo_chess::{OpponentGap, RepertoireAdherence};
use tempo_types::{ContentVersion, DatasetId, MissRevision, Revision, TargetId};

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
    pub played_date_ms: Option<u64>,
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
        let start_ms = self
            .start_ms
            .filter(|_| self.source == "pgn" && !self.mainline_signature.is_empty())?;
        Some(PgnTiming {
            mainline_signature: Some(self.mainline_signature.clone()),
            start_ms,
            end_ms: self.end_ms,
        })
    }
}

/// The issued identity and reset boundary of a guided repertoire repair.
#[derive(Clone, Copy)]
pub struct CorrectionGameLookup<'a> {
    pub dataset: &'a DatasetId,
    pub target: &'a TargetId,
    pub version: ContentVersion,
    pub miss_revision: MissRevision,
    pub issued_at_ms: u64,
    pub reset_at_ms: Option<u64>,
    pub prescribed: &'a str,
}

/// Retained game and exact observed ply for the Game Again that issued a repair.
pub struct CorrectionGame {
    pub game: GameRecord,
    pub ply: usize,
    pub played: String,
}

#[derive(Clone, Debug)]
pub struct ComparedGame {
    pub game: GameRecord,
    pub id: String,
    pub summary: GameSummary,
    pub observations: Vec<GameObservation>,
    pub failures: Vec<crate::AssessmentRecord>,
    pub resolved_choice: Option<AcceptedGameChoice>,
    pub relevance: Option<ComparedRelevance>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction_effect: Option<tempo_learning::GameReviewEffect>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<GameChoice>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disagreements: Vec<RepertoireDisagreement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repertoires: Vec<RepertoireAdherence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backfill_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GameChoice {
    pub ply: usize,
    pub position: String,
    pub candidates: Vec<GameChoiceTarget>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GameChoiceTarget {
    pub repertoire_id: String,
    pub target_id: TargetId,
    pub version: ContentVersion,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepertoireDisagreement {
    pub repertoire_id: String,
    pub plies: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameFailureKey {
    pub source: String,
    pub game_id: String,
    pub target: TargetId,
    pub version: ContentVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_position: Option<String>,
}

impl GameFailureKey {
    /// Stable identity shared by the game comparison producer and Store.
    pub fn assessment_key(&self) -> crate::AssessmentKey {
        let identity = serde_json::json!([self.game_id, self.target]);
        crate::AssessmentKey::External {
            provider: format!("game:{}", self.source),
            event_id: tempo_types::EventId(
                blake3::hash(identity.to_string().as_bytes())
                    .to_hex()
                    .to_string(),
            ),
        }
    }
}

/// Games acceptance facts independent of raw games, comparisons and receipts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortableGameEvidence {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub game_failures: Vec<GameFailureKey>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub repertoire_reset_boundaries: std::collections::BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved_game_choices: Vec<ResolvedGameChoice>,
}

impl PortableGameEvidence {
    pub(crate) fn is_empty(&self) -> bool {
        self.game_failures.is_empty()
            && self.repertoire_reset_boundaries.is_empty()
            && self.resolved_game_choices.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedGameChoice {
    pub source: String,
    pub game_id: String,
    pub position: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<ChoiceAttribution>,
}

/// The exact answer accepted by an explicit choice, independent of any grade.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChoiceAttribution {
    pub target: TargetId,
    pub answer_version: ContentVersion,
}

/// A native resolution carries the saved comparison and the required selection.
#[derive(Clone, Debug)]
pub struct AcceptedGameChoice {
    pub comparison_id: String,
    pub ply: usize,
    pub position: String,
    pub attribution: ChoiceAttribution,
}

/// Position keys carry closure even when their historical attribution is unknown.
pub type GameChoices = BTreeMap<String, Option<ChoiceAttribution>>;

/// Earlier exports carried failure positions and optional closure, but no selected answer.
pub enum PortableChoiceFormat {
    Legacy,
    Attributed,
}

/// An existing game/learner projection eligible for evidence-only reconciliation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EncounterKey {
    pub source: String,
    pub game_id: String,
    pub learner: String,
}

pub type GameEncounters = BTreeMap<TargetId, (ContentVersion, bool)>;

/// Complete replacement from current Chess facts and the exact accepted choices read.
pub struct EncounterRepair {
    pub key: EncounterKey,
    pub choices: GameChoices,
    pub encounters: GameEncounters,
}

pub struct LegacyGameMiss {
    pub source: String,
    pub game_id: String,
    pub learner: String,
    pub ply: usize,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
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
pub struct GameHistoryEntry {
    pub comparison_id: String,
    pub summary: GameSummary,
    pub white: String,
    pub black: String,
    pub played_date_ms: Option<u64>,
}

/// One newest comparison per retained game and learner, with its original PGN.
pub struct GameLibraryEntry {
    pub ordinal: i64,
    pub game: GameRecord,
    pub learner: String,
    pub repertoires: Vec<RepertoireAdherence>,
}

pub struct GameLibraryPage {
    pub through: i64,
    pub entries: Vec<GameLibraryEntry>,
    pub next_before: Option<i64>,
}

#[derive(Debug)]
pub struct GameHistory {
    pub dataset: DatasetId,
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

/// Authored content and eligible repertoire participation observed together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GameComparisonBasis {
    pub content: Revision,
    pub participation: Revision,
}

/// One analyzed game supplies position encounters and current answer comparisons.
#[derive(Clone, Debug, Default)]
pub struct ComparedRelevance {
    pub encounters: GameEncounters,
    pub gaps: BTreeMap<OpponentGap, String>,
    pub prior_choices: GameChoices,
    pub basis: GameComparisonBasis,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GameTargetRelevance {
    pub exposure: u32,
    pub misses: u32,
    pub window_games: u32,
    pub modeled_millionths: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct GroupedOpponentGap {
    #[serde(flatten)]
    pub gap: OpponentGap,
    pub games: usize,
}
