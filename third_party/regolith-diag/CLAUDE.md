# Development Principles

## 0. What Regolith Is

**regolith** (crate: `regolith`; the version is in `Cargo.toml`) is a pure Rust, embedded LSM-tree key-value store built from scratch. The architecture follows the LevelDB design (memtable → WAL → leveled SSTables → background compaction); the public API is shaped to slot into common embedded-KV abstraction layers so consuming applications can swap regolith in alongside other backends through the same trait.

Early-stage. The public API (`Db`, `Snapshot`, `WriteBatch`, `Options`) is small and stable-shaped, but breakage is allowed pre-1.0.

### Design goals

- **Pure Rust, no FFI**: no C/C++ toolchain, no `bindgen`, no linker surprises in the library or checked-in workspace tools
- **LSM-tree**: write-optimized with level-based background compaction
- **MVCC**: point-in-time consistent reads via global sequence numbers
- **Crash recovery**: WAL whose records carry the offset the last sync made durable, so replay tells a crash from damage
- **Concurrent reads**: no read takes a lock. A read loads the published view wait-free (`src/engine/read_view.rs`, a kovan `Atom`), walks the arena-backed skip list memtable (`src/engine/skiplist/`) and its append-only range-tombstone log, and uses a lock-free CLOCK block cache
- **No async runtime required**: compaction runs on background worker threads (`Options::max_background_compactions`), or inline on a target that has none

---

## 1. Information Hygiene

This codebase is designed for **AI-human pair programming**. Every structural choice optimizes for **rapid context acquisition**.

**Context clarity is oxygen for productive collaboration.**

## 2. Temporal Boundaries

| Zone        | Contains                | Lives in                        |
| ----------- | ----------------------- | ------------------------------- |
| **Past**    | How we got here         | Git history, closed issues/PRs  |
| **Present** | What the code does now  | Working tree                    |
| **Future**  | What we might do next   | GitHub issues                   |

**No commented-out code. No TODO comments (create issues instead). No speculative docs.**

## 3. No Documentation Files

Only allowed: `README.md`, `CLAUDE.md`, `Cargo.toml`, `LICENSE-*`.

No `ROADMAP.md`, `DEVELOPMENT.md`, `docs/` directories, or planning documents.

## 4. File Organization

**One concept per file. Small files over large files for new code.**

### Module Map

Workspace layout:

```text
src/                    # Publishable regolith library crate
tests/                  # Public-API integration, corruption, concurrency, and property tests
tests/auto_traits.rs    # Compile-time Send and Sync assertions for the public handles
tools/regolith-bench/       # Pure-Rust benchmark CLI
tools/regolith-stress/      # Pure-Rust stress CLI
tools/regolith-ycsb/        # Pure-Rust YCSB-style workload CLI
tools/regolith-wasm-probe/  # Pure-Rust wasm probe
fuzz/                   # cargo-fuzz harnesses, outside the normal workspace
```

Library modules:

```text
src/
├── lib.rs              # Public API: Db, Snapshot, WriteBatch, column families, re-exports
├── allocate.rs         # Conflict-free allocation of u64 ranges: Db::allocate
├── backup.rs           # BackupEngine: backups, metadata sealed through the KeyProvider (D57), restore
├── backup/             # format.rs: the .backup metadata, plain (version 3) and sealed (version 4)
├── checkpoint.rs       # Hardlinked checkpoint creation
├── column_family.rs    # Column-family handles, descriptors, and the lock-free registry
├── conflict.rs         # Conflict reasons: Conflict, Access, WriteKind
├── encryption.rs       # Encryption at rest: KeyProvider, KeyId, KeyMaterial
├── error.rs            # Error enum, Result alias
├── event_listener.rs   # Flush/compaction event callbacks
├── io_queue.rs         # Non-blocking calls: ReadMode, QueueId, IoBudget, IoProgress
├── io_queue/           # queue.rs (IoQueue: poll, idle_waker, block_on), wait.rs (IoWait, StallWait,
│                       # LockWait, WouldBlock), ticket.rs (CommitTicket, JobTicket), completion.rs (deliver once)
├── iter.rs             # Public iterator wrappers
├── log_layout.rs       # LogLayout: the key layout of a commit-ordered log
├── options.rs          # Options, tuning enums, MergeOperator, CompactionFilter
├── options/            # Options builder methods (builder.rs) and getters (getters.rs)
├── per_thread.rs       # A small number per thread (bounded pool, given back on exit) for per-core shards
├── perf_context.rs     # Per-operation performance counters
├── portability.rs      # Atomics shim and the portability tier map
├── rate_limiter.rs     # Token-bucket rate limiter
├── slice.rs            # DbSlice: zero-copy value handle
├── sst_file_writer.rs  # External SSTable writer API
├── statistics.rs       # Tickers and histograms, sharded per thread, summed when read
├── stream_writer.rs    # StreamingWriter: bounded-memory write stream
├── tailing.rs          # Tailing iterator API
├── testing.rs          # `testing` feature: property checks a caller runs against its own trait implementations
├── transaction.rs      # Optimistic and pessimistic transactions, isolation levels
├── transaction/        # policy.rs (KeyClassifier, key classes), txn_options.rs (TxnOptions),
│                       # scan_range.rs (scan stretches), write_buffer.rs (buffered puts, deletes, merges),
│                       # nowait.rs (commit_nowait), retry.rs (transact, transact_async, ready),
│                       # lock_table.rs (the lock-free pessimistic lock table, a wait kept across calls),
│                       # locking.rs (a transaction's lock calls: try, blocking with the timeout, release)
├── ttl.rs              # TTL database wrapper
├── sync/               # Public regolith::sync: async locks, semaphore, notify, event, latch, barrier, once cell
│   ├── queue.rs        # Wait queue and drain role every primitive builds on
│   ├── contend.rs      # The acquire every lock-like primitive shares: barging with bounded bypass (D49)
│   ├── owned_lock.rs   # A lock whose holder is named in its state word: the pessimistic key lock
│   ├── waiter.rs       # Waiter nodes, their free list, the wake list
│   └── internal.rs     # The engine's private std/loom atomics and Mutex, and how its threads wait on a
│                       # regolith::sync primitive (block_on, wait_here), releasing held-back views first
├── txn_buffer.rs       # Concurrent write and read-set buffer of one transaction
├── env/                # Env trait and backends: StdEnv (positional reads and log writes), MemEnv, WASI, OPFS;
│                       # LogFile, a log written at offsets (serial_log.rs over an append-only file); db_lock.rs
│   ├── mem_file.rs     # Lock-free in-memory file behind MemEnv and the OPFS mirror
│   ├── open_file_limit/ # max_open_files: lock-free slot table (slots.rs), rename-aside removal
│   ├── opfs/           # Browser OPFS backend: lock-free pool and mirror over sync access handles
│   └── persist_order.rs # The order OPFS mirror mode writes files back: tables, manifest, deletions (E20)
└── engine/
    ├── mod.rs          # RegolithEngine orchestration, read paths, rotation, recovery
    ├── commit/         # The lock-free commit pipeline: the ring of slots (ring.rs), what a slot holds
    │                   # (descriptor.rs), the stages any thread runs: decide, apply, write, publish
    │                   # (pipeline.rs), the round that syncs or rolls back the log (round.rs), ordered
    │                   # units, rotation and rollback (ordered.rs), whose reads a decision may make
    │                   # (read_mode.rs), commit_nowait after its call returned (resume.rs), transactions
    │                   # as slots (txn.rs), the check up to a horizon (early.rs), the stall as a unit
    │                   # writers wait on (stall.rs), the column-family fence (families.rs), stage hooks
    │                   # for tests (hook.rs)
    ├── flush.rs        # Writing a frozen memtable to L0, shared with the compaction workers
    ├── background_step.rs # Flushes off the commit path; owed work run as a job on the writer's queue
    ├── foreground.rs   # compact_range, ingest, checkpoint run as jobs behind a JobTicket; a job on a
    │                   # queue parks instead of waiting for the compaction gate
    ├── compaction_gate.rs # The compaction gate: passes share it, jobs hold it alone, fair both ways (E28)
    ├── stall_state.rs  # Write-stall thresholds and the level writers cache
    ├── compaction.rs   # Level/FIFO/universal compaction planning and worker loop
    ├── compaction/     # Per-snapshot-stripe folding of versions and merge chains; claims.rs (the tables
    │                   # passes are working on, claimed with atomics)
    ├── manifest.rs     # VersionSet, VersionEdit log, level tracking
    ├── version_store.rs # The version set behind its step; the current version is one load of the view
    ├── step.rs         # A step: sections run one at a time by whichever thread holds it, no mutex
    ├── manifest/       # Sealed manifests (sealed.rs), the judge of a torn tail (tail.rs, E29), tests
    ├── log_retirement.rs # Retiring flushed logs: min_wal_id in the table's batch, failed removals
    │                   # reported and retried (E30)
    ├── ingest.rs       # External-table ingest without a rewrite: per-file sequence (D48, E8)
    ├── open_transactions.rs # The open transactions close aborts
    ├── read_rule.rs    # How a commit judges one read against later commits
    ├── memtable.rs     # Arena-backed skip list memtable plus its range tombstones
    ├── memtable/       # Key walks, probe tests, and tombstones.rs: the append-only, lock-free range-tombstone log
    ├── skiplist/       # Insert-only skip list over the arena: any thread inserts, idempotent per entry
    ├── sstable.rs      # SSTable reader/writer, footer, index block
    ├── sstable/        # SSTable key walks, size limits, sealed tables (V7, V8)
    ├── wal.rs          # Appending write-ahead log (format 2) for recovery's rewrite, CLOSE, format 1 rules
    ├── wal_log.rs      # The log the pipeline writes: format 2 records written by position by any thread
    ├── take.rs         # A value put by one thread and taken by another, with no lock
    ├── wal_frame.rs    # WAL format 2: stamp, record frame, the scan past damage
    ├── wal_replay.rs   # Streaming reader over one WAL file: the O < P rule
    ├── wal_v1.rs       # Format 1 of the WAL, as 0.1.x wrote it, for replay
    ├── wal_seal.rs     # Sealed WAL: the stamp sealed and durable at creation, sealed records
    ├── seal.rs         # AES-256-GCM-SIV frames and the keyring over a KeyProvider
    ├── recovery.rs     # Replaying the WALs at open, the dropped-tail report, the rewrite
    ├── arena.rs        # Lock-free bump allocator for one memtable
    ├── block.rs        # Data blocks: prefix compression, restart points, varint
    ├── block/          # stamp.rs: an ingested table's blocks read at its one sequence (D48)
    ├── block_cache.rs  # Sharded lock-free CLOCK cache for decompressed SSTable blocks: pins, byte bound
    ├── block_cache/    # ring.rs: the lock-free CLOCK ring of slot words; tests.rs
    ├── callback.rs     # Catching a panic in caller code inside a commit or a background step
    ├── bloom.rs        # Bloom filter (double-hashed xxh3)
    ├── checksum.rs     # Checksum helpers
    ├── filter_block.rs # SSTable filter region: user-key and prefix bloom filters
    ├── index_block.rs  # Decoded SSTable index blocks
    ├── internal_key.rs # MVCC internal key encoding
    ├── io/             # Per-thread queues: unit and job tables, the close gate (mod.rs), single-flight
    │                   # read units (unit.rs) and jobs (job.rs, with parking) over one claim (flight.rs), per-queue
    │                   # inbox and landings (shared.rs), thread binding (bind.rs), the mode scope
    │                   # (scope.rs), stack.rs, atomic_waker.rs
    ├── loom_model/     # Loom models of the engine's lock-free protocols (--cfg loom)
    ├── lookup_key.rs   # Inline-first internal key used by every read path
    ├── iterator.rs     # Engine iterator merge logic
    ├── range_tombstone.rs # Range-delete tombstone encoding
    ├── read_view.rs    # The published set of memtables and version a reader loads, wait-free (kovan Atom)
    ├── reclaim.rs      # Prompt kovan reclamation for retired views and tombstone indexes
    ├── read_horizon.rs # Newest sequence whose data is durable and applied
    ├── snapshot_registry.rs # Live snapshot sequences on per-thread slots: announce, sample, confirm
    ├── snapshot_registry/   # chain.rs: a slot's chunks of counted entries; unit, model and property tests
    ├── source_walk.rs  # Newest-first walk over a view's sources for one key
    ├── background_health.rs # Whether flush or compaction is failing, and why
    ├── compaction_backoff.rs # Retry pacing for a failing compaction worker
    ├── disk_check.rs   # Warning when the filesystem is nearly full, run once after open
    ├── orphan_sweep.rs # Removal at a writable open of SSTables the manifest does not reference and
    │                   # of tables renamed aside on removal that a crash left
    └── pending_outputs.rs # Compaction outputs not yet offered to the manifest
```

### Public API surface

`lib.rs` is the public surface. Core types include `Db`, `Snapshot`,
`WriteBatch`, `Options`, `WriteOptions`, `Iter`, `TailingIter`, `Error`,
and `Result`. Extension surfaces such as column families, transactions,
TTL, backups, checkpoints, external SST ingestion, statistics, event
listeners, merge operators, compaction filters, and rate limiting are
also re-exported from `lib.rs`. `regolith::sync` is a public module of
its own: runtime-free async primitives (barging with bounded bypass,
cancellation-safe, no system call but kovan's `sched_yield`) plus
kovan's channels, map, queues and `Atom`. Anything not re-exported is
internal.

### File Size Guidelines

- Under 200 lines: Fine
- 200 to 400 lines: Check if doing one thing
- Over 400 lines: Consider splitting

Several core engine files are intentionally larger today because they
still carry early-stage API and storage-engine code together. Treat that
as refactoring debt: split them only behind focused issues or while
touching a cohesive area with tests, and keep new modules small.

## 5. Naming Conventions

| Thing         | Convention           | Example              |
| ------------- | -------------------- | -------------------- |
| Files/Modules | snake_case           | `block_cache.rs`     |
| Types         | PascalCase           | `WriteBatch`         |
| Functions     | snake_case           | `flush_memtable()`   |
| Constants     | SCREAMING_SNAKE_CASE | `BLOCK_FOOTER_SIZE`  |

## 6. Comments Policy

**Minimal comments. Code should be self-documenting.**

✅ Comment: Non-obvious WHY, safety invariants, on-disk format descriptions, public API docs (`///`)

❌ Don't: What the code does, TODO/FIXME, commented-out code, change history

## 7. Architecture at a Glance

**Write path:** `put`/`delete`/`write` → take a slot of the commit ring → the slot is decided in ring order (sequences, log position, conflict check), applied to the active memtable and its record written to the WAL at its offset, by whichever thread reaches it → published once written (and synced, at `Immediate`) → when the memtable is full, an ordered step swaps it to immutable and it is flushed to an L0 SSTable → background compaction merges L0→L1 and promotes levels (each 10× larger than the last).

**Read path:** active memtable → frozen memtables → L0 SSTables (bloom filter pre-check, overlap possible) → L1..L6 (sorted, non-overlapping, binary search). The first hit wins.

**MVCC:** every write increments a global sequence number. `snapshot()` captures the current seq; reads through a `Snapshot` ignore any key whose seq is greater. This is how point-in-time isolation works without locks on the read path.

**Durability:** at `DurabilityMode::Immediate` a write returns once an fsync of the WAL covers it, one fsync shared by every commit written by then; `Eventual` (default) leaves it to the OS.

## 8. Git Worktree Workflow

```bash
git worktree add ../regolith-foo -b feat/foo    # Work on feature foo
git worktree add ../regolith-bar -b feat/bar    # Work on feature bar
git worktree remove ../regolith-foo             # Clean up
```

Each worktree is isolated, no branch-switching overhead.

## 9. House Rules

The bare minimum for every change. A change that breaks one of them does not merge.

### The boundary

- regolith is a general-purpose engine. Every type, function, test and comment is named for the mechanism, never for a
  downstream use; the only product-like identifier is `DefraLevel`. No downstream feature lives here (CRDT rules,
  document lifecycles, head sets, arrival journals, indexes): regolith exposes traits and mechanisms, callers
  implement and call them.
- A DefraLevel behaviour is active only at `IsolationLevel::DefraLevel`. Engine fixes, `transact`, `Db::allocate` and
  the commit receipt serve every level and change no level's semantics; a test pins each other level as unchanged.
- Every method of a public trait is really called by the engine with the caller's own data. A method the engine never
  calls is a gap to fix, never a limit to document. Each public trait carries a runnable generic rustdoc example, and
  `testing` ships the property checks a caller runs against its own implementation.
- Nothing valid is rejected. regolith fails loud only about a state it cannot know, with an actionable error.
- No compatibility reasoning: break any API if it makes regolith faster or more correct, and never defer or shape work
  for a release, a semver rule or a freeze. Releases are cut by the maintainers when they choose.

### Threads, waiting and data locality

- No call blocks its caller: it completes, or returns `Pending` or `WouldBlock` at once. Completions go to the asking
  thread's own `IoQueue`; units (a block read, a group's sync) are single-flight and claimed with one CAS; a busy
  thread is never woken.
- No path takes a blocking OS lock or a condvar. What must exclude or wait uses `crate::sync`, which makes no system
  call (kovan's queues may yield; kovan is used as it is). A contended acquire may spin within its bounded budget, then
  suspends the task, never the thread.
- No thread beyond the compaction worker, which stays on by default where the platform has threads. regolith never
  requires a thread: without one, compaction runs inline and the caller's poll drives its own queue.
- Lock-free by default, wait-free where the structure allows it (loads, `fetch_add` reservations, helping in the
  commit pipeline).
- Thread-per-core friendly: hot state is per thread or per core (statistics shards, snapshot-registry slots, I/O
  queues), data stays local to the thread that owns it, and the read path loads the published view wait-free.
- regolith stays as free of dependencies as it can: no new dependency for what the standard library, kovan or a few
  lines of code already do. A map shared between threads is kovan's `HopscotchMap`.
- Every resource (bytes, blocks, file descriptors, threads, in-flight I/O) is charged to the engine's own accounting
  before it exists and released through the same seam.

### Performance

- 100% correct and fast; performance is the key. Reads are zero-copy; every source walk goes through the one shared
  newest-first walk; no allocation, syscall or lock on an uncontended fast path; a mechanism nobody uses costs nothing.
- A performance problem is researched before it is coded: what each tree does (file and line), a short profile,
  upstream sources and papers, then two to four designs with expected numbers and the rules they touch.
- A change that can move a number is measured interleaved against main on fixed CPUs, nothing else building, with
  medians, minimum, maximum, every sample and the perf counters that explain it, and gated on the scales that broke
  before (64 writers on 16 CPUs, 1 writer, Immediate durability). Keep the fastest variant that passes every check.

### Proofs

- No logic change without its models. A change to what the code does ships with its TLA+ model and its Lean proof
  in the same change, and both pass before the code is written or merged. That covers behavior, an ordering, a
  protocol, durability, recovery, conflict and validation rules, an encoding, and a data structure's invariants.
  A model that does not pass means the design is broken: it is not implemented until the design is fixed and TLC
  and Lean pass. A change that keeps behavior identical (a refactor, a speedup with identical results, tests,
  docs) still updates the correspondence table of every model whose code it moves.
- Every mechanism has a written contract (what regolith guarantees, what the caller must uphold) and a TLA+ model
  written before its code: the spec is the contract. Anything temporal or concurrent is modelled in TLA+; data
  structures and laws that hold for every size are proved in Lean. New concurrency on production types gets a loom
  model with a calibration that fails without the guard.
- Each TLA+ model has GREEN configurations and, for every guard and every caller contract, a RED configuration that
  breaks it and fails on its named invariant.
- A correspondence table maps every model action, variable and invariant to the Rust function and line that implements
  it; gaps are closed or explained. Transactional changes also run the real-code semantic checks: Elle, the
  serial-history properties, the validation-set oracle, the MVCC invariants and the power-loss suites.
- Bounded: one TLC configuration under 20 seconds of CPU on one worker, the whole `just tla` under 10 minutes; one loom
  model under 60 seconds, a loom recipe under 5 minutes, `just loom-all` under 10. A slow model is shrunk (its `.cfg`
  or comment says what it still covers), never given a longer limit or more shards. Never bound loom with
  `LOOM_MAX_DURATION`, which passes silently when time runs out.
- Lean: no `sorry`, `admit` or new `axiom`; the axiom audit allows only Lean's three standard axioms.
- Proofs read ELI5, line by line, for someone who knows nothing about proofs or model checkers:
  - each file opens with a plain-words story: the real thing it models, a tiny concrete example, what must never happen;
  - every line of TLA+ and Lean carries a plain-words comment; each variable names the Rust it mirrors; each action
    says when it happens in the program; each Lean tactic step says what it does to the goal;
  - each RED configuration says which bug it plants and how that bug breaks the promise;
  - each theorem and invariant opens with a one-sentence plain statement of what it promises.

  Proof files are the exception to the comments policy (section 6).

### Tests

- A failing test first for every defect, kept as its regression test.
- No test passes when the logic is wrong: break each mechanism once on purpose and see its named test fail.
- Every durability change gets power-loss tests under the fault shim, run with CI's compiler and on a real disk as well
  as tmpfs (where a sync finishes at once and timing bugs hide).
- No fixed sleep in a polling loop: a deadline with bounded backoff.
- Code that renames, removes or reopens files is correct on Windows, where an open file cannot be renamed or removed.

### Checks: build the way CI builds

- `RUSTFLAGS=-D warnings` on every build and test command, with CI's newest stable compiler as well as the default.
- fmt; clippy with `-D warnings`, also for `x86_64-pc-windows-msvc` and `aarch64-apple-darwin`; docs with
  `-D warnings`; the whole nextest profile; the fault-shim suites; doc tests; the MSRV check; the wasm and embedded
  jobs as their workflows run them; `cargo deny`; miri on the unsafe modules touched; `just tla`, `just lean` and
  `just loom-all` for what changed.
- Nothing runs forever: every long command runs under a timeout, and a check that hits its limit has failed.
- Heavy commands run at low priority on a bounded CPU range, one heavy stream at a time, leaving cores free for the
  person using the machine. Stop only your own processes, by pid.

### Code shape and git

- Trait bounds go in a `where` clause, on functions, impls, types and traits alike, never inline in the generic
  parameter list (`fn f<T>(t: T) where T: Trait`, not `fn f<T: Trait>(t: T)`).
- Callback shapes are named traits or `type` aliases, with lifetimes that have meaningful names.
- Every public item has rustdoc stating its contract: guarantees, preconditions, and what misuse breaks.
- One logical change per commit, a one-line conventional subject through `git commit -m`, no body, no AI attribution,
  never a change to git config, never a force push.
- No em-dash or en-dash characters anywhere.

## Build Dependencies

- **Rust** 1.99+ (edition 2024) for the library build (see `rust-version` in `Cargo.toml`); CI runs tests and tools on stable Rust.

That's it for the checked-in workspace. No `protoc`, no `cbindgen`, no C/C++ toolchain, no system libraries. Workspace tool dependencies must also parse and build on the MSRV toolchain because CI runs `cargo check --workspace` on Rust 1.99. Tools that need foreign libraries, bindgen-based comparison backends, or newer toolchains should live outside this workspace so the main CI path remains pure Rust.

## Common Commands

```bash
cargo test --workspace                         # Run library, integration, and tool tests
cargo clippy --workspace --all-targets -- -D warnings # Lint all workspace targets (matches CI)
cargo fmt --all -- --check                     # Format check (matches CI)
cargo fmt                                      # Apply formatting
cargo build --release                          # Optimized build (LTO, strip)
```

Use inline `#[cfg(test)]` tests for module-local invariants and `tests/`
for public-API integration, corruption, concurrency, parity, and property
tests. Keep fuzz harnesses under `fuzz/`; they are not part of the normal
workspace test run.

## Before Committing

1. `cargo test --workspace` passes
2. `cargo clippy --workspace --all-targets -- -D warnings` clean
3. `cargo fmt --all -- --check` clean
4. `cargo deny check` clean: surfaces RUSTSEC advisories, license violations, and duplicate crates against the allow-list in `deny.toml`

These are the local gates mirrored in CI (`.github/workflows/ci.yml`).
CI also builds docs with `cargo doc --workspace --no-deps`, checks the
library and tools on the MSRV toolchain with `cargo check --workspace`,
and runs scheduled ignored stress tests.

CI also publishes a coverage summary via `cargo llvm-cov --summary-only` on every push; run it locally with `cargo llvm-cov` (HTML report lands in `target/llvm-cov/html/`) when a change touches a file whose coverage you care about. No hard gate yet; the baseline at the time of writing is ~93% regions / ~91% lines.

## Goal

**A new contributor should be able to read this file, skim `src/lib.rs`, and start making productive changes to the engine within an hour.**

Fast context acquisition → Confident changes → Productive iteration.
