//! Store and its usage scenarios.

use crate::{Commit, Receipt, Snapshot, StoreError, StoredAttempt, store_error::invalid};
use tempo_chess::ContentItem;
use tempo_learning::LearningState;
use tempo_types::{
    AttemptId, ContentVersion, DatasetId, Revision, StudyDay, SubmissionId, TargetId,
};

/// Current study reads exclude unrelated attempts and their outcomes.
#[derive(Clone, Copy, Default)]
pub struct StudyScope<'a> {
    pub catalog: bool,
    pub attempt: Option<&'a AttemptId>,
    pub target: Option<&'a TargetId>,
    pub submission: Option<&'a SubmissionId>,
}

pub struct ContentReceipt<'a> {
    pub request: &'a str,
    pub reply: &'a str,
}

pub enum ContentSource<'a> {
    /// Historical provenance for an accepted catalog operation.
    Archive { id: &'a str, pgn: &'a str },
    /// The accepted full authored document of one addressed repertoire.
    RepertoireDocument {
        id: &'a str,
        pgn: &'a str,
        repertoire_id: &'a str,
        expected_content_revision: Revision,
        model_input: &'a crate::ModelInputStamp,
    },
}

/// A bounded, consistent read of the active dataset and selected repertoire.
/// An existing repertoire can lack an authoritative PGN association.
pub struct RepertoireDocument {
    pub dataset: DatasetId,
    pub revision: Revision,
    pub item: Option<ContentItem>,
    pub pgn: Option<String>,
    pub source_id: Option<String>,
    pub model_authority: crate::ModelAuthority,
}

/// The same command may be delivered again without changing newer learning.
pub enum RepertoireReset {
    Applied(Vec<TargetId>),
    Replayed,
    StaleDataset,
    UnknownRepertoire,
    CommandConflict,
}

/// The consistent-read and atomic-write persistence boundary.
///
/// # When to use
///
/// Inject an implementation into the application composition layer. The application
/// reads a snapshot, asks domain components for pure results, and commits the typed
/// batch. Study, Chess, Queue and Learning do not import or receive this trait.
///
/// [`crate::MemoryStore`] implements the contract in process memory.
/// [`crate::SqliteStore`] uses the same validation and publishes each batch
/// inside one database transaction.
pub trait Store {
    /// Read a consistent owned view without changing stored state.
    fn snapshot(&self) -> Result<Snapshot, StoreError>;
    /// Read current study state with only the referenced outcome/receipt.
    fn study_state(&self, _scope: StudyScope<'_>) -> Result<Snapshot, StoreError> {
        self.snapshot()
    }

    /// Publish a study batch checked against the same current-state scope.
    fn commit_study(
        &mut self,
        commit: Commit,
        _scope: StudyScope<'_>,
    ) -> Result<Revision, StoreError> {
        self.commit(commit)
    }

    /// Read a page of current tactics and basic learning facts in target order.
    fn tactic_page(
        &self,
        _query: crate::TacticPageQuery<'_>,
    ) -> Result<crate::TacticPage, StoreError> {
        Err(invalid(
            "this Store does not support bounded tactic browsing",
        ))
    }

    fn tactics_extra(&self, _dataset: &DatasetId, _day: StudyDay) -> Result<usize, StoreError> {
        Ok(0)
    }

    /// Persist a day-scoped tactics allowance and receipt in one transaction.
    fn extend_tactics_allowance(
        &mut self,
        _dataset: &DatasetId,
        _revision: Revision,
        _day: StudyDay,
        _increase: usize,
        _receipt: &Receipt,
    ) -> Result<(), StoreError> {
        Err(invalid(
            "this Store does not support daily tactics allowances",
        ))
    }

    fn content_state(&self) -> Result<(Snapshot, Revision), StoreError> {
        let state = self.snapshot()?;
        let revision = state.revision;
        Ok((state, revision))
    }

    fn repertoire_document(&self, _id: &str) -> Result<RepertoireDocument, StoreError> {
        Err(invalid("this Store does not support repertoire documents"))
    }

    /// Clear one repertoire's Study records and publish its reset receipt atomically.
    fn reset_repertoire(
        &mut self,
        _dataset: &DatasetId,
        _repertoire: &str,
        _command_id: &str,
        _at_ms: u64,
        _fresh: &LearningState,
    ) -> Result<RepertoireReset, StoreError> {
        Err(invalid(
            "this Store does not support repertoire progress resets",
        ))
    }

    /// Latest reset whose evidence boundary applies to this repertoire.
    fn repertoire_reset_at(
        &self,
        _dataset: &DatasetId,
        _repertoire: &str,
    ) -> Result<Option<u64>, StoreError> {
        Ok(None)
    }

    fn content_receipt(&self, _request: &str) -> Result<Option<String>, StoreError> {
        Ok(None)
    }

    /// Pin sources, any current repertoire association and the reply atomically.
    fn commit_content(
        &mut self,
        commit: Commit,
        source: Option<ContentSource<'_>>,
        _receipt: Option<ContentReceipt<'_>>,
    ) -> Result<Revision, StoreError> {
        if matches!(source, Some(ContentSource::RepertoireDocument { .. })) {
            return Err(invalid("this Store does not support repertoire documents"));
        }
        self.commit(commit)
    }

    fn stored_attempt(&self, id: &AttemptId) -> Result<Option<StoredAttempt>, StoreError> {
        Ok(self.snapshot()?.attempts.remove(id))
    }

    fn issued_item(
        &self,
        dataset: &DatasetId,
        target: &TargetId,
        version: ContentVersion,
    ) -> Result<Option<ContentItem>, StoreError> {
        Ok(self
            .snapshot()?
            .content_history
            .into_iter()
            .find(|entry| {
                &entry.dataset == dataset
                    && &entry.item.target == target
                    && entry.item.version == version
            })
            .map(|entry| entry.item))
    }

    /// Counts accepted reset commands independent of clock precision or ordering.
    fn repertoire_reset_generation(
        &self,
        _dataset: &DatasetId,
        _repertoire: &str,
    ) -> Result<u64, StoreError> {
        Ok(0)
    }

    /// Save a game batch at the revision whose repertoire was compared.
    fn commit_games(
        &mut self,
        _commit: Commit,
        _games: &[crate::ComparedGame],
        _successful_refresh_ms: Option<u64>,
        _receipt: crate::GameReceipt<'_>,
    ) -> Result<crate::AcceptedGames, StoreError> {
        Err(invalid("this Store does not support game evidence"))
    }

    fn game_receipt(
        &self,
        _dataset: &DatasetId,
        _id: &str,
        _fingerprint: &str,
    ) -> Result<Option<crate::AcceptedGames>, StoreError> {
        Ok(None)
    }

    /// Read chronology for a local PGN with a precise start, by raw game ID.
    fn pgn_timing(
        &self,
        _dataset: &DatasetId,
        _id: &str,
    ) -> Result<Option<crate::PgnTiming>, StoreError> {
        Ok(None)
    }

    /// Resolve a previously verified provider game by canonical URL and played facts.
    fn verified_game(
        &self,
        _dataset: &DatasetId,
        _url: &str,
        _signature: &str,
    ) -> Result<Option<crate::GameRecord>, StoreError> {
        Ok(None)
    }

    fn legacy_game_misses(
        &self,
        _dataset: &DatasetId,
        _target: &TargetId,
        _version: ContentVersion,
    ) -> Result<Vec<crate::LegacyGameMiss>, StoreError> {
        Ok(vec![])
    }

    fn game_comparison(
        &self,
        _dataset: &DatasetId,
        _id: &str,
    ) -> Result<Option<crate::GameSummary>, StoreError> {
        Ok(None)
    }

    /// Read all closed positions and their accepted attribution, if it is known.
    fn game_choices(
        &self,
        _dataset: &DatasetId,
        _source: &str,
        _game_id: &str,
    ) -> Result<crate::GameChoices, StoreError> {
        Ok(Default::default())
    }

    /// Page through existing relevance inputs for games with accepted choices.
    fn choice_encounter_keys(
        &self,
        _dataset: &DatasetId,
        _after: Option<&crate::EncounterKey>,
    ) -> Result<Vec<crate::EncounterKey>, StoreError> {
        Ok(Vec::new())
    }

    /// Replace only encounter maps, under their content and exact choice fences.
    /// Return how many maps actually changed. No game acceptance records are written.
    fn repair_choice_encounters(
        &mut self,
        _dataset: &DatasetId,
        _content_revision: Revision,
        _repairs: &[crate::EncounterRepair],
    ) -> Result<usize, StoreError> {
        Err(invalid(
            "this Store does not support encounter reconciliation",
        ))
    }

    fn game_failed(
        &self,
        _dataset: &DatasetId,
        _key: &crate::GameFailureKey,
    ) -> Result<bool, StoreError> {
        Ok(false)
    }

    fn game_failures(
        &self,
        _dataset: &DatasetId,
    ) -> Result<Vec<crate::GameFailureKey>, StoreError> {
        Ok(vec![])
    }

    /// Read catalog, Learning and game acceptance facts at one revision.
    fn portable_state(&self) -> Result<(Snapshot, crate::PortableGameEvidence), StoreError> {
        let snapshot = self.snapshot()?;
        let evidence = crate::PortableGameEvidence {
            game_failures: self.game_failures(&snapshot.dataset)?,
            ..crate::PortableGameEvidence::default()
        };
        Ok((snapshot, evidence))
    }

    /// Recover the source of the issued miss when the dataset retains enough
    /// acceptance and observation evidence. This is a presentation read.
    fn correction_game(
        &self,
        _lookup: crate::CorrectionGameLookup<'_>,
    ) -> Result<Option<crate::CorrectionGame>, StoreError> {
        Ok(None)
    }

    fn stored_game(
        &self,
        _dataset: &DatasetId,
        _source: &str,
        _id: &str,
    ) -> Result<Option<crate::GameRecord>, StoreError> {
        Err(invalid("this Store does not support game evidence"))
    }

    fn games_in_window(
        &self,
        _dataset: &DatasetId,
        _from: u64,
        _to: u64,
    ) -> Result<Vec<crate::GameRecord>, StoreError> {
        Err(invalid("this Store does not support game evidence"))
    }

    fn historical_pgn_window(
        &self,
        _dataset: &DatasetId,
        _learner: &str,
        _from: u64,
        _to: u64,
    ) -> Result<Vec<crate::GameRecord>, StoreError> {
        Err(invalid(
            "this Store does not support historical PGN evidence",
        ))
    }

    /// Restore a new dataset and its acceptance boundaries in one transaction.
    fn restore_with_game_evidence(
        &mut self,
        commit: Commit,
        evidence: &crate::PortableGameEvidence,
        _choice_format: crate::PortableChoiceFormat,
    ) -> Result<Revision, StoreError> {
        if !evidence.is_empty() {
            return Err(invalid(
                "this Store does not support portable game evidence",
            ));
        }
        self.commit(commit)
    }

    fn game_relevance(
        &self,
        _dataset: &DatasetId,
        _model: &str,
    ) -> Result<std::collections::BTreeMap<TargetId, crate::GameTargetRelevance>, StoreError> {
        Ok(Default::default())
    }

    fn game_content_revision(&self, _dataset: &DatasetId) -> Result<Option<Revision>, StoreError> {
        Ok(None)
    }

    fn opponent_gaps(
        &self,
        _dataset: &DatasetId,
    ) -> Result<Vec<crate::GroupedOpponentGap>, StoreError> {
        Ok(Vec::new())
    }

    fn game_backfill_state(
        &self,
        _dataset: &DatasetId,
        _id: Option<&str>,
    ) -> Result<Option<String>, StoreError> {
        Err(invalid("this Store does not support historical backfill"))
    }

    /// Persist the job and add any explicitly recognized identities atomically.
    /// Progress-only writes supply no accounts; existing recognition is retained.
    fn save_game_backfill_state(
        &mut self,
        _dataset: &DatasetId,
        _id: &str,
        _state: &str,
        _accounts_to_recognize: Option<&[(String, String)]>,
    ) -> Result<(), StoreError> {
        Err(invalid("this Store does not support historical backfill"))
    }

    fn game_move_distribution(
        &self,
        _model: &str,
        _position: &str,
    ) -> Result<Option<std::collections::BTreeMap<String, f64>>, StoreError> {
        Ok(None)
    }

    fn save_game_move_distribution(
        &mut self,
        _model: &str,
        _position: &str,
        _distribution: &std::collections::BTreeMap<String, f64>,
    ) -> Result<(), StoreError> {
        Err(invalid("this Store does not support model distributions"))
    }

    fn replace_game_model_reach(
        &mut self,
        _dataset: &DatasetId,
        _projection: crate::BookProjection<'_>,
    ) -> Result<(), StoreError> {
        Err(invalid("this Store does not support modeled reach"))
    }

    fn game_model_projection_status(
        &self,
        _dataset: &DatasetId,
    ) -> Result<Option<crate::ProjectionStatus>, StoreError> {
        Ok(None)
    }

    fn model_bootstrap_repertoires(&self, _dataset: &DatasetId) -> Result<Vec<String>, StoreError> {
        Ok(Vec::new())
    }

    /// Accept one legacy source verification only while the original source is current.
    fn finish_model_bootstrap(
        &mut self,
        _dataset: &DatasetId,
        _content_revision: Revision,
        _repertoire: &str,
        _source_id: &str,
        _stamp: Option<&crate::ModelInputStamp>,
    ) -> Result<(), StoreError> {
        Err(invalid(
            "this Store does not support model source verification",
        ))
    }

    /// The recent answer-safe comparison records for the active dataset.
    fn game_history(&self) -> Result<crate::GameHistory, StoreError> {
        Err(invalid("this Store does not support game evidence"))
    }

    /// Publish the whole batch and return its new store revision, or publish nothing.
    fn commit(&mut self, commit: Commit) -> Result<Revision, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{issued_record, policy};
    use crate::{MemoryStore, Write};
    use tempo_chess::demo_catalog;
    use tempo_learning::LearningPolicy;

    #[test]
    fn application_code_uses_the_port_without_knowing_the_memory_adapter() {
        let mut memory =
            MemoryStore::new("dataset-a".into(), demo_catalog(), policy().settings()).unwrap();
        let store: &mut dyn Store = &mut memory;
        let snapshot = store.snapshot().unwrap();
        let issued = issued_record(&snapshot);
        let expected_id = issued.attempt.id.clone();
        store
            .commit(Commit {
                expected_revision: snapshot.revision,
                expected_dataset: snapshot.dataset,
                writes: vec![
                    Write::Issue(issued),
                    Write::Active(Some(expected_id.clone())),
                ],
            })
            .unwrap();
        let after = store.snapshot().unwrap();
        assert_eq!(after.active_attempt, Some(expected_id.clone()));
        assert!(after.attempts.contains_key(&expected_id));
    }
}
