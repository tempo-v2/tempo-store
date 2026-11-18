# Tempo Store

Store is Tempo's typed persistence boundary. `MemoryStore` publishes atomic changes
in process memory. `SqliteStore` commits the same typed batches to a local SQLite
file. The application composes transactions and passes domain components values.

## Read the boundary in this order

| Concept | What it means | What it does not do |
| --- | --- | --- |
| `Snapshot` | One consistent owned view of current data and retained history. | Editing the returned value does not persist changes. |
| `Write` | One typed change requested by the application. | A write does not independently publish a partial operation. |
| `Commit` | The complete batch and the snapshot stamps it was based on. | It does not calculate grades, learning policy or content changes. |
| `Store` | The port that returns snapshots and accepts atomic commits. | It does not choose which application operation to run. |
| `MemoryStore` | A process-local adapter for demos and contract tests. | It has no persistent state. |
| `SqliteStore` | A file-backed adapter for restarting with the same data. | It accepts the current version-1 schema. |

All public concepts now have named files, “When to use” documentation and
same-file Rust usage tests. Existing root imports remain available. Start with
the full answer workflow in [commit.rs](src/commit.rs): it visibly composes
Study progress, a Learning transition, the assessment record and the exact
reply before committing them together.

## Public values and their examples

| Concept | File | Usage scenario in that file |
| --- | --- | --- |
| Consistent read view | [snapshot.rs](src/snapshot.rs) | Local edits do not persist, and older snapshots remain consistent after a commit. |
| Typed persistence requests | [write.rs](src/write.rs) | Restore under a new dataset identity and reject reuse of an old empty dataset. |
| Atomic application batch | [commit.rs](src/commit.rs) | Full answer → assessment → Learning → receipt composition; stale revisions and duplicate effects fail. |
| Adapter interface | [store.rs](src/store.rs) | Issue an attempt through `&mut dyn Store`, without depending on memory internals. |
| In-memory implementation | [memory_store.rs](src/memory_store.rs) | Inject a failure before publication, prove no partial effects, and retry the same batch. |
| SQLite implementation | [sqlite_store.rs](src/sqlite_store.rs) | Create a temporary database, reopen it, and read the persisted snapshot. |
| Concrete Study attempt alias | [chess_attempt.rs](src/chess_attempt.rs) | The persisted alias is the same value Study advances. |
| Attempt plus issue context | [stored_attempt.rs](src/stored_attempt.rs) | Reject stale miss context/missing prompts; issue a correctly stamped attempt. |
| Assessment uniqueness identity | [assessment_key.rs](src/assessment_key.rs) | External key and source must name the same provider event. |
| Durable learning outcome | [assessment_record.rs](src/assessment_record.rs) | An assessment cannot commit without its resulting learning state. |
| Reply from one Study command | [answer_result.rs](src/answer_result.rs) | Correction keeps the terminal miss while reporting no new grade/effect. |
| Study or external reply | [receipt_result.rs](src/receipt_result.rs) | Game feedback creates a replayable reply without a human attempt. |
| Submission replay record | [receipt.rs](src/receipt.rs) | Reject inconsistent replies; retain exact historical replies after restore. |
| Versioned content archive | [content_history.rs](src/content_history.rs) | Keep both semantic versions; a label edit does not add another version. |
| Persistence failure | [store_error.rs](src/store_error.rs) | A stale snapshot produces a conflict and preserves the newer committed state. |

### Similar-looking values with different jobs

**`StoredAttempt` versus `ChessAttempt`.** The generic Study attempt owns
progress, presentation, target/content version and terminal outcome. The
persistence wrapper adds issue time and the learning miss revision at issuance.
The latter protects against credit from an attempt started before a newer miss.

**`AssessmentRecord` versus `Receipt`.** An assessment records what Learning
accepted and the resulting transition. A receipt records the exact reply to one
submission. An intermediate answer or correction can have a receipt without a
new assessment. One attempt can therefore have several receipts but at most one
terminal human assessment.

**The different stamps.** The snapshot's store `revision` protects a complete
transaction. The attempt's response `revision` protects an answer command.
`content_version` identifies the definition being answered. The issued
`miss_revision` identifies the learning context when the attempt started.
Dataset identity prevents restored/replaced data from being mistaken for the
same current dataset. These are separate questions, even where the underlying
number wrapper is shared.

**Current state versus history.** Current catalog, learning and policy change as
the application acts. Historical attempts, assessments, receipts and content
versions retain their earlier values. A historical reply must not be checked as
if it were a freshly issued answer against current content.

## How the adapter stays readable

| Implementation step | File | Responsibility |
| --- | --- | --- |
| Stage a commit for both adapters | [transaction.rs](src/transaction.rs) | Check snapshot stamps, clone, apply and validate typed writes, then increment the revision. |
| Read and write a SQLite transaction | [sqlite_store.rs](src/sqlite_store.rs) | Open a consistent read or serialized write transaction and publish the validated batch once. |
| Define relational constraints | [schema.sql](src/sqlite_store/schema.sql) | Enforce identity and references in SQLite while preserving ordered occurrences. |
| Stage one requested write | [apply_write.rs](src/transaction/apply_write.rs) | Apply typed writes and enforce immediate identity/uniqueness/version constraints. |
| Validate the complete view | [validate_snapshot.rs](src/transaction/validate_snapshot.rs) | Check references and attempt/assessment presentation consistency, including history. |
| Validate newly added outcomes | [validate_outcomes.rs](src/transaction/validate_outcomes.rs) | Match new assessments to Learning and new replies to committed results without rewriting historical outcomes. |

These are private implementation helpers. Their behavior tests live beside the
public concept the caller needs to understand. [test_support.rs](src/test_support.rs)
only constructs records and batches; it never commits or hides the operation
being asserted. The full assembly scenario stays in `commit.rs` so the data flow
can be read in one place.

## Preserved persistence guarantees

- Failed batches publish no partial content, progress, assessment, learning or
  receipt changes. A global optimistic revision detects stale snapshots.
- Issue records use current content and miss context. Progress preserves issue
  facts and identity, advances its response revision once and keeps terminal
  assessments sticky.
- Terminal attempts have a matching human assessment. Active attempts have
  prompts; closed attempts do not. A terminal `Good` requires completion.
- New assessment keys agree with their source identities and commit with matching
  Learning state. The current contract allows at most one new assessment per
  target per transaction.
- New receipts match the committed attempt/assessment effect or preserve an exact
  previously committed result. Old receipts remain available after content edits
  and restore.
- Current Learning keys exactly match catalog targets. Semantic edits require a
  newer content version; ordinary import cannot silently delete content.
- Restore needs a never-used dataset ID, including when previous datasets had no
  content. Old attempts, receipts and version history remain available.

Application commands still own replay ordering, current eligibility, policy
compatibility, provider access and semantic learning resets. Store enforces
persistence consistency; it does not replace those decisions.

## Run the examples

From the assembled development workspace:

```sh
cargo test -p tempo-store
cargo test -p tempo-store commit::tests
cargo test -p tempo-store receipt::tests
cargo test -p tempo-store sqlite_store
cargo test -p tempo-store --doc
cargo doc -p tempo-store --no-deps --open
cargo clippy -p tempo-store --all-targets -- -D warnings
```

The SQLite scenarios check version-1 initialization, unsupported schema versions,
foreign keys, uniqueness, simultaneous writers, reopen, and rollback after a late
SQL failure. Schema migration and power-loss qualification remain future work.
