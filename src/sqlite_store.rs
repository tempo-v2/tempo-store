//! File-backed Store adapter. Keep SQL schema and row mapping with this adapter.

use crate::store_error::{conflict, invalid};
use crate::transaction::{initial_snapshot, stage_commit};
use crate::{
    Commit, ContentReceipt, ContentSource, ContentState, Receipt, ReceiptResult,
    RepertoireDocument, RepertoireReset, Snapshot, Store, StoreError, StoredAttempt, StudyScope,
    StudyState, Write,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use std::{collections::BTreeMap, path::Path, time::Duration};
use tempo_chess::{Catalog, ContentItem, LeadInEvidence};
use tempo_learning::{LearningState, SchedulerSettings};
use tempo_types::{AttemptId, ContentVersion, DatasetId, Grade, Revision, StudyDay, TargetId};

mod game_choices;
mod game_evidence;
mod game_model;
mod game_relevance;
mod load_snapshot;
mod load_study;
mod repertoire_resets;
mod repertoire_runs;
mod repertoire_suspensions;
mod row_values;
mod save_snapshot;
mod saved_reviews;
mod tactical_analysis;
mod tactical_targets;

use load_snapshot::load_snapshot;
use save_snapshot::save_snapshot;

pub use game_choices::ChoiceMigrationReport;

const SCHEMA_VERSION: i64 = 16;
const REPERTOIRE_RUNS_SCHEMA: &str = include_str!("sqlite_store/repertoire_runs.sql");
const TACTICAL_IDENTITY_SCHEMA: &str = include_str!("sqlite_store/tactical_identity.sql");
const TACTICAL_REPORT_GENERATION_SCHEMA: &str =
    include_str!("sqlite_store/tactical_report_generation.sql");
const REPERTOIRE_SUSPENSIONS_SCHEMA: &str = include_str!("sqlite_store/repertoire_suspensions.sql");
const SAVED_REVIEWS_SCHEMA: &str = include_str!("sqlite_store/saved_reviews.sql");
const TACTICAL_ANALYSIS_SCHEMA: &str = include_str!("sqlite_store/tactical_analysis.sql");
const CORRECTIVE_RELEASE_SCHEMA: &str = include_str!("sqlite_store/corrective_release.sql");
const GAME_RELEVANCE_SCHEMA: &str = include_str!("sqlite_store/game_relevance.sql");
const PORTABLE_EVIDENCE_SCHEMA: &str = include_str!("sqlite_store/portable_evidence.sql");
const GAME_CORRECTIONS_SCHEMA: &str = include_str!("sqlite_store/game_corrections.sql");
const REPERTOIRE_RESET_SCHEMA: &str = include_str!("sqlite_store/repertoire_resets.sql");
const GAME_REVIEW_BACKFILL: &str = include_str!("sqlite_store/game_review_backfill.sql");
const GAME_EVIDENCE_SCHEMA: &str = include_str!("sqlite_store/game_evidence.sql");
const SCHEMA: &str = include_str!("sqlite_store/schema.sql");
const DAILY_ALLOWANCE_SCHEMA: &str = include_str!("sqlite_store/daily_allowances.sql");
const REPERTOIRE_DOCUMENT_SCHEMA: &str = include_str!("sqlite_store/repertoire_documents.sql");

/// An atomic, file-backed implementation of [`Store`].
///
/// Create a store with an initial dataset, then reopen it with [`Self::open`].
/// Both adapters share the same typed commit validation. SQLite persists each
/// accepted batch and its revision together, and independent connections read
/// from the file instead of keeping a private cached snapshot.
///
/// ```
/// use tempo_chess::{Catalog, ContentItem};
/// use tempo_learning::{LearningPolicy, StandardLearning};
/// use tempo_store::{SqliteStore, Store};
///
/// let dir = tempfile::tempdir()?;
/// let path = dir.path().join("study.sqlite");
/// let policy: StandardLearning = StandardLearning::default();
/// let store = SqliteStore::create(&path, "local".into(), Catalog::default(), policy.settings())?;
/// let before = store.snapshot()?;
/// drop(store);
/// assert_eq!(SqliteStore::open(&path)?.snapshot()?, before);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct SqliteStore {
    connection: Connection,
    choice_migration: Option<ChoiceMigrationReport>,
}

impl SqliteStore {
    /// Initialize an empty database file with the supplied dataset and policy.
    /// A database with a schema or an existing Store version is left intact.
    pub fn create(
        path: impl AsRef<Path>,
        dataset: DatasetId,
        catalog: Catalog,
        policy: SchedulerSettings,
    ) -> Result<Self, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.create");
        Self::create_with_profile(path, dataset, catalog, policy, None)
    }

    /// Create a dataset and persist its application profile together.
    pub fn create_with_profile(
        path: impl AsRef<Path>,
        dataset: DatasetId,
        catalog: Catalog,
        policy: SchedulerSettings,
        profile: Option<&str>,
    ) -> Result<Self, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.create_with_profile");
        let path = file_path(path.as_ref())?;
        let initial = initial_snapshot(dataset, catalog, policy)?;
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(database_error)?;
        configure(&connection)?;
        let transaction = crate::diagnostics::timed("sqlite.begin", || {
            connection.transaction_with_behavior(TransactionBehavior::Immediate)
        })
        .map_err(database_error)?;
        let objects: i64 = transaction
            .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0))
            .map_err(database_error)?;
        if schema_version(&transaction)? != 0 || objects != 0 {
            return Err(conflict("database already has a schema"));
        }
        transaction.execute_batch(SCHEMA).map_err(database_error)?;
        transaction
            .execute_batch(DAILY_ALLOWANCE_SCHEMA)
            .map_err(database_error)?;
        transaction
            .execute_batch(REPERTOIRE_DOCUMENT_SCHEMA)
            .map_err(database_error)?;
        transaction
            .execute_batch(GAME_EVIDENCE_SCHEMA)
            .map_err(database_error)?;
        transaction
            .execute_batch(GAME_REVIEW_BACKFILL)
            .map_err(database_error)?;
        transaction
            .execute_batch(REPERTOIRE_RESET_SCHEMA)
            .map_err(database_error)?;
        transaction
            .execute_batch(GAME_CORRECTIONS_SCHEMA)
            .map_err(database_error)?;
        transaction
            .execute_batch(PORTABLE_EVIDENCE_SCHEMA)
            .map_err(database_error)?;
        transaction
            .execute_batch(GAME_RELEVANCE_SCHEMA)
            .map_err(database_error)?;
        transaction
            .execute_batch(CORRECTIVE_RELEASE_SCHEMA)
            .map_err(database_error)?;
        transaction
            .execute_batch(SAVED_REVIEWS_SCHEMA)
            .map_err(database_error)?;
        transaction
            .execute_batch(REPERTOIRE_SUSPENSIONS_SCHEMA)
            .map_err(database_error)?;
        transaction
            .execute_batch(TACTICAL_ANALYSIS_SCHEMA)
            .map_err(database_error)?;
        transaction
            .execute_batch(TACTICAL_IDENTITY_SCHEMA)
            .map_err(database_error)?;
        transaction
            .execute_batch(TACTICAL_REPORT_GENERATION_SCHEMA)
            .map_err(database_error)?;
        transaction
            .execute_batch(REPERTOIRE_RUNS_SCHEMA)
            .map_err(database_error)?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(database_error)?;
        save_snapshot(&transaction, None, &initial)?;
        transaction
            .execute(
                "UPDATE store_state SET local_profile=?1 WHERE singleton=1",
                [profile],
            )
            .map_err(database_error)?;
        crate::diagnostics::timed("sqlite.commit", || transaction.commit())
            .map_err(database_error)?;
        Ok(Self {
            connection,
            choice_migration: None,
        })
    }

    /// List saved content sources for local provenance reconciliation.
    pub fn content_source_ids(&self) -> Result<std::collections::BTreeSet<String>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.content_source_ids");
        let mut query = self
            .connection
            .prepare("SELECT id FROM content_sources")
            .map_err(database_error)?;
        let rows = query
            .query_map([], |row| row.get(0))
            .map_err(database_error)?;
        rows.collect::<Result<_, _>>().map_err(database_error)
    }

    /// Read one archived PGN referenced by a current content source.
    pub fn archived_content_pgn(&self, id: &str) -> Result<Option<String>, StoreError> {
        self.connection
            .query_row("SELECT pgn FROM content_sources WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(database_error)
    }

    /// Backfill verified presentation metadata against the exact catalog read.
    /// Authored content, issued versions, learning and receipts are independent.
    pub fn backfill_opponent_lead_ins(
        &mut self,
        dataset: &DatasetId,
        expected: Revision,
        evidence: &BTreeMap<TargetId, LeadInEvidence>,
    ) -> Result<(), StoreError> {
        if evidence.is_empty() {
            return Ok(());
        }
        if evidence.values().any(LeadInEvidence::is_unavailable) {
            return Err(invalid("expected resolved lead-in evidence"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let revision = expected
            .0
            .checked_add(1)
            .ok_or_else(|| invalid("store revision exhausted"))?;
        let changed = tx.execute(
            "UPDATE store_state SET revision=?1 WHERE singleton=1 AND dataset_id=?2 AND revision=?3",
            params![revision.to_be_bytes().as_slice(), dataset.0, expected.0.to_be_bytes().as_slice()],
        ).map_err(database_error)?;
        if changed != 1 {
            return Err(conflict("snapshot changed; reload before retrying"));
        }
        {
            let mut read = tx.prepare_cached(
                "SELECT item -> '$.opponent_lead_in' FROM current_content WHERE dataset_id=?1 AND target_id=?2",
            ).map_err(database_error)?;
            let mut update = tx
                .prepare_cached(
                    "UPDATE current_content SET item=json_set(item,'$.opponent_lead_in',json(?1))
                 WHERE dataset_id=?2 AND target_id=?3",
                )
                .map_err(database_error)?;
            for (target, context) in evidence {
                let raw: Option<String> = read
                    .query_row(params![dataset.0, target.0], |row| row.get(0))
                    .optional()
                    .map_err(database_error)?
                    .ok_or_else(|| conflict("setup target changed; reload before retrying"))?;
                let previous: LeadInEvidence = raw
                    .as_deref()
                    .map(row_values::decode)
                    .transpose()?
                    .unwrap_or_default();
                let mut resolved = previous.clone();
                resolved.reconcile(context);
                if resolved == previous || resolved != *context {
                    return Err(conflict("setup context changed; reload before retrying"));
                }
                update
                    .execute(params![row_values::encode(context)?, dataset.0, target.0])
                    .map_err(database_error)?;
            }
        }
        tx.commit().map_err(database_error)
    }

    /// Read the local application's persisted settings representation.
    pub fn local_profile(&self) -> Result<Option<String>, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.local_profile");
        self.connection
            .query_row(
                "SELECT local_profile FROM store_state WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(database_error)
    }

    /// Replace settings if another editor has not changed the profile in the meantime.
    pub fn update_local_profile(&self, expected: &str, profile: &str) -> Result<(), StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.update_local_profile");
        let changed = self
            .connection
            .execute(
                "UPDATE store_state SET local_profile=?1 WHERE singleton=1 AND local_profile=?2",
                params![profile, expected],
            )
            .map_err(database_error)?;
        if changed != 1 {
            return Err(conflict("study settings changed; reload before retrying"));
        }
        Ok(())
    }

    /// Read participation for a worker about to dispatch a position shared by multiple books.
    pub fn suspended_repertoires(
        &self,
        dataset: &DatasetId,
    ) -> Result<std::collections::BTreeSet<String>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.suspended_repertoires");
        repertoire_suspensions::read(&self.connection, &dataset.0)
    }

    /// Inspect the current schema without creating or migrating a database.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let connection = Connection::open_with_flags(
            file_path(path.as_ref())?,
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(database_error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(database_error)?;
        if schema_version(&connection)? != SCHEMA_VERSION {
            return Err(invalid(
                "read-only inspection requires the current Store schema",
            ));
        }
        Ok(Self {
            connection,
            choice_migration: None,
        })
    }

    /// Open an existing database using the current schema. A missing file is an error.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.open");
        let path = file_path(path.as_ref())?;
        let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(database_error)?;
        configure(&connection)?;
        let mut choice_migration = None;
        if matches!(schema_version(&connection)?, 2..=15) {
            let tx = crate::diagnostics::timed("sqlite.begin", || {
                connection.transaction_with_behavior(TransactionBehavior::Immediate)
            })
            .map_err(database_error)?;
            if schema_version(&tx)? == 2 {
                tx.execute_batch(DAILY_ALLOWANCE_SCHEMA)
                    .map_err(database_error)?;
                tx.pragma_update(None, "user_version", 3)
                    .map_err(database_error)?;
            }
            if schema_version(&tx)? == 3 {
                tx.execute_batch(REPERTOIRE_DOCUMENT_SCHEMA)
                    .map_err(database_error)?;
                tx.pragma_update(None, "user_version", 4)
                    .map_err(database_error)?;
            }
            if schema_version(&tx)? == 4 {
                tx.execute_batch(GAME_EVIDENCE_SCHEMA)
                    .map_err(database_error)?;
                tx.pragma_update(None, "user_version", 5)
                    .map_err(database_error)?;
            }
            if schema_version(&tx)? == 5 {
                tx.execute_batch(GAME_REVIEW_BACKFILL)
                    .map_err(database_error)?;
                tx.pragma_update(None, "user_version", 6)
                    .map_err(database_error)?;
            }
            if schema_version(&tx)? == 6 {
                tx.execute_batch(REPERTOIRE_RESET_SCHEMA)
                    .map_err(database_error)?;
                tx.pragma_update(None, "user_version", 7)
                    .map_err(database_error)?;
            }
            if schema_version(&tx)? == 7 {
                tx.execute_batch(GAME_CORRECTIONS_SCHEMA)
                    .map_err(database_error)?;
                tx.pragma_update(None, "user_version", 8)
                    .map_err(database_error)?;
            }
            if schema_version(&tx)? == 8 {
                tx.execute_batch(PORTABLE_EVIDENCE_SCHEMA)
                    .map_err(database_error)?;
                tx.pragma_update(None, "user_version", 9)
                    .map_err(database_error)?;
            }
            if schema_version(&tx)? == 9 {
                tx.execute_batch(GAME_RELEVANCE_SCHEMA)
                    .map_err(database_error)?;
                tx.pragma_update(None, "user_version", 10)
                    .map_err(database_error)?;
            }
            if schema_version(&tx)? == 10 {
                tx.execute_batch(CORRECTIVE_RELEASE_SCHEMA)
                    .map_err(database_error)?;
                choice_migration = Some(game_choices::migrate(&tx)?);
                tx.pragma_update(None, "user_version", 11)
                    .map_err(database_error)?;
            }
            if schema_version(&tx)? == 11 {
                tx.execute_batch(SAVED_REVIEWS_SCHEMA)
                    .map_err(database_error)?;
                tx.pragma_update(None, "user_version", 12)
                    .map_err(database_error)?;
            }
            if schema_version(&tx)? == 12 {
                tx.execute_batch(REPERTOIRE_SUSPENSIONS_SCHEMA)
                    .map_err(database_error)?;
                tx.pragma_update(None, "user_version", 13)
                    .map_err(database_error)?;
            }
            if schema_version(&tx)? == 13 {
                tx.execute_batch(TACTICAL_ANALYSIS_SCHEMA)
                    .map_err(database_error)?;
                tx.pragma_update(None, "user_version", 14)
                    .map_err(database_error)?;
            }
            if schema_version(&tx)? == 14 {
                tx.execute_batch(TACTICAL_IDENTITY_SCHEMA)
                    .map_err(database_error)?;
                tactical_targets::backfill(&tx)?;
                tx.execute_batch(TACTICAL_REPORT_GENERATION_SCHEMA)
                    .map_err(database_error)?;
                tx.pragma_update(None, "user_version", 15)
                    .map_err(database_error)?;
            }
            if schema_version(&tx)? == 15 {
                tx.execute_batch(REPERTOIRE_RUNS_SCHEMA)
                    .map_err(database_error)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)
                    .map_err(database_error)?;
            }
            crate::diagnostics::timed("sqlite.commit", || tx.commit()).map_err(database_error)?;
        }
        let version = schema_version(&connection)?;
        if version != SCHEMA_VERSION {
            return Err(invalid(&format!(
                "unsupported SQLite store version {version}"
            )));
        }
        let store = Self {
            connection,
            choice_migration,
        };
        store.study_state(StudyScope::default())?;
        Ok(store)
    }

    /// Counts for an upgrade performed by this opening of the database.
    pub fn choice_migration_report(&self) -> Option<&ChoiceMigrationReport> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.choice_migration_report");
        self.choice_migration.as_ref()
    }

    /// Current model and Games inputs, without reading documents or targets.
    pub fn game_comparison_marker(
        &self,
    ) -> Result<(DatasetId, crate::GameComparisonBasis), StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.game_comparison_marker");
        game_relevance::comparison_marker(&self.connection)
    }

    /// Read desired books, source associations and active target facts at one revision.
    pub fn projection_books(
        &self,
        dataset: &DatasetId,
        scope: crate::ProjectionScope,
    ) -> Result<Vec<crate::ProjectionBook>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.projection_books");
        game_model::books(&self.connection, dataset, scope)
    }
}

mod tactic_page;

impl Store for SqliteStore {
    fn latest_applied_test(
        &self,
        dataset: &DatasetId,
        target: &TargetId,
        version: ContentVersion,
    ) -> Result<Option<Grade>, StoreError> {
        repertoire_runs::latest_test(&self.connection, dataset, target, version)
    }
    fn saved_review_page(
        &self,
        dataset: &DatasetId,
        before: Option<i64>,
    ) -> Result<Vec<crate::SavedReview>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.saved_review_page");
        saved_reviews::page(&self.connection, dataset, before)
    }

    fn saved_review_context(
        &self,
        dataset: &DatasetId,
        id: i64,
    ) -> Result<Option<crate::SavedReviewContext>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.saved_review_context");
        saved_reviews::context(&self.connection, dataset, id)
    }

    fn saved_review_receipt(
        &self,
        key: &crate::ReviewCommandKey<'_>,
    ) -> Result<Option<i64>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.saved_review_receipt");
        saved_reviews::receipt(&self.connection, key)
    }

    fn write_saved_review(
        &mut self,
        key: &crate::ReviewCommandKey<'_>,
        change: crate::ReviewWrite<'_>,
    ) -> Result<i64, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.write_saved_review");
        saved_reviews::write(&mut self.connection, key, change)
    }

    fn repertoire_activity(
        &self,
        command: &crate::RepertoireActivityCommand<'_>,
    ) -> Result<crate::RepertoireActivityRead, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.repertoire_activity");
        repertoire_suspensions::context(&self.connection, command)
    }

    fn set_repertoire_activity(
        &mut self,
        command: &crate::RepertoireActivityCommand<'_>,
        expected_revision: Revision,
        abandoned: Option<crate::ChessAttempt>,
    ) -> Result<crate::RepertoireActivityResult, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.set_repertoire_activity");
        repertoire_suspensions::apply(&mut self.connection, command, expected_revision, abandoned)
    }

    fn tactical_acceptance_basis(&self) -> Result<crate::TacticalAcceptanceBasis, StoreError> {
        let (dataset, revision) = tactical_targets::revision(&self.connection)?;
        Ok(crate::TacticalAcceptanceBasis { dataset, revision })
    }

    fn tactical_content_matches(
        &self,
        dataset: &DatasetId,
        lookup: &tempo_chess::TacticLookup,
    ) -> Result<Vec<ContentItem>, StoreError> {
        tactical_targets::matches(&self.connection, dataset, lookup)
    }

    fn tactical_target_learning(
        &self,
        dataset: &DatasetId,
        target: &TargetId,
    ) -> Result<Option<LearningState>, StoreError> {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        if &tactical_targets::revision(&tx)?.0 != dataset {
            return Err(conflict("tactical analysis dataset changed"));
        }
        tactical_targets::learning(&tx, target)
    }

    fn commit_tactical_analysis(
        &mut self,
        commit: crate::TacticalCommit,
        analysis: &crate::TacticalAnalysis,
    ) -> Result<crate::TacticalAcceptance, StoreError> {
        tactical_analysis::accept(&mut self.connection, commit, analysis)
    }
    fn pending_tactical_analyses(
        &self,
        dataset: &DatasetId,
        detector: &str,
        version: &str,
        after: Option<&crate::TacticalGameCursor>,
        limit: usize,
    ) -> Result<Vec<crate::TacticalGame>, StoreError> {
        tactical_analysis::pending_games(&self.connection, dataset, detector, version, after, limit)
    }
    fn regular_review_owed(&self, dataset: &DatasetId) -> Result<bool, StoreError> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM tactical_study_turn WHERE dataset_id=?1)",
                [&dataset.0],
                |row| row.get(0),
            )
            .map_err(database_error)
    }

    fn tactical_training_binding(
        &self,
        dataset: &DatasetId,
        game: &crate::TacticalGame,
        report: &str,
        address: crate::TacticalRequestAddress<'_>,
    ) -> Result<Option<crate::TacticalPuzzleRef>, StoreError> {
        tactical_analysis::training_binding(&self.connection, dataset, game, report, address)
    }

    fn tactical_lead_in(
        &self,
        dataset: &DatasetId,
        request_id: u64,
    ) -> Result<Option<tempo_chess::OpponentLeadIn>, StoreError> {
        tactical_analysis::lead_in(&self.connection, dataset, request_id)
    }

    fn pending_tactical_requests(
        &self,
        dataset: &DatasetId,
    ) -> Result<Vec<crate::TacticalRequest>, StoreError> {
        tactical_analysis::pending_requests(&self.connection, dataset)
    }
    fn has_pending_tactical_request_for_attempt(
        &self,
        dataset: &DatasetId,
        attempt: &AttemptId,
    ) -> Result<bool, StoreError> {
        tactical_analysis::has_pending_request_for_attempt(&self.connection, dataset, attempt)
    }
    fn commit_daily_issuance(
        &mut self,
        commit: Commit,
        scope: StudyScope<'_>,
        immediate_request: Option<u64>,
        ordering: crate::StudyOrderingWrite,
    ) -> Result<Revision, StoreError> {
        tactical_analysis::issue_daily(
            &mut self.connection,
            commit,
            scope,
            immediate_request,
            ordering,
        )
    }
    fn tactical_statistics(
        &self,
        dataset: &DatasetId,
        detector: &str,
        version: &str,
    ) -> Result<crate::TacticalStatistics, StoreError> {
        tactical_analysis::statistics(&self.connection, dataset, detector, version)
    }
    fn tactical_observations(
        &self,
        dataset: &DatasetId,
        detector: &str,
        version: &str,
        before: Option<u64>,
        limit: usize,
    ) -> Result<crate::TacticalObservationPage, StoreError> {
        tactical_analysis::observations(&self.connection, dataset, detector, version, before, limit)
    }
    fn reset_repertoire(
        &mut self,
        dataset: &DatasetId,
        repertoire: &str,
        command_id: &str,
        at_ms: u64,
        fresh: &LearningState,
    ) -> Result<RepertoireReset, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.reset_repertoire");
        repertoire_resets::apply(
            &mut self.connection,
            dataset,
            repertoire,
            command_id,
            at_ms,
            fresh,
        )
    }

    fn repertoire_reset_at(
        &self,
        dataset: &DatasetId,
        repertoire: &str,
    ) -> Result<Option<u64>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.repertoire_reset_at");
        self.connection.query_row(
            "SELECT max(reset_at_ms) FROM (
                SELECT reset_at_ms FROM repertoire_resets WHERE dataset_id=?1 AND repertoire_id=?2
                UNION ALL
                SELECT reset_at_ms FROM restored_reset_boundaries WHERE dataset_id=?1 AND repertoire_id=?2
            )",
            params![dataset.0, repertoire],
            |row| row.get(0),
        ).map_err(database_error)
    }

    fn repertoire_reset_generation(
        &self,
        dataset: &DatasetId,
        repertoire: &str,
    ) -> Result<u64, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.repertoire_reset_generation");
        self.connection
            .query_row(
                "SELECT count(*) FROM repertoire_resets WHERE dataset_id=?1 AND repertoire_id=?2",
                params![dataset.0, repertoire],
                |row| row.get(0),
            )
            .map_err(database_error)
    }

    fn commit_games(
        &mut self,
        commit: Commit,
        games: &[crate::ComparedGame],
        successful_refresh_ms: Option<u64>,
        receipt: crate::GameReceipt<'_>,
    ) -> Result<crate::AcceptedGames, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.commit_games");
        self.save_game_evidence(commit, games, successful_refresh_ms, receipt)
    }

    fn game_receipt(
        &self,
        dataset: &DatasetId,
        id: &str,
        fingerprint: &str,
    ) -> Result<Option<crate::AcceptedGames>, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.game_receipt");
        self.read_game_receipt(dataset, id, fingerprint)
    }

    fn pgn_timing(
        &self,
        dataset: &DatasetId,
        id: &str,
    ) -> Result<Option<crate::PgnTiming>, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.pgn_timing");
        game_evidence::read_pgn_timing(&self.connection, &dataset.0, id)
    }

    fn verified_game(
        &self,
        dataset: &DatasetId,
        url: &str,
        signature: &str,
    ) -> Result<Option<crate::GameRecord>, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.verified_game");
        self.read_verified_game(dataset, url, signature)
    }

    fn legacy_game_misses(
        &self,
        dataset: &DatasetId,
        target: &TargetId,
        version: ContentVersion,
    ) -> Result<Vec<crate::LegacyGameMiss>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.legacy_game_misses");
        let tx = crate::diagnostics::timed("sqlite.begin", || {
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
        })
        .map_err(database_error)?;
        row_values::read_selected(&tx,
            "SELECT DISTINCT game.source,game.id,json_extract(comparison.summary,'$.learner'),observation.ply,game.start_ms,game.end_ms
             FROM game_observations AS observation
             JOIN game_comparisons AS comparison ON comparison.dataset_id=observation.dataset_id AND comparison.id=observation.comparison_id
             JOIN games AS game ON game.dataset_id=comparison.dataset_id AND game.source=comparison.source AND game.id=comparison.game_id
             WHERE observation.dataset_id=?1 AND observation.target_id=?2 AND observation.version=?3
               AND observation.played<>observation.prescribed AND json_extract(comparison.summary,'$.correction_effect') IS NULL
             ORDER BY game.source,game.id,observation.ply",
            params![dataset.0, target.0, version.0.to_be_bytes().as_slice()], |row| {
                Ok(crate::LegacyGameMiss {
                    source: row.get(0).map_err(database_error)?, game_id: row.get(1).map_err(database_error)?,
                    learner: row.get(2).map_err(database_error)?, ply: row.get(3).map_err(database_error)?,
                    start_ms: row.get(4).map_err(database_error)?, end_ms: row.get(5).map_err(database_error)?,
                })
            })
    }

    fn game_comparison(
        &self,
        dataset: &DatasetId,
        id: &str,
    ) -> Result<Option<crate::GameSummary>, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.game_comparison");
        let saved: Option<String> = self
            .connection
            .query_row(
                "SELECT summary FROM game_comparisons WHERE dataset_id=?1 AND id=?2",
                params![dataset.0, id],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)?;
        saved.as_deref().map(row_values::decode).transpose()
    }

    fn game_choices(
        &self,
        dataset: &DatasetId,
        source: &str,
        game_id: &str,
    ) -> Result<crate::GameChoices, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.game_choices");
        game_choices::read(&self.connection, dataset, source, game_id)
    }

    fn choice_encounter_keys(
        &self,
        dataset: &DatasetId,
        after: Option<&crate::EncounterKey>,
    ) -> Result<Vec<crate::EncounterKey>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.choice_encounter_keys");
        game_relevance::choice_encounter_keys(&self.connection, dataset, after)
    }

    fn repair_choice_encounters(
        &mut self,
        dataset: &DatasetId,
        basis: crate::GameComparisonBasis,
        repairs: &[crate::EncounterRepair],
    ) -> Result<usize, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.repair_choice_encounters");
        game_relevance::repair_choice_encounters(&mut self.connection, dataset, basis, repairs)
    }

    fn game_failed(
        &self,
        dataset: &DatasetId,
        key: &crate::GameFailureKey,
    ) -> Result<bool, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.game_failed");
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM game_failures WHERE dataset_id=?1 AND source=?2 AND game_id=?3 AND target_id=?4)",
            params![dataset.0, key.source, key.game_id, key.target.0], |row| row.get(0)
        ).map_err(database_error)
    }

    fn game_failures(&self, dataset: &DatasetId) -> Result<Vec<crate::GameFailureKey>, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.game_failures");
        let tx = crate::diagnostics::timed("sqlite.begin", || {
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
        })
        .map_err(database_error)?;
        game_evidence::read_game_failures(&tx, dataset)
    }

    fn portable_state(&self) -> Result<(Snapshot, crate::PortableGameEvidence), StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.portable_state");
        let tx = crate::diagnostics::timed("sqlite.begin", || {
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
        })
        .map_err(database_error)?;
        let snapshot = load_snapshot(&tx)?;
        let evidence = game_evidence::read_portable_evidence(&tx, &snapshot.dataset)?;
        crate::diagnostics::timed("sqlite.commit", || tx.commit()).map_err(database_error)?;
        Ok((snapshot, evidence))
    }

    fn restore_with_game_evidence(
        &mut self,
        commit: Commit,
        evidence: &crate::PortableGameEvidence,
        choice_format: crate::PortableChoiceFormat,
    ) -> Result<Revision, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.restore_with_game_evidence");
        if !matches!(commit.writes.as_slice(), [Write::Restore { .. }]) {
            return Err(invalid("portable evidence requires a dataset restore"));
        }
        let tx = crate::diagnostics::timed("sqlite.begin", || {
            self.connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
        })
        .map_err(database_error)?;
        let previous = load_snapshot(&tx)?;
        let next = stage_commit(&previous, commit)?;
        save_snapshot(&tx, Some(&previous), &next)?;
        game_evidence::save_portable_evidence(&tx, &next, evidence, choice_format)?;
        crate::diagnostics::timed("sqlite.commit", || tx.commit()).map_err(database_error)?;
        Ok(next.revision)
    }

    fn correction_game(
        &self,
        lookup: crate::CorrectionGameLookup<'_>,
    ) -> Result<Option<crate::CorrectionGame>, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.correction_game");
        let tx = crate::diagnostics::timed("sqlite.begin", || {
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
        })
        .map_err(database_error)?;
        game_evidence::correction_game(&tx, lookup)
    }

    fn stored_game(
        &self,
        dataset: &DatasetId,
        source: &str,
        id: &str,
    ) -> Result<Option<crate::GameRecord>, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.stored_game");
        self.connection.query_row(
            "SELECT source,id,url,white,black,start_ms,end_ms,fetched_ms,pgn,coalesce(mainline_signature,''),played_date_ms FROM games
             WHERE dataset_id=?1 AND source=?2 AND id=?3",
            params![dataset.0, source, id], game_evidence::game_row
        ).optional().map_err(database_error)
    }

    fn games_in_window(
        &self,
        dataset: &DatasetId,
        from: u64,
        to: u64,
    ) -> Result<Vec<crate::GameRecord>, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.games_in_window");
        let mut query = self.connection.prepare(
            "SELECT source,id,url,white,black,start_ms,end_ms,fetched_ms,pgn,coalesce(mainline_signature,''),played_date_ms FROM games
             WHERE dataset_id=?1 AND coalesce(start_ms,end_ms,played_date_ms,fetched_ms)>=?2 AND coalesce(start_ms,end_ms,played_date_ms,fetched_ms)<?3
             ORDER BY coalesce(start_ms,end_ms,played_date_ms,fetched_ms),source,id LIMIT 501"
        ).map_err(database_error)?;
        query
            .query_map(params![dataset.0, from, to], game_evidence::game_row)
            .map_err(database_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)
    }

    fn historical_pgn_window(
        &self,
        dataset: &DatasetId,
        learner: &str,
        from: u64,
        to: u64,
    ) -> Result<Vec<crate::GameRecord>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.historical_pgn_window");
        let mut query = self.connection.prepare_cached(
            "SELECT source,id,url,white,black,start_ms,end_ms,fetched_ms,pgn,coalesce(mainline_signature,''),played_date_ms
             FROM games WHERE dataset_id=?1 AND source='pgn' AND (lower(white)=?2) != (lower(black)=?2)
             AND coalesce(start_ms,end_ms,played_date_ms)>=?3 AND coalesce(start_ms,end_ms,played_date_ms)<?4
             ORDER BY coalesce(start_ms,end_ms,played_date_ms),id LIMIT 501"
        ).map_err(database_error)?;
        query
            .query_map(
                params![dataset.0, learner, from, to],
                game_evidence::game_row,
            )
            .map_err(database_error)?
            .collect::<Result<_, _>>()
            .map_err(database_error)
    }

    fn game_relevance(
        &self,
        dataset: &DatasetId,
        model: &str,
    ) -> Result<std::collections::BTreeMap<TargetId, crate::GameTargetRelevance>, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.game_relevance");
        game_relevance::read(&self.connection, dataset, model)
    }

    fn game_comparison_basis(
        &self,
        dataset: &DatasetId,
    ) -> Result<Option<crate::GameComparisonBasis>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.game_comparison_basis");
        let (current, basis) = self.game_comparison_marker()?;
        Ok((&current == dataset).then_some(basis))
    }

    fn opponent_gaps(
        &self,
        dataset: &DatasetId,
    ) -> Result<Vec<crate::GroupedOpponentGap>, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.opponent_gaps");
        game_relevance::gaps(&self.connection, dataset)
    }

    fn game_backfill_state(
        &self,
        dataset: &DatasetId,
        id: Option<&str>,
    ) -> Result<Option<String>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.game_backfill_state");
        self.connection
            .query_row(
                "SELECT state FROM game_backfill_jobs WHERE dataset_id=?1 AND (?2 IS NULL OR id=?2)
             ORDER BY rowid DESC LIMIT 1",
                params![dataset.0, id],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)
    }

    fn save_game_backfill_state(
        &mut self,
        dataset: &DatasetId,
        id: &str,
        state: &str,
        accounts_to_recognize: Option<&[(String, String)]>,
    ) -> Result<(), StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.save_game_backfill_state");
        let tx = crate::diagnostics::timed("sqlite.begin", || {
            self.connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
        })
        .map_err(database_error)?;
        game_relevance::check_dataset(&tx, dataset)?;
        tx.execute(
            "INSERT INTO game_backfill_jobs VALUES (?1,?2,?3)
             ON CONFLICT(dataset_id,id) DO UPDATE SET state=excluded.state",
            params![dataset.0, id, state],
        )
        .map_err(database_error)?;
        let mut recognition_changed = false;
        for (source, learner) in accounts_to_recognize.unwrap_or_default() {
            recognition_changed |= tx
                .execute(
                    "INSERT INTO game_learner_accounts VALUES (?1,?2,?3)
                     ON CONFLICT(dataset_id,source,learner) DO NOTHING",
                    params![dataset.0, source, learner],
                )
                .map_err(database_error)?
                != 0;
        }
        if recognition_changed {
            game_relevance::rebuild(&tx, &dataset.0)?;
        }
        crate::diagnostics::timed("sqlite.commit", || tx.commit()).map_err(database_error)
    }

    fn game_move_distribution(
        &self,
        model: &str,
        position: &str,
    ) -> Result<Option<std::collections::BTreeMap<String, f64>>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.game_move_distribution");
        let json: Option<String> = self
            .connection
            .query_row(
                "SELECT distribution FROM game_move_models WHERE model_key=?1 AND position=?2",
                params![model, position],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)?;
        json.as_deref()
            .map(game_relevance::decode_distribution)
            .transpose()
    }

    fn save_game_move_distribution(
        &mut self,
        model: &str,
        position: &str,
        distribution: &std::collections::BTreeMap<String, f64>,
    ) -> Result<(), StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.save_game_move_distribution");
        self.connection.execute(
            "INSERT INTO game_move_models VALUES (?1,?2,?3) ON CONFLICT(model_key,position) DO NOTHING",
            params![model, position, row_values::encode(distribution)?],
        ).map_err(database_error)?;
        Ok(())
    }

    fn replace_game_model_reach(
        &mut self,
        dataset: &DatasetId,
        projection: crate::BookProjection<'_>,
    ) -> Result<(), StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.replace_game_model_reach");
        game_model::replace(&mut self.connection, dataset, projection)
    }

    fn game_model_projection_status(
        &self,
        dataset: &DatasetId,
    ) -> Result<Option<crate::ProjectionStatus>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.game_model_projection_status");
        game_model::status(&self.connection, dataset).map(Some)
    }

    fn model_bootstrap_repertoires(&self, dataset: &DatasetId) -> Result<Vec<String>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.model_bootstrap_repertoires");
        game_model::bootstrap_repertoires(&self.connection, dataset)
    }

    fn finish_model_bootstrap(
        &mut self,
        dataset: &DatasetId,
        content_revision: Revision,
        repertoire: &str,
        source_id: &str,
        stamp: Option<&crate::ModelInputStamp>,
    ) -> Result<(), StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.finish_model_bootstrap");
        game_model::finish_bootstrap(
            &mut self.connection,
            dataset,
            content_revision,
            repertoire,
            source_id,
            stamp,
        )
    }

    fn game_library(
        &self,
        dataset: &DatasetId,
        through: Option<i64>,
        before: Option<i64>,
    ) -> Result<crate::GameLibraryPage, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.game_library");
        self.read_game_library(dataset, through, before)
    }

    fn game_history(&self) -> Result<crate::GameHistory, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.game_history");
        self.read_game_history()
    }

    fn tactic_page(
        &self,
        query: crate::TacticPageQuery<'_>,
    ) -> Result<crate::TacticPage, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.tactic_page");
        self.read_tactic_page(query)
    }

    fn snapshot(&self) -> Result<Snapshot, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.snapshot");
        let transaction = crate::diagnostics::timed("sqlite.begin", || {
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
        })
        .map_err(database_error)?;
        let snapshot = load_snapshot(&transaction)?;
        crate::diagnostics::timed("sqlite.commit", || transaction.commit())
            .map_err(database_error)?;
        Ok(snapshot)
    }

    fn study_state(&self, scope: StudyScope<'_>) -> Result<StudyState, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.study_state");
        let transaction = crate::diagnostics::timed("sqlite.begin", || {
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
        })
        .map_err(database_error)?;
        let state = load_study::load_study(&transaction, scope)?;
        crate::diagnostics::timed("sqlite.commit", || transaction.commit())
            .map_err(database_error)?;
        Ok(state)
    }

    fn stored_attempt(&self, id: &AttemptId) -> Result<Option<StoredAttempt>, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.stored_attempt");
        let record: Option<String> = self
            .connection
            .query_row("SELECT record FROM attempts WHERE id=?1", [&id.0], |row| {
                row.get(0)
            })
            .optional()
            .map_err(database_error)?;
        record.as_deref().map(row_values::decode).transpose()
    }

    fn issued_item(
        &self,
        dataset: &DatasetId,
        target: &TargetId,
        version: ContentVersion,
    ) -> Result<Option<ContentItem>, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.issued_item");
        let item: Option<String> = self.connection.query_row(
            "SELECT item FROM content_history WHERE dataset_id=?1 AND target_id=?2 AND version=?3",
            params![dataset.0, target.0, version.0.to_be_bytes().as_slice()], |row| row.get(0)
        ).optional().map_err(database_error)?;
        item.as_deref().map(row_values::decode).transpose()
    }

    fn tactics_extra(&self, dataset: &DatasetId, day: StudyDay) -> Result<usize, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.tactics_extra");
        self.connection
            .query_row(
                "SELECT extra FROM daily_tactics_allowances WHERE dataset_id=?1 AND study_day=?2",
                params![dataset.0, day.0],
                |row| row.get(0),
            )
            .optional()
            .map(|extra| extra.unwrap_or(0))
            .map_err(database_error)
    }

    fn extend_tactics_allowance(
        &mut self,
        dataset: &DatasetId,
        revision: Revision,
        day: StudyDay,
        increase: usize,
        receipt: &Receipt,
    ) -> Result<(), StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.extend_tactics_allowance");
        if increase == 0
            || receipt.id.0.is_empty()
            || !matches!(receipt.result, ReceiptResult::TacticsAllowance { .. })
        {
            return Err(invalid("expected a positive allowance and its receipt"));
        }
        let next = revision
            .0
            .checked_add(1)
            .ok_or_else(|| invalid("store revision exhausted"))?;
        let tx = crate::diagnostics::timed("sqlite.begin", || {
            self.connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
        })
        .map_err(database_error)?;
        let changed = tx
            .execute(
                "UPDATE store_state SET revision=?1 WHERE singleton=1 AND dataset_id=?2
             AND revision=?3 AND active_attempt_id IS NULL",
                params![
                    next.to_be_bytes().as_slice(),
                    dataset.0,
                    revision.0.to_be_bytes().as_slice()
                ],
            )
            .map_err(database_error)?;
        if changed != 1 {
            return Err(conflict("study changed before the allowance was applied"));
        }
        tx.execute(
            "INSERT INTO daily_tactics_allowances VALUES (?1, ?2, ?3)
             ON CONFLICT(dataset_id, study_day) DO UPDATE SET extra=extra+excluded.extra",
            params![dataset.0, day.0, increase],
        )
        .map_err(database_error)?;
        tx.execute(
            "INSERT INTO receipts VALUES (?1, NULL, ?2, ?3)",
            params![
                receipt.id.0,
                receipt.command,
                row_values::encode(&receipt.result)?
            ],
        )
        .map_err(database_error)?;
        crate::diagnostics::timed("sqlite.commit", || tx.commit()).map_err(database_error)?;
        Ok(())
    }

    fn content_state(&self) -> Result<ContentState, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.content_state");
        let tx = crate::diagnostics::timed("sqlite.begin", || {
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Deferred)
        })
        .map_err(database_error)?;
        let state = load_study::load_study(
            &tx,
            StudyScope {
                catalog: true,
                ..StudyScope::default()
            },
        )?;
        let (_, basis) = game_relevance::comparison_marker(&tx)?;
        crate::diagnostics::timed("sqlite.commit", || tx.commit()).map_err(database_error)?;
        ContentState::from_study(state, basis)
    }

    fn commit_study(
        &mut self,
        commit: Commit,
        scope: StudyScope<'_>,
    ) -> Result<Revision, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.commit_study");
        if commit
            .writes
            .iter()
            .any(|write| matches!(write, Write::Catalog(_) | Write::Restore { .. }))
        {
            return Err(invalid("content writes require a content transaction"));
        }
        let transaction = crate::diagnostics::timed("sqlite.begin", || {
            self.connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
        })
        .map_err(database_error)?;
        let state = load_study::load_study(&transaction, scope)?;
        let previous = load_study::staging_image(state);
        let next = stage_commit(&previous, commit)?;
        save_snapshot(&transaction, Some(&previous), &next)?;
        crate::diagnostics::timed("sqlite.commit", || transaction.commit())
            .map_err(database_error)?;
        Ok(next.revision)
    }

    fn commit(&mut self, commit: Commit) -> Result<Revision, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.commit");
        self.commit_content(commit, None, None)
    }

    fn repertoire_document(&self, id: &str) -> Result<RepertoireDocument, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.repertoire_document");
        let (dataset, revision, item, pgn, source_id, authority, suspended):
            (_, u64, Option<String>, Option<String>, _, Option<String>, bool) = self.connection
            .query_row(
                "SELECT state.dataset_id, state.content_revision,
                        (SELECT item FROM current_content
                         WHERE dataset_id=state.dataset_id AND repertoire=?1 ORDER BY target_id LIMIT 1),
                        CASE WHEN input.state='unavailable' THEN NULL ELSE source.pgn END,
                        document.source_id,input.state,
                        EXISTS(SELECT 1 FROM repertoire_suspensions
                            WHERE dataset_id=state.dataset_id AND repertoire_id=?1)
                 FROM store_state AS state
                 LEFT JOIN repertoire_documents AS document
                    ON document.dataset_id=state.dataset_id AND document.repertoire_id=?1
                 LEFT JOIN content_sources AS source ON source.id=document.source_id
                 LEFT JOIN game_model_inputs AS input
                    ON (input.dataset_id,input.repertoire_id)=(document.dataset_id,document.repertoire_id)
                 WHERE state.singleton=1",
                [id],
                |row| Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                    row.get(4)?, row.get(5)?, row.get(6)?)),
            )
            .map_err(database_error)?;
        Ok(RepertoireDocument {
            suspended,
            dataset: DatasetId(dataset),
            revision: Revision(revision),
            item: item.as_deref().map(row_values::decode).transpose()?,
            pgn,
            source_id,
            model_authority: match authority.as_deref() {
                Some("needs_bootstrap") => crate::ModelAuthority::NeedsBootstrap,
                Some("verified") => crate::ModelAuthority::Verified,
                _ => crate::ModelAuthority::Unavailable,
            },
        })
    }

    fn repertoire_introductions(
        &self,
        dataset: &DatasetId,
        revision: Revision,
        repertoire: &str,
        positions: &[tempo_chess::PositionKey],
    ) -> Result<std::collections::BTreeMap<tempo_chess::PositionKey, bool>, StoreError> {
        let _diagnostic =
            crate::diagnostics::Operation::new("sqlite.sqlite_store.repertoire_introductions");
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let current: Option<u64> = tx
            .query_row(
                "SELECT state.content_revision FROM store_state AS state
             WHERE state.singleton=1 AND state.dataset_id=?1
               AND NOT EXISTS (SELECT 1 FROM repertoire_suspensions
                   WHERE dataset_id=state.dataset_id AND repertoire_id=?2)",
                params![dataset.0, repertoire],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)?;
        if current != Some(revision.0) {
            return Err(conflict("The repertoire changed while reading the game."));
        }
        let mut facts = std::collections::BTreeMap::new();
        let mut stmt = tx.prepare_cached(
            "SELECT c.item,l.state FROM current_content AS c JOIN learning AS l USING(target_id)
             WHERE c.dataset_id=?1 AND c.repertoire=?2 AND c.position=?3"
        ).map_err(database_error)?;
        for position in positions {
            let row: Option<(String, String)> = stmt
                .query_row(params![dataset.0, repertoire, position.0], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()
                .map_err(database_error)?;
            if let Some((item, state)) = row {
                let item: ContentItem = row_values::decode(&item)?;
                if item.active() {
                    let learning: LearningState = row_values::decode(&state)?;
                    facts.insert(position.clone(), learning.introduced);
                }
            }
        }
        Ok(facts)
    }

    fn content_receipt(&self, request: &str) -> Result<Option<String>, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.content_receipt");
        self.connection
            .query_row(
                "SELECT reply FROM content_receipts WHERE request=?1",
                [request],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)
    }

    fn commit_content(
        &mut self,
        commit: Commit,
        source: Option<ContentSource<'_>>,
        receipt: Option<ContentReceipt<'_>>,
    ) -> Result<Revision, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.commit_content");
        let transaction = crate::diagnostics::timed("sqlite.begin", || {
            self.connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
        })
        .map_err(database_error)?;
        let dataset = commit.expected_dataset.clone();
        if let Some(ContentSource::RepertoireDocument {
            expected_content_revision,
            ..
        }) = &source
        {
            game_relevance::check_content_revision(&transaction, *expected_content_revision)?;
        }
        let no_writes_with_receipt = commit.writes.is_empty() && receipt.is_some();
        let (mut revision, catalog_changed) = if no_writes_with_receipt {
            let current = load_study::load_study(&transaction, StudyScope::default())?;
            if commit.expected_dataset != current.dataset
                || commit.expected_revision != current.revision
            {
                return Err(conflict("snapshot changed; reload before retrying"));
            }
            (current.revision, false)
        } else {
            let previous = load_snapshot(&transaction)?;
            let next = stage_commit(&previous, commit)?;
            save_snapshot(&transaction, Some(&previous), &next)?;
            (
                next.revision,
                previous.dataset != next.dataset || previous.catalog != next.catalog,
            )
        };
        if let Some(source) = source {
            let (id, pgn, repertoire) = match source {
                ContentSource::Archive { id, pgn } => (id, pgn, None),
                ContentSource::RepertoireDocument {
                    id,
                    pgn,
                    repertoire_id,
                    model_input,
                    ..
                } => (id, pgn, Some((repertoire_id, model_input))),
            };
            let inserted = transaction
                .execute(
                    "INSERT INTO content_sources VALUES (?1, ?2) ON CONFLICT DO NOTHING",
                    params![id, pgn],
                )
                .map_err(database_error)?;
            if inserted == 0 {
                let matches: bool = transaction
                    .query_row(
                        "SELECT pgn=?2 FROM content_sources WHERE id=?1",
                        params![id, pgn],
                        |row| row.get(0),
                    )
                    .map_err(database_error)?;
                if !matches {
                    return Err(invalid(
                        "archived content source disagrees with the supplied document",
                    ));
                }
            }
            if let Some((repertoire, model_input)) = repertoire {
                let exists: bool = transaction
                    .query_row(
                        "SELECT EXISTS (SELECT 1 FROM current_content
                     WHERE dataset_id=?1 AND repertoire=?2)",
                        params![dataset.0, repertoire],
                        |row| row.get(0),
                    )
                    .map_err(database_error)?;
                if !exists {
                    return Err(invalid(
                        "current document requires a repertoire in the active dataset",
                    ));
                }
                let changed = transaction.execute(
                    "INSERT INTO repertoire_documents (dataset_id,repertoire_id,source_id) VALUES (?1, ?2, ?3)
                     ON CONFLICT(dataset_id, repertoire_id) DO UPDATE SET source_id=excluded.source_id
                     WHERE source_id<>excluded.source_id",
                    params![dataset.0, repertoire, id],
                ).map_err(database_error)? != 0;
                let verified =
                    game_model::set_verified(&transaction, &dataset, repertoire, model_input)?;
                if (changed || verified) && !catalog_changed {
                    if no_writes_with_receipt {
                        revision = Revision(
                            revision
                                .0
                                .checked_add(1)
                                .ok_or_else(|| invalid("store revision exhausted"))?,
                        );
                    }
                    transaction.execute(
                        "UPDATE store_state SET content_revision=content_revision+1, revision=?1",
                        [revision.0.to_be_bytes().as_slice()],
                    ).map_err(database_error)?;
                }
            }
        }
        if let Some(receipt) = receipt {
            transaction
                .execute(
                    "INSERT INTO content_receipts VALUES (?1, ?2)",
                    params![receipt.request, receipt.reply],
                )
                .map_err(database_error)?;
        }
        crate::diagnostics::timed("sqlite.commit", || transaction.commit())
            .map_err(database_error)?;
        Ok(revision)
    }
}

fn file_path(path: &Path) -> Result<&Path, StoreError> {
    if path.as_os_str().is_empty() || path == Path::new(":memory:") {
        return Err(invalid("SQLite store needs a file path"));
    }
    Ok(path)
}

fn configure(connection: &Connection) -> Result<(), StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.sqlite_store.configure");
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(database_error)?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")
        .map_err(database_error)
}

fn schema_version(connection: &Connection) -> Result<i64, StoreError> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(database_error)
}

fn database_error(error: rusqlite::Error) -> StoreError {
    crate::diagnostics::sqlite_error(&error);
    StoreError::Unavailable(format!("SQLite store: {error}"))
}

#[cfg(test)]
mod tests;
