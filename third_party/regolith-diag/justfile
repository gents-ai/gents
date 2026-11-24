

default:
    @just --list

# Everything CI enforces on every push, in the order it fails fastest.
gate: fmt perf-tools lint doc test test-testing deny

perf-tools:
    python3 -m unittest discover -s tools/perf-report -p 'test_*.py' -v

fmt:
    cargo fmt --all -- --check

fmt-fix:
    cargo fmt --all

# The second pass of `lint`, `doc` and `msrv` turns on `testing`: the
# feature adds `regolith::testing`, which no default build contains, so
# the default pass never sees it.

lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo clippy -p regolith --all-targets --features testing -- -D warnings

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    RUSTDOCFLAGS="-D warnings" cargo doc -p regolith --no-deps --features testing

# nextest, not `cargo test`: each test gets its own process, so a
# wedged test fails on the profile's slow-timeout instead of holding the
# whole binary. See `.config/nextest.toml`.
#
# nextest cannot run doc tests, so they run under a hard limit: measured at
# 12 s with TMPDIR on an nvme disk at load 310 on 36 threads (3 s on tmpfs
# at load 88), times 25.

test:
    cargo nextest run --workspace
    just bounded 5m test cargo test --workspace --doc

# A run under a hard limit, for what nextest cannot bound (doc tests, wasm
# targets, a workload its caller sizes): one that reaches the limit fails and
# says which recipe hit which limit. Needs GNU `timeout` from coreutils
# (`gtimeout` where it is installed under that name). Runs where it is
# called from, because the Elle harness calls it from its own directory.
[no-cd]
[private]
bounded limit recipe +command:
    @t=$(command -v timeout || command -v gtimeout) && "$t" --version >/dev/null 2>&1 \
        || { echo "error: just {{recipe}} needs GNU timeout (coreutils) to bound its run" >&2; exit 1; }; \
    "$t" {{limit}} {{command}}; code=$?; \
    [ "$code" -ne 124 ] || echo "error: just {{recipe}} did not finish within {{limit}} and was stopped" >&2; \
    exit "$code"

# `regolith::testing` under its feature: the contract checks' own tests,
# then every doc test again, which adds the ones that call the checks.

test-testing:
    cargo nextest run -p regolith --features testing --lib testing::
    just bounded 5m test-testing cargo test -p regolith --features testing --doc

deny:
    cargo deny check

msrv:
    cargo "+$(grep -m1 '^rust-version' Cargo.toml | cut -d'"' -f2)" check --workspace
    cargo "+$(grep -m1 '^rust-version' Cargo.toml | cut -d'"' -f2)" check -p regolith --features testing

# The summary line CI annotates a build with.
# Through nextest, like the gate: one process per test, so a wedged
# test fails on the profile's slow-timeout instead of holding the whole
# instrumented run open until the job's ceiling.
cov-summary:
    # The `ci` profile, not the default: instrumented code runs several
    # times slower, and the default profile's 60s slow-timeout turns the
    # heavier soaks into timeouts that say nothing about the code.
    cargo llvm-cov nextest --summary-only --workspace --profile ci --ignore-filename-regex '(^|/)tools/'

# The browsable HTML report, for reading locally. Through nextest and its
# `ci` profile, for the reasons `cov-summary` gives.
cov:
    cargo llvm-cov nextest --html --workspace --profile ci --ignore-filename-regex '(^|/)tools/'
    @echo "report: target/llvm-cov/html/index.html"

# ── suites CI runs as their own jobs, split by mechanism ─────
#
# All through nextest, for the profile's per-test bound. The five that the
# durability job runs (`test-crash`, `test-power`, `test-fault`,
# `test-lifecycle`, `mvcc`) take no retry: a durability failure that passes
# the second time is still a failure.

# The fault-injection harness's own tests: the LD_PRELOAD shim, kill points,
# the power-cut simulation and the prefix validator every durability suite
# rests on. Linux only.

test-fault:
    cargo nextest run --test fault_smoke --retries 0

# Process-crash recovery: kill -9 at every point in the write path, each in
# a child process the harness kills at a byte-exact point. Linux only.

test-crash:
    cargo nextest run --test crash_recovery --retries 0

# Power loss: a child killed at a byte-exact point, then every byte it never
# synced discarded. One test at a time with its output shown, so the
# measured cost of the default DurabilityMode::Eventual is visible.

test-power:
    cargo nextest run --test power_loss --retries 0 --no-capture -- --skip crash_child

# Corruption sweeps, the sampled ones and the exhaustive ones: truncation at
# every byte offset and every single-bit flip of a WAL, an SSTable and a
# MANIFEST.

test-corruption-slow:
    cargo nextest run --test corruption_exhaustive

# Durability property tests: random operation sequences checked against a
# model after every operation, and across a kill and a power cut. Linux only.

test-durability-slow:
    cargo nextest run --test proptest_durability

# Resource exhaustion and extremes: six-figure operation counts, values and
# keys at the size maxima, megabyte keys, a tree deep enough that a read
# walks past L3, and a real ENOSPC on a tmpfs in a private user namespace.
# `--test-threads 1` is required, not cosmetic: each test resets the
# kernel's peak-RSS counter to report its own memory high-water mark, and a
# concurrent test would inflate that number.

test-extremes:
    cargo nextest run --test resource_limits --no-capture --test-threads 1

# Handle lifecycle: open, close, reopen under changed Options, locking,
# and every misuse of a closed or read-only handle.

test-lifecycle:
    cargo nextest run --test lifecycle --retries 0 -- --skip crash_child

# The whole workspace in a release build, one test at a time with its
# output shown: what the nightly job runs. It runs no `#[ignore]`d test.

test-slow:
    cargo nextest run --workspace --release --no-capture

# Rebuild the LD_PRELOAD fault shim from scratch by dropping its cache.

fault-shim-clean:
    rm -rf target/tmp/regolith-fault

# MVCC and concurrency invariants: snapshot stability, WriteBatch atomicity
# seen by concurrent readers, monotonic reads, version integrity across
# delete/compact/reopen, and iterators pinned across compactions. The fast
# tests and their full-scale versions both run: none is `#[ignore]`d.

mvcc:
    cargo nextest run --test mvcc_invariants --retries 0 -- --skip crash_child

# The same file, one test at a time with its output shown, under the
# profile's retry setting.

mvcc-slow:
    cargo nextest run --test mvcc_invariants --no-capture

set shell := ["bash", "-uc"]

gains_dir := justfile_directory() / "../regolithgains"

label     := env_var_or_default("LABEL", "wip")

py        := env_var_or_default("REGOLITHGAINS_PY", "python3")

# NOTE: the commit sha is resolved INSIDE the recipe, never as a top-level
# `sha := `git ...`` assignment. just evaluates those eagerly at parse time, so a
# top-level backtick makes every `just --list` fail outside a git checkout.

# ---------- the gate ----------

fuzz target time="300":
    cargo +nightly fuzz run {{target}} -- -max_total_time={{time}}

# ---------- benchmark gating ----------

# Exit 1 when the host is too busy to trust a measurement. A dependency, not advice.

loadguard:
    {{py}} {{gains_dir}}/loadguard.py

# ---------- benchmarks ----------

# Capture Criterion's named baseline. Custom harnesses emit family records;
# compare their collected run files with tools/perf-report/compare.py.

bench-baseline: loadguard
    cargo bench --bench point_read --bench write_durable --bench write_buffered \
                --bench scan --bench batch --bench transaction --bench scan_commit \
                --bench append --bench read_rules --bench txn_callbacks --bench sync \
                --bench large_value -- --save-baseline pre

# Compare Criterion against `pre`; custom harnesses need paired run files.

bench: loadguard
    cargo bench -- --baseline pre

# The sweep the CI perf artifact is built from: every family, into the
# JSON Lines file `collect` assembles a run file out of.
bench-collect: loadguard
    cargo bench --bench point_read --bench write_durable --bench write_buffered \
                --bench scan --bench batch --bench transaction --bench scan_commit \
                --bench append --bench read_rules --bench txn_callbacks --bench sync \
                --bench large_value --bench memory --bench size

bench-one name: loadguard
    cargo bench --bench {{name}} -- --baseline pre

# RSS soak. Default 360s; pass seconds and an Options variant tag.

soak secs="360" wb="64" cache="64" shard_bits="6" tag="default": loadguard
    cargo bench --bench soak -- {{secs}} {{wb}} {{cache}} {{shard_bits}} {{tag}}

# The two soak variants compared. Deterministic: no loadguard needed.

soak-pair:
    just soak 360 64 64 6 default
    just soak 360 64 64 0 cache-budgeted

# Binary size, native and both wasm targets, against the budget.

size:
    cargo bench --bench size

# The memory table in the README: every profile, on both hosts, one
# workload. The wasm column is the reproducible one because linear
# memory only ever grows; RSS moves between runs.

wasm-budget puts="20000":
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release --example embedded_profile --target wasm32-wasip1
    for profile in embedded wasm default; do
        echo "== $profile, x86_64 Linux =="
        d=$(mktemp -d)
        cargo run --release --quiet --example embedded_profile -- "$d" "$profile" {{puts}}
        rm -rf "$d"
        echo "== $profile, wasm32-wasip1 under wasmtime =="
        d=$(mktemp -d)
        wasmtime run --dir="$d::/data" \
            target/wasm32-wasip1/release/examples/embedded_profile.wasm \
            /data "$profile" {{puts}}
        rm -rf "$d"
    done

# Point memory probes.

mem:
    cargo bench --bench memory

# The MVCC regression probes. Must stay at zero violations.

ycsb workload="a" records="1000000" ops="1000000": loadguard
    cargo run --release -p regolith-ycsb -- \
        --workload {{workload}} --records {{records}} --operations {{ops}}

ycsb-all: loadguard
    for w in a b c d e f; do just ycsb $w; done

stress secs="600":
    cargo run --release -p regolith-stress -- --duration {{secs}}

# ---------- model checking ----------

# Loom model checks for the arena memtable, read horizon and version
# handoffs. `--cfg loom` swaps the primitives in `src/engine/sync.rs`
# for loom's instrumented ones; without it the whole target compiles
# away, so an ordinary `cargo test` neither builds loom nor runs these.
#
# | recipe        | models | profile | measured |
# |---------------|--------|---------|----------|
# | `loom`        | 16     | release | 21.6s    |
# | `loom-debug`  | 17     | debug   | 16.0s    |
# | `loom-all`    | both   | both    | ~38s     |
#
# Re-measured on a host at load average 105-140 on 36 threads (a shared,
# heavily loaded machine), so these are not comparable to a quiet-host
# baseline; they are the wall time `cargo test`'s own summary line
# reported for the test binary, excluding compilation.
#
# Both profiles run the same nineteen models: the memtable takes writes
# from any thread, so there is no single-writer guard left for a debug
# build to trip. Seven of the models are `should_panic` calibrations that
# deliberately get the ordering wrong; they are what make the passes mean
# anything.

# Loom is bounded. Every recipe below builds its models first and then runs
# them under a hard limit of 10 minutes (15 for the nightly twins): a run
# that hits it fails. A model is sized to finish in under a minute and a
# recipe in under five, by its scenario and by an explicit preemption bound
# in its builder, never by `LOOM_MAX_DURATION`, which stops the search and
# lets the test pass. A model that is shrunk says so in its comment.
loom:
    RUSTFLAGS="--cfg loom" cargo test --release --test loom_memtable --no-run
    RUSTFLAGS="--cfg loom" timeout 10m cargo test --release --test loom_memtable

loom-debug:
    RUSTFLAGS="--cfg loom" cargo test --test loom_memtable --no-run
    RUSTFLAGS="--cfg loom" timeout 10m cargo test --test loom_memtable

# Loom models for `regolith::sync`: every primitive's wait protocol,
# with seven calibrations that must fail. Release only: the larger models
# explore 0.1 to 0.4 million interleavings each (about four minutes in
# all), which a debug build multiplies several times over.
loom-sync:
    RUSTFLAGS="--cfg loom" cargo test --release --test loom_sync --no-run
    RUSTFLAGS="--cfg loom" timeout 10m cargo test --release --test loom_sync

# The five sync models whose three threads all contend for one lock or
# semaphore, at preemption bound 2 instead of the bound 1 `loom-sync`
# runs them at: their `#[ignore]`d twins, or the one `model` names. Each
# twin runs a smaller scenario of its own, because the shared one does not
# finish at bound 2 in any useful time (43 to 79 minutes a model, and never
# for the upgradable read). They take 2 s to 6 minutes each, so the nightly
# CI run carries them, one job per model.
loom-sync-deep model="":
    RUSTFLAGS="--cfg loom" cargo test --release --test loom_sync --no-run
    RUSTFLAGS="--cfg loom" timeout 15m cargo test --release --test loom_sync -- --ignored {{model}}

# Loom models for the per-thread I/O queues (D53): the unit claim, the
# completion pushed to every waiting queue, the idle waker, a read's wait,
# and close meeting a miss under way, with five calibrations that must
# fail. Release only, like
# `loom-sync`: the wait model explores close to 0.8 million interleavings.
loom-io:
    RUSTFLAGS="--cfg loom" cargo test --release --test loom_io_queue --no-run
    RUSTFLAGS="--cfg loom" timeout 10m cargo test --release --test loom_io_queue

# Loom models for the snapshot registry's per-thread slots: the slot and
# thread-number claims, a moved pin's release, announce-sample-confirm and
# the drain wake, with four calibrations that must fail. Release only.
loom-snapshots:
    RUSTFLAGS="--cfg loom" cargo test --release --test loom_snapshots --no-run
    RUSTFLAGS="--cfg loom" timeout 10m cargo test --release --test loom_snapshots

# Loom models for the per-core read path (Phase 7b): the read view's
# compare-and-swap publication, its reclamation and its freshness; the
# block cache's pin check, racing hands and byte bound; and the range-
# tombstone log's prefix publication, run against the real log. Each has
# a calibration that must fail.
loom-view:
    RUSTFLAGS="--cfg loom" cargo test --release --test loom_read_view --no-run
    RUSTFLAGS="--cfg loom" timeout 10m cargo test --release --test loom_read_view

loom-cache:
    RUSTFLAGS="--cfg loom" cargo test --release --test loom_block_cache --no-run
    RUSTFLAGS="--cfg loom" timeout 10m cargo test --release --test loom_block_cache

loom-tombstones:
    RUSTFLAGS="--cfg loom" cargo test --release --test loom_range_tombstones --no-run
    RUSTFLAGS="--cfg loom" timeout 10m cargo test --release --test loom_range_tombstones

# Loom models for the open-file slot table (acquire, evict, release, drain,
# and D60's park of a reopen that may not wait and the wake of a freed slot)
# and the column-family registry (create, drop and use racing in the
# ordered step), with five calibrations that must fail. Release, like
# `loom-io`.
loom-tables:
    RUSTFLAGS="--cfg loom" cargo test --release --test loom_tables --no-run
    RUSTFLAGS="--cfg loom" timeout 10m cargo test --release --test loom_tables

# Loom models for the lock-free commit pipeline (plan 4.7) and the version
# store's step: reserve and void, a cell's next lap, the pin, deciding with
# helping, the watermark claim, publication, a rollback meeting a decision
# under way, the step's sections, and parking (a queue's poll never waits:
# a job its holder parks, a job parked for the step, a rollback parked for
# a write in flight, a sync parked for a record's late copy in flight, the
# wake of a parked job against its landing and against the claim of the
# thread that parks it), with fifteen calibrations that must fail. Thirty
# models, each bounded by a ring of two cells, two or three threads and an
# explicit preemption bound; measured at 1.1s in all, the largest at 4,214
# interleavings. The limit is a hard one: a run that reaches it has failed.
loom-pipeline:
    RUSTFLAGS="--cfg loom" cargo test --release --test loom_pipeline --no-run
    RUSTFLAGS="--cfg loom" timeout 5m cargo test --release --test loom_pipeline

# Loom models for the pessimistic lock table (plan 4.6, 4.10): one holder
# per key, the bypass bound, a wait delivered to its own queue, the table's
# use count, and close meeting a wait being queued; and for the first four
# abort callbacks a transaction's claim holds in place, pushed while another
# thread takes them (at a bound that explores every schedule). Six
# calibrations must fail. Release only; the thirteen run in under ten
# seconds.
loom-locks:
    RUSTFLAGS="--cfg loom" cargo test --release --test loom_locks --no-run
    RUSTFLAGS="--cfg loom" timeout 10m cargo test --release --test loom_locks

# Loom models for the compaction gate (E28) and for a job that parks on its
# queue instead of waiting for the gate, with three calibrations that must
# fail: a job that skips the gate, the old writer-preferring gate, and a
# parked job with no way back to its queue. Release only, like `loom-sync`.
loom-gate:
    RUSTFLAGS="--cfg loom" cargo test --release --test loom_gate --no-run
    RUSTFLAGS="--cfg loom" timeout 10m cargo test --release --test loom_gate

# Loom models for the slots close finds open transactions in (claim_slots.rs):
# every reference dropped once by its owner or close, a claim listed before
# close began found, no ABA on the free stack, and a pushed slot's link
# seen by its taker, driving the production table; six calibrations must
# fail. Measured at 2.4s in all, the largest at about 35,000 interleavings.
loom-claims:
    RUSTFLAGS="--cfg loom" cargo test --release --test loom_claim_slots --no-run
    RUSTFLAGS="--cfg loom" timeout 5m cargo test --release --test loom_claim_slots

# Loom models for an in-memory file's extents (mem_file.rs, extent.rs),
# transcribed because kovan's Atom and the raw byte copies are invisible to
# loom: a read racing growth in place, two appends, a copy walking a growing
# chain, an extension read as zeros, a read racing a replacement; five
# calibrations must fail. Measured at 0.5s in all.
loom-mem-file:
    RUSTFLAGS="--cfg loom" cargo test --release --test loom_mem_file --no-run
    RUSTFLAGS="--cfg loom" timeout 5m cargo test --release --test loom_mem_file

loom-all: loom loom-debug loom-sync loom-io loom-snapshots loom-view loom-cache loom-tombstones loom-tables loom-locks loom-gate loom-pipeline loom-claims loom-mem-file

# The read-view chaos workload at a larger size than `cargo test` runs it:
# 4 instances x 2 rounds x 120 versions by default, against 2 x 1 x 50.
# Release, because debug is where the cost is.
#
# Sized to finish, not to be maximal. Cost is roughly
# instances x rounds x versions x compaction passes, and the compaction
# passes each rewrite a database that grows with the version count, so
# raising `versions` raises the run time faster than linearly: 400 does
# not complete inside seven minutes, 120 completes in seconds. What the
# workload is hunting is overlap between a compaction and a read, and
# the overlap count is already in the thousands per instance here.
#
# Built first, then run under `limit`, which is a parameter because the
# size is: the default size measured at 46 s with TMPDIR on an nvme disk at
# load 209 on 36 threads (under a second on tmpfs at load 68), times 13. A
# larger size passes its own limit.

chaos instances="4" rounds="2" versions="120" min_rounds="20" limit="10m":
    cargo test --release --test read_view_chaos_workload --no-run
    REGOLITH_CHAOS_INSTANCES={{instances}} REGOLITH_CHAOS_ROUNDS={{rounds}} \
    REGOLITH_CHAOS_VERSIONS={{versions}} REGOLITH_CHAOS_MIN_ROUNDS={{min_rounds}} \
        just bounded {{limit}} chaos cargo test --release --test read_view_chaos_workload -- --nocapture

# TLA+ model of what commit validation covers at each isolation level,
# checked against the Merkle-DAG CRDT workload `RepeatableRead` exists for: appends to a
# Merkle DAG whose head set is derived from keys, the sweep that reclaims
# it, and a write derived from a definition read; and against DefraLevel's
# relaxations: merges into a counter, scans that stay inside or leave a
# commutative prefix, and content-addressed keys.
# `proofs/tla/RepeatableRead.tla` carries the model; each configuration below
# runs with the verdict it must produce, and a RED row names the invariant it
# must break. A GREEN model that reports an error, or a
# RED one that does not break exactly its invariant, fails the recipe: the RED
# rows are what make the GREEN ones mean anything, and a RED row that fails
# for some other reason shows nothing.
# The storage-engine models follow, each backed by a Lean proof in
# `proofs/lean` that holds for every size where TLC checks a few:
# `LsmOrder.tla` (E1, compact_range picks a closed L0 input set),
# `BatchRead.tla` (E6, one view per batch read), `IngestPublication.tla`
# (E8, the visible sequence never passes a pending slot) and
# `WalRotation.tla` (E2, a rotation syncs the sealed log first).
# Each row prints the distinct states TLC explored and its wall time.
# TLC is fetched on first use, pinned by checksum in `proofs/tla/tools/tlc`.
#
# The suite is bounded. Every row runs on ONE TLC worker under a hard limit
# of 120 seconds: a row that hits it is reported as TIMED OUT and fails the
# recipe, it never passes. A configuration is sized to finish in under 60
# seconds on one worker; one that does not is shrunk (its `.cfg` says what
# was shrunk and what the smaller instance still covers), because the Lean
# proof covers every size and TLC only has to show a small instance's
# interleavings. Time a new row before adding it.
#
# As many rows run side by side as the machine has cores, each in a JVM
# sized for its one worker, and their lines are printed in the order below
# once all are done. With one worker a RED row stops at the shallowest
# broken invariant, the same on every run.
# Check every TLA+ row.
tla:
    #!/usr/bin/env bash
    set -uo pipefail
    cd proofs/tla
    # Fetched once, before the rows run side by side.
    ./tools/tlc --fetch-only >/dev/null || exit 1
    results=$(mktemp -d)
    trap 'rm -rf "$results"' EXIT
    # The cores this run may use (`nproc` counts those it is confined to).
    side=$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN)
    row=0
    # The module the next rows check; each block below sets it.
    spec=RepeatableRead
    # One row, started in the background: its line goes to a file named by
    # its number, and a `.fail` file beside it marks a wrong verdict.
    check() {
        row=$((row + 1))
        while [ "$(jobs -rp | wc -l)" -ge "$side" ]; do wait -n; done
        run_row "$(printf '%04d' "$row")" "$spec" "$@" > "$results/$(printf '%04d' "$row").out" 2>&1 &
    }
    run_row() {
        local n="$1" spec="$2" cfg="$3" expect="$4" inv="${5:-}" out code broke verdict states took
        # A serial collector and two compiler threads: what one worker on
        # one core uses. A temporary directory of its own, because TLC
        # unpacks its standard modules into it and two rows must not share
        # the files; the row's states go there too.
        mkdir -p "$results/tmp.$n"
        out=$(TLC_JVM_FLAGS="-XX:+UseSerialGC -XX:CICompilerCount=2 -Djava.io.tmpdir=$results/tmp.$n" \
              timeout 120 ./tools/tlc -workers 1 -metadir "$results/tmp.$n/states" -config "$cfg.cfg" "$spec.tla" 2>&1)
        code=$?
        # TLC words a broken invariant two ways: "is violated." when the search reaches a bad
        # state, and "is violated by the initial state:" when the very first state is already
        # bad. Both name the invariant, and both count.
        broke=$(sed -n -e 's/^Error: Invariant \(.*\) is violated\.$/\1/p' \
                       -e 's/^Error: Invariant \(.*\) is violated by the initial state:$/\1/p' <<<"$out" | head -1)
        # A broken promise about whole runs (a PROPERTY) is reported without its name. A row that
        # plants a bug against one names exactly one PROPERTY in its configuration, so that is the
        # one that broke.
        if [ -z "$broke" ] && grep -q '^Error: Temporal properties were violated\.$' <<<"$out"; then
            broke=$(sed -n 's/^PROPERTY \(.*\)$/\1/p' "$cfg.cfg" | head -1)
        fi
        states=$(sed -n 's/^.* states generated, \([0-9,]*\) distinct states found.*$/\1/p' <<<"$out" | tail -1)
        took=$(sed -n 's/^Finished in \(.*\) at (.*$/\1/p' <<<"$out" | tail -1)
        if [ "$code" -eq 124 ]; then
            # The hard limit: a failure, whatever the row was meant to show.
            verdict="TIMED OUT"
            took="over 120s"
        elif grep -q "No error has been found" <<<"$out"; then
            verdict=GREEN
        elif [ -n "$broke" ] && [ "$broke" = "$inv" ]; then
            verdict=RED
        elif [ -n "$broke" ]; then
            verdict="RED on $broke"
        else
            verdict=BROKEN
            echo "$out" | tail -20
        fi
        printf '  %-48s %-9s %9s states %9s  (expected %s)\n' \
            "$cfg" "$verdict" "${states:-?}" "${took:-?}" "$expect${inv:+ on $inv}"
        if [ "$verdict" != "$expect" ]; then touch "$results/$n.fail"; fi
    }
    check MC_RepeatableRead_Green                      GREEN
    check MC_RepeatableRead_Green_Live                 GREEN
    check MC_RepeatableRead_Red_Serializable           RED INV_AppendsCommit
    check MC_RepeatableRead_Red_SnapshotIsolation      RED INV_NoStaleDefinition
    check MC_RepeatableRead_Red_ReadCommitted          RED INV_NoStaleDefinition
    # Same-commit operand folding; Lean: Regolith/BatchMergeFold.lean.
    spec=BatchMergeFold
    check MC_BatchMergeFold_Green                     GREEN
    check MC_BatchMergeFold_Green_Size                GREEN
    check MC_BatchMergeFold_Red_Order                 RED INV_Reads
    check MC_BatchMergeFold_Red_Declined              RED INV_Reads
    check MC_BatchMergeFold_Red_Boundary              RED INV_Reads
    check MC_BatchMergeFold_Red_Size                  RED INV_Size
    check MC_BatchMergeFold_Red_Parts                 RED INV_Parts
    spec=RepeatableRead
    check MC_DefraLevel_Green_Heads                    GREEN
    check MC_DefraLevel_Green_Counters                 GREEN
    check MC_DefraLevel_Green_Counters_Clear           GREEN
    check MC_DefraLevel_Green_Migrations               GREEN
    check MC_DefraLevel_Red_RepeatableRead_Merges      RED INV_DuplicateMergesCommit
    check MC_DefraLevel_Red_NoPolicy                   RED INV_DuplicateMergesCommit
    check MC_DefraLevel_Red_BlocksOrdinary             RED INV_DuplicateMergesCommit
    check MC_DefraLevel_Red_RepeatableRead_Counters    RED INV_IncrementsCommitUnlessReplaced
    check MC_DefraLevel_Red_ReadMergeBlind             RED INV_ReceiptsExact
    check MC_DefraLevel_Red_PutMergeBlind              RED INV_CounterExact
    check MC_DefraLevel_Red_PutMergeElides             RED INV_CounterExact
    check MC_DefraLevel_Red_MergeIgnoresReplacement    RED INV_CounterExact
    check MC_DefraLevel_Red_RangeDeleteNotReplacement  RED INV_CounterExact
    check MC_DefraLevel_Red_PolicyIgnoresRange         RED INV_NoStaleDefinition
    check MC_DefraLevel_Red_DefinitionContentAddressed RED INV_NoStaleDefinition
    # DefraLevel's key classes and mechanisms (plan 3.1 to 3.15). Lean:
    # Regolith/Validation.lean, all_classes_serial; Regolith/Relaxations.lean;
    # Regolith/MergeOperator.lean.
    check MC_DefraLevel_Green_ContentAddressed         GREEN
    check MC_DefraLevel_Red_CaDeleteExempt             RED INV_NoDanglingReference
    check MC_DefraLevel_Red_PresenceReadFull           RED INV_LinksCommit
    check MC_DefraLevel_Red_PresenceUnchecked          RED INV_HoldsCurrent
    check MC_DefraLevel_Green_Parts                    GREEN
    check MC_DefraLevel_Green_Parts_Rewriter           GREEN
    check MC_DefraLevel_Red_PartsIgnorePut             RED INV_PartsCurrent
    check MC_DefraLevel_Red_PartsIgnoreTouch           RED INV_PartsCurrent
    check MC_DefraLevel_Red_PartsNoAbsentFallback      RED INV_PartsCurrent
    check MC_DefraLevel_Red_PartsNoPutFallback         RED INV_PartsCurrent
    check MC_DefraLevel_Red_PartsNamesOther            RED INV_PartsCurrent
    check MC_DefraLevel_Red_Parts_RepeatableRead       RED INV_PartsRelaxed
    check MC_DefraLevel_Green_ValueReads               GREEN
    check MC_DefraLevel_Red_SeqOnlyValidation          RED INV_IdenticalRewritesCommit
    # E50, a commit that writes nothing at every level: no pipeline slot, no
    # sequence, its level's checks against every decided commit and at the
    # published point, the receipt its snapshot, a latched database commits
    # it. Lean: Regolith/Validation.lean, writeFree_at_horizon;
    # Regolith/Relaxations.lean, read_only_anomaly.
    check MC_ReadCommitted_Green_WriteFree             GREEN
    check MC_SnapshotIsolation_Green_WriteFree         GREEN
    check MC_RepeatableRead_Green_WriteFree            GREEN
    check MC_Serializable_Green_WriteFree              GREEN
    check MC_DefraLevel_Green_WriteFree                GREEN
    check MC_DefraLevel_Red_WriteFreePessimistic       RED INV_WriteFreeConsistent
    check MC_RepeatableRead_Red_WriteFreeUnchecked     RED INV_WriteFreeAtHorizon
    check MC_SnapshotIsolation_Red_WriteFreeUnchecked  RED INV_WriteFreeAtHorizon
    check MC_SnapshotIsolation_Red_WriteFreeChecksPlainReads RED INV_WriteFreeCommits
    check MC_ReadCommitted_Red_ReadCommittedChecksForUpdate RED INV_WriteFreeCommits
    check MC_DefraLevel_Red_ValueAboveHorizonRefuses   RED INV_WriteFreeIdenticalCommits
    check MC_SnapshotIsolation_Red_WriteFreeRefusedWhenLatched RED INV_LatchRefusesOnlyWrites
    check MC_SnapshotIsolation_Red_WriteFreeTakesSequence RED INV_WriteFreeTakesNoSequence
    check MC_SnapshotIsolation_Red_ReceiptAtHorizon    RED INV_WriteFreeReceipt
    # A value read judged where it is placed: changed at the published point,
    # restored by a commit decided and not yet visible, it is refused.
    check MC_DefraLevel_Green_WriteFreeRestore         GREEN
    check MC_DefraLevel_Red_JudgedAtDecidedOnly        RED INV_WriteFreeAtHorizon
    # Write-free scans: a validated range is checked at every level but under
    # DefraLevel's relaxation, a plain scan's keys at Serializable.
    check MC_Serializable_Green_WriteFreeScan          GREEN
    check MC_RepeatableRead_Green_WriteFreeScan        GREEN
    check MC_DefraLevel_Green_WriteFreeScan            GREEN
    check MC_RepeatableRead_Red_WriteFreeIgnoresRanges RED INV_ScanDecisionsCurrent
    check MC_Serializable_Red_WriteFreeScanUnchecked   RED INV_WriteFreeAtHorizon
    # The read-only anomaly (Fekete, O'Neil and O'Neil 2004): a report checked
    # against every decided commit is refused once the withdrawal is decided;
    # checked against the published state alone, or not at all, it commits a
    # sheet no serial order gives, as one at SnapshotIsolation does by design.
    check MC_Serializable_Green_ReadOnlyAnomaly        GREEN
    check MC_RepeatableRead_Green_ReadOnlyAnomaly      GREEN
    check MC_Serializable_Green_ReadOnlyAnomalyPessimistic GREEN
    check MC_DefraLevel_Green_ReadOnlyAnomalyPessimistic GREEN
    check MC_Serializable_Red_ReadOnlyAnomaly          RED INV_NoReadOnlyAnomaly
    check MC_Serializable_Red_ReadOnlyAnomalyPublishedOnly RED INV_NoReadOnlyAnomaly
    check MC_SnapshotIsolation_Red_ReadOnlyAnomaly     RED INV_NoReadOnlyAnomaly
    check MC_DefraLevel_Green_Log                      GREEN
    check MC_DefraLevel_Red_OwnAppendVisible           RED INV_NoPhantomAppend
    check MC_DefraLevel_Red_DecideOnLogRead            RED INV_LogDecisionsCurrent
    check MC_DefraLevel_Green_MergeBeforeScan          GREEN
    check MC_DefraLevel_Red_MergeBeforeScanReadsBase   RED INV_TallyMergesCommit
    check MC_DefraLevel_Green_OwnWrites                GREEN
    check MC_DefraLevel_Red_PutsBeforeMerges           RED INV_ReadYourOwnWrites
    check MC_DefraLevel_Red_OwnMergesInvisible         RED INV_ReadYourOwnWrites
    check MC_DefraLevel_Green_ValidatedScan            GREEN
    check MC_DefraLevel_Red_PlainScanDecides           RED INV_ScanDecisionsCurrent
    check MC_DefraLevel_Red_ReasonNewestWrite          RED INV_ReasonsExact
    # The exempt reads a caller must not decide on: a scan inside a
    # commutative prefix and a read by no parts. Each GREEN uses the
    # exemption as declared; each RED decides on it and loses.
    check MC_DefraLevel_Green_CommutativeScan          GREEN
    check MC_DefraLevel_Red_DecideOnCommutativeScan    RED INV_SeatExclusive
    check MC_DefraLevel_Green_ClaimUndeclared          GREEN
    check MC_DefraLevel_Green_NoParts                  GREEN
    check MC_DefraLevel_Red_DecideOnNoParts            RED INV_PartsCurrent
    # E1. Lean: Regolith/LsmOrder.lean, compact_range_reads_newest.
    spec=LsmOrder
    # E31, a flush's table and its memtable's retirement are one publication.
    # Lean: Regolith/LsmOrder.lean, flush_keeps_sources_disjoint,
    # split_install_holds_a_memtable_twice.
    check MC_LsmOrder_Red_SplitRetire                  RED NoDoubleSource
    check MC_LsmOrder_Green                            GREEN
    check MC_LsmOrder_Red_Intersect                    RED ReadNewest
    # Flush order, ingest placement, overlap demotion (E14, D13), binary
    # search. Lean: Regolith/LsmOrder.lean, install_oldest_same_order,
    # ingest_ordered, demote_reads_newest, level_read_bsearch.
    check MC_LsmOrder_Green_Flush                      GREEN
    check MC_LsmOrder_Red_FlushAnyOrder                RED ReadNewest
    check MC_LsmOrder_Green_Ingest                     GREEN
    check MC_LsmOrder_Red_IngestIgnoresUpper           RED ReadNewest
    check MC_LsmOrder_Green_Demote                     GREEN
    check MC_LsmOrder_Red_DemoteLevelOnly              RED ReadNewest
    check MC_LsmOrder_Red_NoDemotion                   RED ReadNewest
    # E38: a universal merge takes L0 files that lie next to one another.
    # Lean: Regolith/LsmOrder.lean, merge_window_reads_newest,
    # merge_gapped_breaks_reads.
    check MC_LsmOrder_Green_L0Merge                    GREEN
    check MC_LsmOrder_Red_L0MergeGap                   RED ReadNewest
    # E14 and E37, the range deletes of the tables an open moves out of
    # overlapping levels and those 0.1.x left below older keys they delete:
    # copied into the recovered memtable, and logged before the manifest
    # records the move and the check. Lean: Regolith/RangeDelete.lean,
    # hoisted_delete_reads_by_sequence.
    spec=LegacyRangeRead
    check MC_LegacyRangeRead_Green                     GREEN
    check MC_LegacyRangeRead_Red_NoHoist               RED ReadNewest
    check MC_LegacyRangeRead_Red_PreScanOnly           RED ReadNewest
    check MC_LegacyRangeRead_Red_MoveBeforeLog         RED ReadNewest
    check MC_LegacyRangeRead_Green_Buried              GREEN
    check MC_LegacyRangeRead_Red_BuriedRetired         RED ScanNewest
    check MC_LegacyRangeRead_Red_CheckBeforeLog        RED HeadNewest
    # E6. Lean: Regolith/BatchView.lean, batch_one_view_newest.
    spec=BatchRead
    check MC_BatchRead_Green                           GREEN
    check MC_BatchRead_Red_ViewPerKey                  RED BatchConsistent
    check MC_BatchRead_Red_ViewPerKey_Exact            RED BatchExact
    # E8. Lean: Regolith/Publication.lean, repeatable_snapshot.
    spec=IngestPublication
    check MC_IngestPublication_Green                   GREEN
    check MC_IngestPublication_Red_CommitPassesSlot    RED RepeatableSnapshot
    check MC_IngestPublication_Red_IngestPublishesEarly RED RepeatableSnapshot
    # D48: an ingest syncs the log before its manifest record. Lean:
    # Regolith/IngestDurability.lean, surviving_ingest_keeps_prefix.
    spec=IngestDurability
    check MC_IngestDurability_Green                    GREEN
    check MC_IngestDurability_Red_NoLogSync            RED GapFreePrefix
    # E2. Lean: Regolith/WalRecovery.lean, reachable_recovers_prefix.
    spec=WalRotation
    check MC_WalRotation_Green                         GREEN
    check MC_WalRotation_Red_NoSync                    RED RecoversPrefix
    check MC_WalRotation_Red_NoSync_Gap                RED NoGap
    # Plan 3.6, commit-ordered append. Each
    # Teeth row is GREEN: its mutant breaks nothing but its RED's invariant.
    # Lean: Regolith/Append.lean, dense_from_one, unique_positions,
    # at_most_once, commit_order, snapshot_sees_prefix.
    spec=CommitOrderedAppend
    check MC_CommitOrderedAppend_Green_Writers                 GREEN
    check MC_CommitOrderedAppend_Green_Groups                  GREEN
    check MC_CommitOrderedAppend_Green_Faults                  GREEN
    check MC_CommitOrderedAppend_Green_Pipeline                GREEN
    check MC_CommitOrderedAppend_Red_Counter                   RED INV_NoDisjointConflict
    check MC_CommitOrderedAppend_Red_Detached                  RED INV_OneCommitPerWrite
    check MC_CommitOrderedAppend_Red_DetachedOrder             RED INV_CommitOrder
    check MC_CommitOrderedAppend_Red_DetachedNumbering         RED INV_NumberedAtCommit
    check MC_CommitOrderedAppend_Red_LatestBound               RED INV_NoSkip
    check MC_CommitOrderedAppend_Red_Stamps                    RED INV_Dense
    check MC_CommitOrderedAppend_Red_ViewOnlyHead              RED INV_UniqueCursors
    check MC_CommitOrderedAppend_Red_OnceFromView              RED INV_AtMostOnce
    check MC_CommitOrderedAppend_Red_AssignBeforeValidation    RED INV_Dense
    check MC_CommitOrderedAppend_Red_HeadCache                 RED INV_Dense
    check MC_CommitOrderedAppend_Red_PublishedView             RED INV_UniqueCursors
    check MC_CommitOrderedAppend_Teeth_Counter                 GREEN
    check MC_CommitOrderedAppend_Teeth_Detached                GREEN
    check MC_CommitOrderedAppend_Teeth_LatestBound             GREEN
    check MC_CommitOrderedAppend_Teeth_Stamps                  GREEN
    check MC_CommitOrderedAppend_Teeth_ViewOnlyHead            GREEN
    check MC_CommitOrderedAppend_Teeth_OnceFromView            GREEN
    check MC_CommitOrderedAppend_Teeth_AssignBeforeValidation  GREEN
    check MC_CommitOrderedAppend_Teeth_HeadCache               GREEN
    check MC_CommitOrderedAppend_Teeth_PublishedView           GREEN
    # Plan 3.6 across crashes, per durability mode. Green_Eventual is also
    # the RED's teeth. Lean: Regolith/Append.lean, snapshot_sees_prefix;
    # Regolith/WalRecovery.lean, recovers_prefix.
    spec=CommitOrderedAppendCrash
    check MC_CommitOrderedAppendCrash_Green_Immediate          GREEN
    check MC_CommitOrderedAppendCrash_Green_Eventual           GREEN
    check MC_CommitOrderedAppendCrash_Red_EventualExternal     RED INV_ExternalStable
    # Plan 3.7, conflict-free allocation. Lean: Regolith/Allocate.lean,
    # ranges_disjoint, ranges_grow, uses_allocated, alloc_fresh.
    spec=Allocate
    check MC_Allocate_Green_Immediate                          GREEN
    check MC_Allocate_Green_Eventual                           GREEN
    check MC_Allocate_Red_InTxn                                RED INV_NeverConflicts
    check MC_Allocate_Red_LogAfterUse                          RED INV_UseDurable
    check MC_Allocate_Red_LogAfterUse_Reuse                    RED INV_UsesUnique
    check MC_Allocate_Teeth_InTxn                              GREEN
    check MC_Allocate_Teeth_LogAfterUse                        GREEN
    # A reservation is handed out before it is published; a log failure that
    # discards it writes the counter back. TLC only.
    check MC_Allocate_Green_Rollback                   GREEN
    check MC_Allocate_Red_ForgetOnRollback             RED INV_HandedOutOnce
    # 4.2, E3: WAL format 2 replay; E46: close seals in place, and a close
    # with no record syncs once. Lean: Regolith/WalClose.lean and
    # Regolith/WalRecovery.lean,
    # recovers_prefix2.
    spec=WalRecovery
    check MC_WalRecovery_Green_Immediate               GREEN
    check MC_WalRecovery_Green_Eventual                GREEN
    check MC_WalRecovery_Green_Residual                GREEN
    check MC_WalRecovery_Green_Aead                    GREEN
    check MC_WalRecovery_Red_DropBelowP                RED NoProvenLoss
    check MC_WalRecovery_Red_RefuseAboveP              RED RecoveryOpens
    check MC_WalRecovery_Red_Format1                   RED RecoveryOpens
    check MC_WalRecovery_Red_CloseWithoutSync          RED RecoveryOpens
    check MC_WalRecovery_Red_NoTruncate                RED RecoveryOpens
    check MC_WalRecovery_Red_StampNotSealed            RED AckedSurvive
    check MC_WalRecovery_Red_StampUnsynced             RED RecoveryOpens
    check MC_WalRecovery_Red_NewLogBeforeFlush         RED RecoveryOpens
    check MC_WalRecovery_Red_UnstampedRefuses          RED RecoveryOpens
    # E29, E30: the manifest's torn end and the logs a flush retires, on a
    # sealed manifest and with flushes on several threads. Lean:
    # Regolith/ManifestRecovery.lean, crash_never_proves, replay_keeps_synced,
    # replay_never_above_newer_table, crash_never_proves_opened,
    # oldest_first_keeps_every_log.
    spec=ManifestRecovery
    check MC_ManifestRecovery_Green                    GREEN
    check MC_ManifestRecovery_Green_Refuses            RED NeverRefusesRot
    check MC_ManifestRecovery_Red_OldGuard             RED RecoveryOpens
    check MC_ManifestRecovery_Red_NoSyncBeforeNext     RED RecoveryOpens
    check MC_ManifestRecovery_Red_UnlinkBeforeSync     RED AckedSurvive
    check MC_ManifestRecovery_Red_RetireBeforeTable    RED AckedSurvive
    check MC_ManifestRecovery_Red_NoMinWalId           RED ReadsNewest
    check MC_ManifestRecovery_Red_IgnoreProof          RED RotSafe
    check MC_ManifestRecovery_Green_Sealed             GREEN
    check MC_ManifestRecovery_Red_JudgeCiphertext      RED RecoveryOpens
    check MC_ManifestRecovery_Green_Frozen2            GREEN
    check MC_ManifestRecovery_Red_NewestFirst          RED AckedSurvive
    # 4.12, D45: a sealed manifest batch keeps its checksum, checked before
    # any key. Lean: Regolith/ManifestSeal.lean, replay_opens_with_right_keys,
    # open_ends_only_at_a_torn_batch.
    spec=ManifestSeal
    check MC_ManifestSeal_Green                        GREEN
    check MC_ManifestSeal_Red_TagOnlyStop              RED OnlyTornEnds
    check MC_ManifestSeal_Red_TagOnlyRefuse            RED RecoveryOpens
    # 4.12, D57: a backup of an encrypted database seals its metadata, and a
    # restore checks every key before its first write; the listing names
    # every backup, a page of ids at a time, and a delete or purge sweeps
    # the pool a batch at a time, reading each listing as its walk of meta/
    # reaches it, and removes every unlisted table and no listed one. Lean:
    # Regolith/BackupSeal.lean, entitled_restore_is_faithful,
    # restored_tables_verify, refusal_writes_nothing, collect_keeps_listed,
    # last_write_holds_all, listing_names_every_backup, purge_keeps_the_newest,
    # collect_stream_eq, stream_holds_one_batch, sweep_eq,
    # sweep_removes_every_unlisted, list_pages_all.
    spec=BackupSeal
    check MC_BackupSeal_Green                          GREEN
    check MC_BackupSeal_Green_Attack                   GREEN
    check MC_BackupSeal_Green_Stream                   GREEN
    check MC_BackupSeal_Green_Sweep                    GREEN
    check MC_BackupSeal_Green_SweepTwoTables           GREEN
    check MC_BackupSeal_Red_StreamSkips                RED ListedRestores
    check MC_BackupSeal_Red_SweepSkips                 RED SweepRemovesEveryUnlisted
    check MC_BackupSeal_Red_PlainMeta                  RED SealedMetadata
    check MC_BackupSeal_Red_PlainRestore               RED SealedMetadata
    check MC_BackupSeal_Red_OpenUnderCurrent           RED RightKeysRestore
    check MC_BackupSeal_Red_ListingUnbound             RED FaithfulRestore
    check MC_BackupSeal_Red_IdUnbound                  RED FaithfulRestore
    check MC_BackupSeal_Red_ManifestFirst              RED FaithfulRestore
    check MC_BackupSeal_Red_OverDatabase               RED FaithfulRestore
    check MC_BackupSeal_Red_CheckAfterCopy             RED RefusalWritesNothing
    check MC_BackupSeal_Red_FooterUnchecked            RED RestoredOpens
    check MC_BackupSeal_Red_GcSkipsSealed              RED ListedRestores
    check MC_BackupSeal_Red_GcSkipsUnreadable          RED ListedRestores
    check MC_BackupSeal_Red_SyncOnlyRemoved            RED ListedRestores
    check MC_BackupSeal_Red_MetaFirst                  RED ListedRestores
    check MC_BackupSeal_Red_ListSkipsUnreadable        RED ListingNamesEvery
    check MC_BackupSeal_Red_ListNeedsKey               RED ListingNamesEvery
    # 4.8, E5: compaction per snapshot stripe. Lean: Regolith/Stripes.lean,
    # reduce_reads.
    spec=StripeCompaction
    check MC_StripeCompaction_Green                    GREEN
    check MC_StripeCompaction_Green_Filter             GREEN
    check MC_StripeCompaction_Green_Inexact            GREEN
    check MC_StripeCompaction_Red_CaptureEarly         RED SnapshotReadsKept
    check MC_StripeCompaction_Red_InexactFold          RED HeadKept
    check MC_StripeCompaction_Red_IgnoreStripes        RED SnapshotReadsKept
    # E27: a pass retires a range tombstone below its range and every
    # snapshot. Green_Retires is the witness that the fix retires at all.
    # The Rewrite rows check a pass over one run alone, which a pass picked
    # for its deletions (D14) runs on a bottom-level table and on the only
    # run of a universal pool.
    # Lean: Regolith/TombstoneRetirement.lean, retire_keeps_reads.
    spec=TombstoneRetirement
    check MC_TombstoneRetirement_Green                 GREEN
    check MC_TombstoneRetirement_Green_Retires         RED NothingRetired
    check MC_TombstoneRetirement_Red_IgnoreSnapshots   RED SnapshotReadsKept
    check MC_TombstoneRetirement_Red_IgnoreDeeper      RED SnapshotReadsKept
    check MC_TombstoneRetirement_Green_Rewrite         GREEN
    check MC_TombstoneRetirement_Green_RewriteRetires  RED NothingRetired
    check MC_TombstoneRetirement_Red_RewriteIgnoreSnapshots RED SnapshotReadsKept
    check MC_TombstoneRetirement_Red_RewriteIgnoreDeeper RED SnapshotReadsKept
    # 4.6, D54: the snapshot registry on per-thread slots with counted
    # entries, moved handles, copies and shared slots. Lean:
    # Regolith/SnapshotRegistry.lean, scan_respects_live,
    # clone_respects_live, pins_exact.
    spec=SnapshotRegistry
    check MC_SnapshotRegistry_Green                    GREEN
    check MC_SnapshotRegistry_Green_ThreeHandles       GREEN
    check MC_SnapshotRegistry_Red_NoConfirm            RED MinBelowLive
    check MC_SnapshotRegistry_Red_ScanThenSample       RED MinBelowLive
    check MC_SnapshotRegistry_Red_ReleaseHere          RED LiveCovered
    check MC_SnapshotRegistry_Red_CloneFresh           RED LiveCovered
    check MC_SnapshotRegistry_Red_PlainClaim           RED MinBelowLive
    check MC_SnapshotRegistry_Red_JoinNoRecheck        RED LiveEntryExact
    # E15: waiting for the last snapshot. With threads, the release that
    # drains the last pin wakes the waiter; on a target with one thread the
    # call reports what is still pinned and never waits. Lean:
    # Regolith/SnapshotWait.lean, no_lost_wake, one_thread_never_waits.
    spec=SnapshotWait
    check MC_SnapshotWait_Green                        GREEN
    check MC_SnapshotWait_Green_NoThreads              GREEN
    check MC_SnapshotWait_Red_WaitAnyway               RED NeverWaitsAlone
    check MC_SnapshotWait_Red_FutureAfterWatch         RED NoLostWake
    check MC_SnapshotWait_Red_ReadThenMark             RED NoLostWake
    # 4.6, D54: per-thread statistics shards. Lean: Regolith/ShardedStats.lean,
    # total_after_adds, read_between, reads_monotone.
    spec=ShardedStats
    check MC_ShardedStats_Green                        GREEN
    check MC_ShardedStats_Red_LostUpdate               RED QuiescentExact
    check MC_ShardedStats_Red_PartialRead              RED ReadBetween
    # A histogram sample's skipped updates (statistics.rs, HistogramData::record):
    # a sample of 0 leaves the sum alone, and the minimum and maximum are
    # loaded and moved only when the sample lies outside them; three threads
    # share one shard. Green_ReachesStaleSkip is a witness: a skip on a bound
    # another thread has since moved is reached. Lean:
    # Regolith/HistogramBounds.lean, minRun_eq, maxRun_eq, sumRun_eq.
    spec=HistogramBounds
    check MC_HistogramBounds_Green                     GREEN
    check MC_HistogramBounds_Red_StoreAfterSkip        RED MinExact
    check MC_HistogramBounds_Red_WrongDirection        RED MinExact
    check MC_HistogramBounds_Red_MaxStoreAfterSkip     RED MaxExact
    check MC_HistogramBounds_Red_SumLoadStore          RED SumExact
    check MC_HistogramBounds_Green_ReachesStaleSkip    RED NeverSkipsStale
    # E10 and 4.7, the lock-free commit pipeline as built: helping, voiding,
    # the check in two parts (early, then in ring order), the round (one
    # claim on the log's device step), the rollback of a failed log, ordered
    # units and rotation through the ring, and bounded work (E21). It carries
    # every rule of the group commit it replaced; the model's header lists
    # which rule and which row took each one over. A Green row whose name
    # ends in Discards, Stale, Rotates, Voids, Helps or Aborts is a witness:
    # the GREEN run beside it with one false claim added, which must break,
    # so the GREEN run is known to reach the case. Lean:
    # Regolith/Pipeline.lean, seqs_dense, reader_sees_published,
    # rollback_as_if_never_decided, rollback_keeps_published,
    # sync_covers_only_written; Regolith/PipelineCheck.lean,
    # later_slot_aborts, abort_has_cause, early_check_split,
    # published_only_loses_an_update; Regolith/PipelineHelping.lean,
    # first_unfinished_can_step, helper_alone_finishes, helping_is_bounded.
    spec=CommitPipeline
    check MC_CommitPipeline_Green                           GREEN
    check MC_CommitPipeline_Green_Stop                      GREEN
    check MC_CommitPipeline_Green_Live                      GREEN
    check MC_CommitPipeline_Green_LiveIngest                GREEN
    check MC_CommitPipeline_Green_LiveApart                 GREEN
    check MC_CommitPipeline_Green_LiveThree_Head1           GREEN
    check MC_CommitPipeline_Green_LiveThree_Head2           GREEN
    check MC_CommitPipeline_Green_LiveThree_Head3           GREEN
    check MC_CommitPipeline_Green_Rollback                  GREEN
    check MC_CommitPipeline_Green_Rollback_Discards         RED NothingThrownOut
    check MC_CommitPipeline_Green_Rollback_Stale            RED NoStaleVerdict
    check MC_CommitPipeline_Green_Rollback_Three            GREEN
    check MC_CommitPipeline_Green_Rotation                  GREEN
    check MC_CommitPipeline_Green_Rotation_Rotates          RED NeverRotated
    check MC_CommitPipeline_Green_Rotation_Three            GREEN
    check MC_CommitPipeline_Green_Unit_Rollback             GREEN
    check MC_CommitPipeline_Green_Live_Voids                RED NothingVoided
    check MC_CommitPipeline_Green_Live_Helps                RED StoppedStaysUnfinished
    check MC_CommitPipeline_Green_Early                     GREEN
    check MC_CommitPipeline_Green_Early_Aborts              RED NoEarlyAbort
    check MC_CommitPipeline_Green_ReadOnly                  GREEN
    check MC_CommitPipeline_Green_Plain                     GREEN
    check MC_CommitPipeline_Red_OutOfOrder                  RED ReadersSeePrefix
    check MC_CommitPipeline_Red_AbortHole                   RED NoHole
    check MC_CommitPipeline_Red_NoHelping                   RED NoLiveThreadBlocked
    check MC_CommitPipeline_Red_SyncPastGap                 RED DurableImpliesWritten
    check MC_CommitPipeline_Red_ValidatePublished           RED NoLostUpdate
    check MC_CommitPipeline_Red_VoidNotCas                  RED OwnerSeesItsFate
    check MC_CommitPipeline_Red_TwoSyncs                    RED OneRound
    check MC_CommitPipeline_Red_TwoSyncs_Durable            RED DurableWithinDecided
    check MC_CommitPipeline_Red_FrontierFirst               RED PublishedIsVisible
    check MC_CommitPipeline_Red_UnitNoDrain                 RED DurableImpliesWritten
    check MC_CommitPipeline_Red_UnitNoDrain_SealedLog       RED InFlightInCurrentLog
    check MC_CommitPipeline_Red_UnitNoDrain_Unit            RED DurableImpliesWritten
    check MC_CommitPipeline_Red_SettleAnyEpoch              RED NoHole
    check MC_CommitPipeline_Red_SettleAnyEpoch_Abort        RED AbortsHaveCause
    check MC_CommitPipeline_Red_KeepMemtable                RED MemtableOnlyLive
    check MC_CommitPipeline_Red_NoEpochGate                 RED RecordsAreLive
    check MC_CommitPipeline_Red_PassBeforeApply             RED AppliedBeforePassed
    check MC_CommitPipeline_Red_PassBeforeApply_LostUpdate  RED NoLostUpdate
    check MC_CommitPipeline_Red_RollbackForgets             RED PublishedPrefix
    check MC_CommitPipeline_Red_RollbackForgets_Ack         RED NoFalseAck
    check MC_CommitPipeline_Red_NoFreeze                    RED NoHole
    check MC_CommitPipeline_Red_TrustEarly                  RED NoLostUpdate
    check MC_CommitPipeline_Red_SkipOnPublished             RED NoLostUpdate
    check MC_CommitPipeline_Red_PublishBeforeSync           RED DurableBeforeVisible
    check MC_CommitPipeline_Red_DecideAnyOrder              RED DecidedIsPrefix
    check MC_CommitPipeline_Red_DecideAnyOrder_Order        RED SeqInRingOrder
    check MC_CommitPipeline_Red_WorkBeyond                  RED WorksUpToItsOwn
    check MC_CommitPipeline_Red_StopHoldsClaim              RED NoLiveThreadBlocked
    # The write unit (E45, L58; src/engine/commit/write_unit.rs): the owner
    # encodes its record, or a thread that needs it and finds nobody encoding
    # it does; one thread at a time writes the run of filled records with one
    # write and publishes what it made publishable; the others park on their
    # record and are woken, or run out of patience and write it themselves
    # as a copy (WG4: a late copy or run never lands under a sync that
    # covers it). Rust: src/engine/commit/late_copy_tests.rs and
    # write_unit_tests.rs, the loom models in
    # src/engine/commit/loom_write_unit.rs. Lean:
    # Regolith/PipelineHelping.lean, a_park_is_bounded, run_step_counts,
    # encoding_never_adds.
    check MC_CommitPipeline_Green_Copies                    GREEN
    check MC_CommitPipeline_Green_Copies_InFlight           RED NoTwoCopies
    check MC_CommitPipeline_Green_Copies_Runs               RED NoRunOfTwo
    check MC_CommitPipeline_Green_Copies_Parks              RED NeverAsleep
    check MC_CommitPipeline_Green_Copies_Steals             RED NeverStolen
    check MC_CommitPipeline_Green_Copies_Woken              GREEN
    check MC_CommitPipeline_Green_Copies_Stop_Head1         GREEN
    check MC_CommitPipeline_Green_Copies_Stop_Head2         GREEN
    check MC_CommitPipeline_Green_Copies_Stop_Holder        RED HolderNeverStops
    check MC_CommitPipeline_Green_Copies_Rollback_Head1     GREEN
    check MC_CommitPipeline_Green_Copies_Rollback_Head2     GREEN
    check MC_CommitPipeline_Red_SyncNoWait                  RED NoWriteUnderSync
    check MC_CommitPipeline_Red_StealNoRecheck              RED NoWriteUnderSync
    check MC_CommitPipeline_Red_GatherBeforeCountIn         RED NoWriteUnderSync
    check MC_CommitPipeline_Red_LostWake                    RED NoLiveThreadBlocked
    # Three threads (about 55 s, the one row over 20 s): a parked thread
    # that trusts a load showing an old claim's note sleeps for good.
    check MC_CommitPipeline_Red_NoteTrustsALoad             RED NoLiveThreadBlocked
    check MC_CommitPipeline_Green_Copies_HelperEncodes      RED OwnersEncodeOwn
    check MC_CommitPipeline_Green_Copies_RunPastOwn         RED NoRunPastOwn
    check MC_CommitPipeline_Red_EncodeNoClaim               RED OneEncoder
    check MC_CommitPipeline_Red_RunSkipsCheck               RED DurableBeforeVisible
    # The fast path (WU8): an owner writes its record by itself when it finds
    # the unit free with no note on it. Green_Direct_AnyTime takes it
    # with its guard removed and stays GREEN: the guard is about speed, the
    # look again once counted in keeps it safe (Red_DirectNoRecheck). Loom:
    # the_fast_path_racing_a_claimer_writes_only_whole_records.
    check MC_CommitPipeline_Green_Copies_Direct             RED NeverDirect
    check MC_CommitPipeline_Green_Direct_AnyTime            GREEN
    check MC_CommitPipeline_Red_DirectNoRecheck             RED NoWriteUnderSync
    # A blocking thread far from its turn (E45 step 2, P6;
    # src/engine/commit/turn_wait.rs) parks on its own order; the pass that
    # brings its turn near names it, and the next finished commit tells it.
    # Green_Turn checks with no time outs that nobody sleeps for good. Rust:
    # src/engine/commit/turn_wait_tests.rs, the loom models in
    # src/engine/commit/loom_turn_wait.rs. Lean:
    # Regolith/PipelineHelping.lean, a_turn_park_is_bounded.
    check MC_CommitPipeline_Green_Copies_TurnParks          RED NeverTurnParked
    check MC_CommitPipeline_Green_Turn                      GREEN
    check MC_CommitPipeline_Green_Turn_Parks                RED NeverTurnParked
    check MC_CommitPipeline_Red_TurnNeverTold               RED NoLiveThreadBlocked
    check MC_CommitPipeline_Red_LeadZero                    RED NoLiveThreadBlocked
    # Decision 3 of plan 4.7: a queue's poll never waits for another thread.
    # A job that cannot get the round, the version store's step, the flush
    # exclusion or a failed log clear of writes parks: it leaves a note, looks
    # again, gives back what it took and frees its claim, and whoever waited
    # on it is told. Green_Parks is the witness that the GREEN runs park.
    # Lean: Regolith/Parking.lean, no_parked_job_is_forgotten,
    # no_second_look_forgets_a_job.
    spec=Parking
    check MC_Parking_Green                             GREEN
    check MC_Parking_Green_Round                       GREEN
    check MC_Parking_Green_OneQueue                    GREEN
    check MC_Parking_Green_Sleeper                     GREEN
    check MC_Parking_Green_TwoQueues                   GREEN
    check MC_Parking_Green_Parks                       RED NeverParked
    check MC_Parking_Red_PollWaits                     RED NoPollWaits
    check MC_Parking_Red_KeepsTool                     RED ParkedHoldsNothing
    check MC_Parking_Red_NoSecondLook                  RED JobLands
    check MC_Parking_Red_ReleaseTellsNobody            RED JobLands
    check MC_Parking_Red_WakeOnlyStepper               RED JobLands
    check MC_Parking_Red_ParkTellsNobody               RED JobLands
    check MC_Parking_Red_WakeFindsClaimed              RED JobLands
    check MC_Parking_Red_StaleTellDelivers             RED JobLands
    # The step that replaced the version store's mutex: one section at a
    # time, in the order handed over, run by whichever thread holds the step,
    # none left behind, and a job that keeps the step never waits.
    # Green_Parks is the witness that the GREEN run parks a job, and
    # Red_KeeperStops the stated limit: a job that stops while it keeps the
    # step keeps the callers behind it waiting. Lean:
    # Regolith/VersionStep.lean, one_holder, runs_in_push_order,
    # none_left_behind, giving_up_blind_leaves_a_section.
    spec=VersionStep
    check MC_VersionStep_Green                         GREEN
    check MC_VersionStep_Green_OneCaller               GREEN
    check MC_VersionStep_Green_Parks                   RED NeverParked
    check MC_VersionStep_Red_HoldWithoutCas            RED OneAtATime
    check MC_VersionStep_Red_HoldWithoutCas_LostEdit   RED NoLostEdit
    check MC_VersionStep_Red_GiveUpBlind               RED NoneLeftBehind
    check MC_VersionStep_Red_NewestFirst               RED InPushOrder
    check MC_VersionStep_Red_ReturnEarly               RED FinishedBeforeReturn
    check MC_VersionStep_Red_HolderWaits               RED NoHolderWaits
    check MC_VersionStep_Red_ReleaseTellsNobody        RED EverySectionRuns
    check MC_VersionStep_Red_NoSecondTry               RED EverySectionRuns
    check MC_VersionStep_Red_KeeperStops               RED EverySectionRuns
    # The landing listeners of a slot: told exactly once, after the commit
    # is visible or discarded, by whichever thread lands the slot; a
    # publisher that stops before it takes them leaves the telling to the
    # next. Red_StopAfterTake is the stated limit: one that stops holding
    # the listeners it took keeps them. Lean: Regolith/Landing.lean,
    # told_at_most_once, one_place, closed_sheet_is_empty,
    # read_then_clear_tells_twice.
    spec=Landing
    check MC_Landing_Green                             GREEN
    check MC_Landing_Green_Discarded                   GREEN
    check MC_Landing_Red_TakeNoShut                    RED EveryListenerTold
    check MC_Landing_Red_ReadThenClear                 RED ToldAtMostOnce
    check MC_Landing_Red_TellBeforeHorizon             RED ToldAfterLanding
    check MC_Landing_Red_FrontierFirst                 RED EveryListenerTold
    check MC_Landing_Red_StopAfterTake                 RED EveryListenerTold
    # R13 and D53, non-blocking reads on per-thread I/O queues: single-flight
    # units claimed by one CAS, completions only on the asking queue, idle
    # wakeups. Lean: Regolith/IoQueue.lean, claim_once, told_at_most_once,
    # every_waiter_told, no_lost_idle_wakeup, busy_never_woken,
    # single_thread_finishes.
    spec=NonBlocking
    check MC_NonBlocking_Green_Pool                    GREEN
    check MC_NonBlocking_Green_PoolLive                GREEN
    check MC_NonBlocking_Green_Jobs                    GREEN
    check MC_NonBlocking_Green_JobsLive                GREEN
    check MC_NonBlocking_Green_Single                  GREEN
    check MC_NonBlocking_Green_SingleLive              GREEN
    check MC_NonBlocking_Red_WrongQueue                RED OwnQueue
    check MC_NonBlocking_Red_LostIdleWakeup            RED NoLostIdleWakeup
    check MC_NonBlocking_Red_DoubleRun                 RED SingleRun
    check MC_NonBlocking_Red_SelfIoNowhere             RED NoOrphanUnit
    check MC_NonBlocking_Red_BusyWoken                 RED BusyNeverWoken
    # D53, the commit side's units: a sync round claimed by the poll of one
    # of the queues that wait on it and told to each of them, and a stall's
    # passive unit landed by its clearer or by a writer's second look. Lean:
    # Regolith/IoQueue.lean, round_sync_once, round_visible_after_sync,
    # ticket_ready_only_at_own_poll, stall_lands_when_clear.
    check MC_NonBlocking_Green_Commit                  GREEN
    check MC_NonBlocking_Green_Stall                   GREEN
    check MC_NonBlocking_Red_DoubleSync                RED SingleRun
    check MC_NonBlocking_Red_PublishAtWrite            RED VisibleAfterSync
    check MC_NonBlocking_Red_DeliverOnLander           RED DeliveredOnOwnPoll
    check MC_NonBlocking_Red_NoRecheck                 RED StallLandsWhenClear
    # D53, timed waits (the wait a retry backs off on): completed only by
    # the poll of the queue that was asked, not before the due time, by an
    # owner that polls again within what its poll reported, and by the drop
    # of the queue. Lean: Regolith/IoQueue.lean, timed_wait_completed_once,
    # timed_wait_completed_by_owner, timed_wait_not_before_due,
    # dropped_queue_holds_no_wait, poll_after_wake_in_completes.
    check MC_NonBlocking_Green_Timer                   GREEN
    check MC_NonBlocking_Green_TimerLive               GREEN
    check MC_NonBlocking_Red_IdlePastDue               RED AllDone
    check MC_NonBlocking_Red_EarlyFire                 RED TimerNotEarly
    check MC_NonBlocking_Red_DropForgets               RED DroppedHoldsNothing
    check MC_NonBlocking_Red_TimerWrongQueue           RED TimerOnOwnQueue
    # D53, what a read run again finds: a landing that a call still needs is
    # held past the queue's byte bound until the call runs again, so a call
    # needing more blocks at once than the bound finishes, and the eviction
    # that held nothing never let it.
    spec=LandingHold
    check MC_LandingHold_Green                         GREEN
    check MC_LandingHold_Green_Three                   GREEN
    check MC_LandingHold_Red_Fifo                      RED EveryCallFinishes
    check MC_LandingHold_Red_NoHolds                   RED RunsKeepProgress
    # A completed wait keeps its landing while a delayed caller owns it;
    # polls still reclaim abandoned waits behind live ones.
    spec=LandingLifetime
    check MC_LandingLifetime_Green                     GREEN
    check MC_LandingLifetime_Red_ExpireLive             RED LiveLandingRetained
    check MC_LandingLifetime_Red_ConsumeLive            RED LiveLandingRetained
    check MC_LandingLifetime_Red_LeakAbandoned          RED AbandonedReleased
    check MC_LandingLifetime_Red_LeakLive               RED LiveBounded
    # A let-go landing frees its block while kovan has yet to reclaim the
    # map nodes that named it: both maps hold weak handles. Lean:
    # Regolith/LandingReclaim.lean.
    spec=LandingReclaim
    check MC_LandingReclaim_Green                      GREEN
    check MC_LandingReclaim_Green_ReachesIdle          RED NeverIdleUnreclaimed
    check MC_LandingReclaim_Red_StrongInMap            RED INV_ReleasedBlockFreed
    check MC_LandingReclaim_Red_StrongUnit             RED INV_ReleasedBlockFreed
    # D53, a CacheOnly commit whose validation misses the cache: a miss in
    # the early check leaves the commit owed on its queue, which reads the
    # block and submits it again; a miss deciding its slot leaves the
    # decision on the queue of a thread that must not read the device. The
    # commit is never ended by a miss, and its call never reads the device.
    # Lean: Regolith/PipelineCheck.lean, early_check_split.
    spec=ValidationFloor
    check MC_ValidationFloor_Green GREEN
    check MC_ValidationFloor_Red_AlwaysSkip RED Exact
    check MC_ValidationFloor_Red_StaleIngestCeiling RED Bounded
    spec=ValidationProgress
    check MC_ValidationProgress_Green GREEN
    check MC_ValidationProgress_Green_NoOldChange GREEN
    check MC_ValidationProgress_Red_Restart RED Cost
    check MC_ValidationProgress_Red_SkipMiss RED Exact
    check MC_ValidationProgress_Red_SkipLateCheck RED Exact
    spec=NowaitMiss
    check MC_NowaitMiss_Green                          GREEN
    check MC_NowaitMiss_Red_Refuse                     RED INV_NoMissEnds
    check MC_NowaitMiss_Red_ReadOnCaller               RED INV_CallNeverReads
    check MC_NowaitMiss_Red_NoResubmit                 RED LIVE_EveryCommitEnds
    check MC_NowaitMiss_Red_StaleHorizon               RED INV_Exact
    # Dropping a range of a commit-ordered log inside a transaction
    # (Transaction::truncate_log): it deletes exactly the entry keys of its
    # range, leaves the head alone, and never conflicts, because a delete of a
    # key another commit deleted is elided and an append writes only past the
    # head the truncate read; a snapshot taken before it keeps the entries;
    # a rollback to a savepoint drops it. Lean: Regolith/LogTruncate.lean,
    # live_is_head_less_truncated, snapshot_reads_as_of, step_keeps_reads,
    # no_version_past_head, truncate_never_conflicts, head_monotone,
    # truncate_commutes, truncate_idempotent, and the REDs
    # unguarded_truncate_breaks_law and erasing_loses_snapshot.
    spec=LogTruncate
    check MC_LogTruncate_Green                         GREEN
    check MC_LogTruncate_Green_Three                   GREEN
    check MC_LogTruncate_Red_OrdinaryWrite             RED INV_NoConflict
    check MC_LogTruncate_Red_ReadHead                  RED INV_NoConflict
    check MC_LogTruncate_Red_NoHeadGuard               RED INV_WithinSeenHead
    check MC_LogTruncate_Red_NoHeadGuard_Conflict      RED INV_NoConflict
    check MC_LogTruncate_Red_OffByOne                  RED INV_LiveLaw
    check MC_LogTruncate_Red_RewindsHead               RED INV_HeadMonotone
    check MC_LogTruncate_Red_ErasesOnTruncate          RED INV_ReaderStable
    check MC_LogTruncate_Red_CompactIgnoresPins        RED INV_ReaderStable
    check MC_LogTruncate_Red_RollbackKeepsTruncate     RED INV_RollbackHonoured
    check MC_LogTruncate_Red_OpenKeys                  RED INV_LiveLaw
    # Reading a transaction's own appends back (Transaction::appended): the
    # entries it returns for a log are exactly the ones its commit numbers
    # for that log, in that order, with no position among them; a rollback to
    # a savepoint gives back the answer the savepoint saw. The reader guard
    # of MC_DefraLevel_Red_OwnAppendVisible is untouched, because no position
    # is given. Lean: Regolith/Appended.lean, appended_push_same,
    # appended_push_other, rollback_restores, appended_after_rollback,
    # appended_is_commit_order and the RED newest_first_breaks_order.
    spec=AppendedEntries
    check MC_AppendedEntries_Green                     GREEN
    check MC_AppendedEntries_Red_NewestFirst           RED INV_AppendedIsCommitOrder
    check MC_AppendedEntries_Red_OtherLog              RED INV_AppendedIsCommitOrder
    check MC_AppendedEntries_Red_RollbackKeeps         RED INV_RollbackRestores
    # Phase 7 integration: with no worker and inline compaction on, no
    # stopped writer waits on a relief nobody runs. Every piece of progress
    # lands the stall's unit (relief handed back), a contended writer reads
    # the progress count again once registered, and a foreground job that
    # held the gate relieves the stop. Lean: Regolith/StallRelief.lean,
    # no_stranded_wait, no_relief_strands_a_waiter,
    # no_hand_back_strands_a_waiter, no_recheck_strands_a_waiter.
    spec=StallRelief
    check MC_StallRelief_Green                         GREEN
    check MC_StallRelief_Red_NoRelief                  RED NoStrandedWait
    check MC_StallRelief_Red_NoHandBack                RED NoStrandedWait
    check MC_StallRelief_Red_NoRecheck                 RED NoStrandedWait
    # E35: an engine that latches clears no stop, so the latch lands the
    # stall's unit (its flag stored first, read again by a writer once
    # registered), and a rotation that flushed at the memtable cap counts
    # its progress whether its seal went through or failed. Lean:
    # no_wait_once_latched, no_latch_land_strands_a_waiter,
    # no_latch_recheck_strands_a_waiter, no_rotate_refresh_strands_a_waiter.
    check MC_StallRelief_Green_Failures                GREEN
    check MC_StallRelief_Red_NoLatchLand               RED NoWaitOnceLatched
    check MC_StallRelief_Red_NoLatchRecheck            RED NoWaitOnceLatched
    check MC_StallRelief_Red_NoRotateRefresh           RED NoStrandedWait
    # E36: a write stopped while its relief's failure is recorded is told
    # that failure, so recording one lands the stall's unit (stored first,
    # read again by a writer once registered, while a stop still holds), and
    # a write that was already waiting is told the same. Lean:
    # no_wait_once_failing, no_fail_land_strands_a_waiter,
    # no_fail_recheck_strands_a_waiter.
    check MC_StallRelief_Green_ReliefFails             GREEN
    check MC_StallRelief_Red_NoFailLand                RED NoWaitOnceFailing
    check MC_StallRelief_Red_NoFailRecheck             RED NoWaitOnceFailing
    # A commit is never lost to a stall: however its call ends, its write
    # went in or it was told a failure. It is not handed the wait a plain
    # write is, inline compaction off included (L32), and it does not give
    # up while its steps make progress. Lean: Regolith/StallCommit.lean,
    # no_lost_commit, takes_wait_loses_a_commit, gives_up_loses_a_commit,
    # left_to_caller_loses_a_commit.
    check MC_StallRelief_Green_Commit                  GREEN
    check MC_StallRelief_Green_Commit_Failures         GREEN
    check MC_StallRelief_Red_CommitTakesWait           RED NoLostCommit
    check MC_StallRelief_Red_CommitGivesUp             RED NoLostCommit
    check MC_StallRelief_Green_Commit_InlineOff        GREEN
    check MC_StallRelief_Red_CommitLeftToCaller        RED NoLostCommit
    # E16: with no worker, a write that owes a step of background work and
    # runs it inline runs exactly one, and L0 stays at its compaction trigger
    # (at the stop trigger when every write seals). A writer with a queue
    # leaves the step there for its polls: L0 may then pass the trigger, and
    # stays within the stop trigger plus the sealed memtables a rotation
    # keeps. Lean: Regolith/OwedStep.lean, write_one_step, l0_bounded,
    # l0_bounded_by_stop, queued_l0_bounded, queued_passes_trigger.
    spec=OwedStep
    check MC_OwedStep_Green                            GREEN
    check MC_OwedStep_Green_TinyMemtable               GREEN
    check MC_OwedStep_Green_Queued                     GREEN
    check MC_OwedStep_Red_FlushOwesNothing             RED L0Bounded
    check MC_OwedStep_Red_StepUntilIdle                RED OneStepPerWrite
    check MC_OwedStep_Red_QueuedTightBound             RED L0Bounded
    # regolith::sync's locks after D49: barging with bounded bypass, for
    # Mutex, Semaphore and ReentrantMutex, one step per atomic instruction
    # of src/sync. Lean: Regolith/Sync.lean, mutual_exclusion,
    # bounded_bypass, owed_at_bound, owed_exclusive.
    spec=Sync
    check MC_Sync_Green_Mutex                          GREEN
    check MC_Sync_Green_MutexBarger                    GREEN
    check MC_Sync_Green_MutexBoth                      GREEN
    check MC_Sync_Green_MutexLive                      GREEN
    check MC_Sync_Green_Semaphore                      GREEN
    check MC_Sync_Green_BigRequest                     GREEN
    check MC_Sync_Green_SemaphoreLive                  GREEN
    check MC_Sync_Green_Reentrant                      GREEN
    check MC_Sync_Green_ReentrantLive                  GREEN
    check MC_Sync_Red_UnboundedBarging                 RED BoundedBypass
    check MC_Sync_Red_ReleaseNoWake                    RED NoLostWakeup
    check MC_Sync_Red_LoseQueuePosition                RED BoundedBypass
    check MC_Sync_Red_CancelNoPassOn                   RED CancelPassesOn
    check MC_Sync_Red_NoRecheck                        RED NoLostWakeup
    check MC_Sync_Red_NoDirty                          RED NoLostWakeup
    check MC_Sync_Red_LowerPendingMarker               RED EveryWaiterServed
    check MC_Sync_Red_IgnoreOwed                       RED HandoffExclusive
    check MC_Sync_Red_NoDepth                          RED ReentrancyDepth
    # E32, what a drain costs its thread: one pass, and one more for each
    # waiter operation, or release with a node queued, that lands on it.
    # Green_PassesGrow is the witness that the passes do grow with those
    # (lock-free, not wait-free): FewPasses is no promise and must fail.
    check MC_Sync_Red_ReleaseDirties                   RED DrainBounded
    check MC_Sync_Red_ExitIgnoresRequest               RED NoLostWakeup
    check MC_Sync_Green_PassesGrow                     RED FewPasses
    # E48: the spin before queueing and the spin on the waiter's own node.
    # The spin must stop for an owed handoff, and a spin that runs out must
    # read its node's marks as it arms the waker again: a release that found
    # it spinning called no waker.
    check MC_Sync_Red_SpinNoRecheck                    RED NoLostWakeup
    check MC_Sync_Red_SpinIgnoresOwed                  RED HandoffExclusive
    # regolith::sync::Notify, which keeps FIFO handoff. Lean:
    # Regolith/SyncFifo.lean, fifo_served, no_stranded_waiter.
    spec=SyncNotify
    check MC_SyncNotify_Green                          GREEN
    check MC_SyncNotify_Green_Live                     GREEN
    check MC_SyncNotify_Red_WakeBeforeHandoff          RED NoLostWakeup
    check MC_SyncNotify_Red_CancelNoPassOn             RED CancelPassesOn
    check MC_SyncNotify_Red_NoRecheck                  RED NoLostWakeup
    check MC_SyncNotify_Red_NoGenCheck                 RED NoLostWakeup
    check MC_SyncNotify_Red_FrontGenCheck              RED NoLostWakeup
    # regolith::sync::RwLock and ReentrantRwLock after D49, with D50's
    # upgradable read, one step of src/sync/raw_rwlock.rs at a time on the
    # queue of Sync.tla. Lean: Regolith/Sync.lean for the exclusive side.
    spec=SyncRwLock
    check MC_SyncRwLock_Green_Plain                    GREEN
    check MC_SyncRwLock_Green_WriterLive               GREEN
    check MC_SyncRwLock_Green_ReaderLive               GREEN
    check MC_SyncRwLock_Green_Upgrade                  GREEN
    check MC_SyncRwLock_Green_UpgradeLive              GREEN
    check MC_SyncRwLock_Green_Reentrant                GREEN
    check MC_SyncRwLock_Green_ReentrantLive            GREEN
    check MC_SyncRwLock_Red_UnboundedBarging           RED BoundedBypass
    check MC_SyncRwLock_Red_AnyReaderUpgrade           RED NoUpgradeDeadlock
    check MC_SyncRwLock_Red_UpgradeNoHoldBack          RED UpgradeHoldsBack
    check MC_SyncRwLock_Red_OwnerUnaware               RED NoSelfDeadlock
    # E33: a dropped upgrade while the reader leaves and the next task takes
    # the upgradable read. The RED is the step the fix removed: the pass
    # reads UPGRADING and takes the write for the node in its slot.
    check MC_SyncRwLock_Green_UpgradeDropped           GREEN
    check MC_SyncRwLock_Red_PassTakesWrite             RED GuardsCounted
    # E48: a spin on the node that runs out must read the marks as it arms
    # the waker again, for the RwLock's waits too.
    check MC_SyncRwLock_Red_SpinNoRecheck              RED NoLostWakeup
    # The compaction gate (4.6, E28): a pass shares it, a job holds it
    # alone, no waiter is passed over more than Bound times either way, the
    # worker takes turns between jobs and passes, and a job parked on its
    # queue is always woken. The old gate's rule is Red_OldGate. Lean:
    # Regolith/CompactionGate.lean, mutual_exclusion, bounded_bypass,
    # owed_only_gets_nearer, worker_takes_turns, parked_job_keeps_its_wake.
    spec=CompactionGate
    check MC_CompactionGate_Green                      GREEN
    check MC_CompactionGate_Green_All                  GREEN
    check MC_CompactionGate_Green_Queue                GREEN
    check MC_CompactionGate_Green_Passes               GREEN
    check MC_CompactionGate_Green_Live                 GREEN
    check MC_CompactionGate_Green_LiveServed           GREEN
    check MC_CompactionGate_Red_OldGate                RED BoundedBypass
    check MC_CompactionGate_Red_NoHandoff              RED BoundedBypass
    check MC_CompactionGate_Red_DrainJobs              RED WorkerTakesTurns
    check MC_CompactionGate_Red_WakeDropped            RED NoLostWake
    check MC_CompactionGate_Red_StaleWaker             RED NoLostWake
    check MC_CompactionGate_Red_SkipGate               RED MutualExclusion
    # regolith::sync::Event, Latch and Barrier.
    spec=SyncLatch
    check MC_SyncLatch_Green_Event                     GREEN
    check MC_SyncLatch_Green_Latch                     GREEN
    check MC_SyncLatch_Green_Barrier                   GREEN
    check MC_SyncLatch_Red_NoRecheck                   RED NoLostWakeup
    check MC_SyncLatch_Red_CountCheck                  RED NoLostWakeup
    # regolith::sync::OnceCell and Lazy: one pointer published by one
    # compare-and-swap, and a one-permit semaphore for the initializers'
    # turns, which a publisher opens wide.
    spec=SyncOnce
    check MC_SyncOnce_Green                            GREEN
    check MC_SyncOnce_Red_PlainStore                   RED PublishedOnce
    check MC_SyncOnce_Red_LoserKeepsOwn                RED ReadersAgree
    check MC_SyncOnce_Red_CancelNoReset                RED NoLostWakeup
    check MC_SyncOnce_Red_PublishNoOpen                RED NoLostWakeup
    # Transaction callbacks (3.16), told on the committing thread (D53), in
    # both forms of commit: the blocking commit `transact` calls, and
    # commit_nowait, whose pending ticket is resolved from its commit's
    # slot at delivery. Lean: Regolith/Callbacks.lean, exactly_once, order,
    # attempts_isolated, delivered_once, delivered_on_queue_thread,
    # resolved_after_landing, resolved_once_on_queue_thread.
    spec=TxnCallbacks
    check MC_TxnCallbacks_Green_Immediate              GREEN
    check MC_TxnCallbacks_Green_Blocking               GREEN
    check MC_TxnCallbacks_Green_Nowait                 GREEN
    check MC_TxnCallbacks_Green_Eventual               GREEN
    check MC_TxnCallbacks_Red_CommitBeforeDurable      RED CommitAfterDurable
    check MC_TxnCallbacks_Red_SkipCallbackWrites       RED NoLostUpdate
    check MC_TxnCallbacks_Red_CallbacksSurvive         RED AttemptIsolation
    check MC_TxnCallbacks_Red_DeliverNoClaim           RED AtMostOnce
    check MC_TxnCallbacks_Red_HelperDelivers           RED OnCommittingThread
    check MC_TxnCallbacks_Red_CloseNoClaim             RED AtMostOnce
    # The slots close finds the open transactions' claims in (claim_slots.rs):
    # a lock-free free stack with an ABA tag, one swap deciding who drops a
    # claim, and a drain that sees every claim listed when it began. The
    # Green_Reaches rows are witnesses: each GREEN run reaches its race.
    # Lean: Regolith/ClaimSlots.lean, place_bijective (every slot in one
    # chunk cell, for every size) and no_aba (the tag's bound); loom:
    # `just loom-claims`.
    spec=ClaimSlots
    check MC_ClaimSlots_Green                          GREEN
    check MC_ClaimSlots_Green_Reuse                    GREEN
    check MC_ClaimSlots_Green_ReuseShared              GREEN
    check MC_ClaimSlots_Green_ABA                      GREEN
    check MC_ClaimSlots_Green_Full                     GREEN
    check MC_ClaimSlots_Red_NoTag                      RED FreeStackSound
    check MC_ClaimSlots_Red_GiveBackFirst              RED CellBelongsToHolder
    check MC_ClaimSlots_Red_TakeByLoad                 RED DroppedOnce
    check MC_ClaimSlots_Red_NoClamp                    RED DrainInBounds
    check MC_ClaimSlots_Red_StopAtEmpty                RED LiveTakenByClose
    check MC_ClaimSlots_Green_ReachesABA               RED NeverABA
    check MC_ClaimSlots_Green_ReachesCloseFirst        RED NeverCloseFirst
    check MC_ClaimSlots_Green_ReachesLateChunk         RED NeverLateChunk
    # 4.6, Phase 7b: the wait-free read view (D54). Lean:
    # Regolith/ReadView.lean, reachable_safe and cas_chain.
    spec=ReadView
    check MC_ReadView_Green                            GREEN
    check MC_ReadView_Red_FreeEarly                    RED NoFreedRead
    check MC_ReadView_Red_StaleLoad                    RED FreshLoad
    check MC_ReadView_Red_PlainStore                   RED ChainOfPublications
    # The read view's publishers as the commit pipeline and the flush off the
    # commit path have them, every one by compare-and-swap: a rotation in its
    # ordered unit and a rollback, one at a time (the round); a flush, a
    # compaction and an ingest, one version publish at a time (the step). A
    # flush installs its table and retires its memtable in one publication.
    # Lean: Regolith/ReadViewPublishers.lean, no_lost_memtable,
    # retired_stay_gone, replaced_stay_gone, flush_keeps_sources_disjoint,
    # split_install_holds_a_memtable_twice, plain_retire_loses_a_seal,
    # plain_version_loses_a_seal, plain_rollback_loses_an_ingest,
    # plain_rollback_brings_back_a_flushed_memtable,
    # seal_beside_a_rollback_freezes_the_replaced_memtable.
    spec=ReadViewPublishers
    check MC_ReadViewPublishers_Green                  GREEN
    check MC_ReadViewPublishers_Red_SplitRetire        RED NoDoubleSource
    check MC_ReadViewPublishers_Red_PlainRetire        RED NoLostMemtable
    check MC_ReadViewPublishers_Red_PlainVersion       RED NoLostMemtable
    check MC_ReadViewPublishers_Red_PlainRollback      RED NoLostIngest
    check MC_ReadViewPublishers_Red_PlainRollbackFlush RED RetiredStayGone
    check MC_ReadViewPublishers_Red_SealOutsideRound   RED ReplacedStayGone
    # 4.6, Phase 7b: the lock-free CLOCK block cache. Lean:
    # Regolith/ClockCache.lean, evict_only_unpinned, reachable_safe,
    # two_level_bounded and used_exact.
    spec=ClockCache
    check MC_ClockCache_Green                          GREEN
    check MC_ClockCache_Red_IgnorePins                 RED NoFreedPin
    check MC_ClockCache_Red_Unbounded                  RED ByteBound
    check MC_ClockCache_Red_LandingUnheld              RED LandedReadable
    # 4.6, Phase 7b and 4.7: a memtable's append-only range-tombstone log,
    # appended to by whichever threads apply commits. Green_Twice is the
    # witness that the GREEN run appends one tombstone twice. Lean:
    # Regolith/TombstoneLog.lean, one_slot_each, ready_whole and
    # published_seen.
    spec=TombstoneLog
    check MC_TombstoneLog_Green               GREEN
    check MC_TombstoneLog_Green_Readers       GREEN
    check MC_TombstoneLog_Green_Twice         RED NeverTwice
    check MC_TombstoneLog_Red_ReadyFirst      RED ReadWhole
    check MC_TombstoneLog_Red_HorizonFirst    RED PublishedSeen
    check MC_TombstoneLog_Red_LoadStore       RED OneSlotEach
    check MC_TombstoneLog_Red_LoadStore_Lost  RED PublishedSeen
    # 4.6, Phase 7c1: the open-file slot table under max_open_files. Lean:
    # Regolith/OpenFileTable.lean, open_le_cap, never_closes_in_use,
    # reads_own_file.
    spec=OpenFileTable
    check MC_OpenFileTable_Green                       GREEN
    check MC_OpenFileTable_Green_TwoSlots              GREEN
    check MC_OpenFileTable_Red_IgnoreReaders           RED NeverClosedInUse
    check MC_OpenFileTable_Red_NoOwnerRecheck          RED ReadsOwnFile
    check MC_OpenFileTable_Red_OutsideTable            RED AtMostCap
    # D60: a queue unit's reopen never waits for a slot, and a parked one is
    # never forgotten. Lean: Regolith/OpenFileNoWait.lean, never_waits;
    # Regolith/OpenFileNoWaitProof.lean, retry_never_lost; the REDs in
    # Regolith/OpenFileNoWaitRed.lean.
    check MC_OpenFileTable_Green_NoWait                GREEN
    check MC_OpenFileTable_Green_NoWaitTwoSlots        GREEN
    check MC_OpenFileTable_Red_Waits                   RED NeverWaits
    check MC_OpenFileTable_Red_MarkFirst               RED RetryNeverLost
    check MC_OpenFileTable_Red_PublishDropsWanted      RED RetryNeverLost
    check MC_OpenFileTable_Red_WakeNeedsParked         RED RetryNeverLost
    # 4.6, Phase 7c1 and 4.7: column-family create, drop and use, fenced when
    # a slot of the commit ring is decided. Lean: Regolith/CfRegistry.lean,
    # no_write_after_drop, no_write_before_birth, life_in_order.
    spec=CfRegistry
    check MC_CfRegistry_Green                          GREEN
    check MC_CfRegistry_Red_FenceOutside               RED NoWriteAfterDrop
    check MC_CfRegistry_Red_RetireLate                 RED NoWriteAfterDrop
    check MC_CfRegistry_Red_ReuseId                    RED LifeInOrder
    # The fence on the pipeline's one path: a write's, a transaction's and
    # an ingest's slot are each checked when the slot is decided, not when it
    # is taken. Lean: ring_no_write_after_drop,
    # fenced_at_install_lands_after_drop.
    check MC_CfRegistry_Red_FenceAtInstall             RED NoWriteAfterDrop
    check MC_CfRegistry_Red_TxnUnfenced                RED NoWriteAfterDrop
    # 4.6, Phase 7c1: the env file maps, positional reads and lock-free
    # in-memory files. Lean: Regolith/EnvFiles.lean, positional_reads_own,
    # current_holds_finished, read_sees_finished.
    spec=EnvFiles
    check MC_EnvFiles_Green                            GREEN
    check MC_EnvFiles_Green_ThreeWriters               GREEN
    check MC_EnvFiles_Red_SharedCursor                 RED ReadsOwnBytes
    check MC_EnvFiles_Red_NoFreeze                     RED CurrentHoldsFinished
    check MC_EnvFiles_Red_StaleFrozen                  RED CurrentHoldsFinished
    # A table renamed aside while its handles reopen it: the Env's rename adds
    # the new name before it drops the old, and a reopen that found nothing
    # loads the names again. Lean: Regolith/EnvFilesRename.lean,
    # open_finds_file, and the REDs remove_first_loses and no_reload_loses.
    check MC_EnvFiles_Green_Rename                     GREEN
    check MC_EnvFiles_Red_RemoveFirst                  RED OpenFindsFile
    check MC_EnvFiles_Red_NoReload                     RED OpenFindsFile
    # An in-memory file's extent as a chain of segments (mem_file/extent.rs):
    # growth links a segment before it publishes the length, the room is
    # zeroed where a write exposes it, a read holds its extent across the
    # copy, and a replaced large extent is submitted to the reclaimer after
    # the writer lets go. Red_NoSubmit breaks LargeFreed, a property of whole
    # runs. Lean: Regolith/MemFileExtents.lean, pieces_exact (a range cut
    # into pieces inside their segments, no gap or overlap, for every chain)
    # and grow_doubling; loom: `just loom-mem-file`.
    spec=MemFileExtents
    check MC_MemFileExtents_Green_Appends              GREEN
    check MC_MemFileExtents_Green_Extend               GREEN
    check MC_MemFileExtents_Green_Shrink               GREEN
    check MC_MemFileExtents_Green_Overwrite            GREEN
    check MC_MemFileExtents_Green_Reclaim              GREEN
    check MC_MemFileExtents_Red_NoZeroExtend           RED CurrentIsFile
    check MC_MemFileExtents_Red_NoZeroGap              RED CurrentIsFile
    check MC_MemFileExtents_Red_PublishFirst           RED CurrentIsFile
    check MC_MemFileExtents_Red_OverwriteInPlace       RED CurrentIsFile
    check MC_MemFileExtents_Red_ReadsRoom              RED ReadIsSnapshot
    check MC_MemFileExtents_Red_ReadAfterRelease       RED NoUseAfterFree
    check MC_MemFileExtents_Red_CapAfterDrop           RED NoUseAfterFree
    check MC_MemFileExtents_Red_NoSubmit               RED LargeFreed
    check MC_MemFileExtents_Green_ReachesGrowthRead    RED NeverReadsGrowing
    check MC_MemFileExtents_Green_ReachesRetiredRead   RED NeverReadsRetired
    # MemEnv's rename racing other changes to the namespace: the rename marks
    # its source before it puts the file at the destination, so a removal or
    # a rename that moves the destination on cannot make it undo itself.
    # Green_Moved is the witness that the GREEN run reaches that race.
    spec=EnvRename
    check MC_EnvRename_Green                           GREEN
    check MC_EnvRename_Green_FourCalls                 GREEN
    check MC_EnvRename_Green_Moved                     RED NeverMovedOn
    check MC_EnvRename_Red_Rollback                    RED Linearizable
    check MC_EnvRename_Red_RemoveIgnoresMark           RED Linearizable
    # OPFS mirror mode's persist (E20, and #289's lock, keep-alive and
    # removal check): a batch takes the lock before it captures and holds
    # it across every browser await; a caller that drops its future drops
    # neither the batch nor the lock; a removal that fails with anything but
    # NotFoundError fails the call and its name stays owed; a batch writes
    # its tables, then the MANIFEST, then makes its removals, and only once
    # every write went through. Green_ReachesWaitBehindOlder is the witness
    # that a call waits while an older MANIFEST is being written. Lean:
    # Regolith/MirrorPersist.lean, serial_never_older, serial_monotone,
    # failed_removal_stays_owed, all_gone_clears, cut_short_keeps_tables,
    # cut_short_keeps_writes, batch_ordered, and the REDs
    # overlap_stores_older, swallow_loses_failed, strict_owed_forever,
    # manifest_first_loses_table and removal_first_loses_manifest.
    spec=MirrorPersist
    check MC_MirrorPersist_Green                       GREEN
    check MC_MirrorPersist_Red_NoLock                  RED INV_ManifestMonotone
    check MC_MirrorPersist_Red_NoLock_Clean            RED INV_CleanIsStored
    check MC_MirrorPersist_Red_CaptureFirst            RED INV_ManifestMonotone
    check MC_MirrorPersist_Red_NoKeepAlive             RED INV_SerialBatches
    check MC_MirrorPersist_Red_NoKeepAlive_Manifest    RED INV_ManifestMonotone
    check MC_MirrorPersist_Red_SwallowRemoval          RED INV_RemovalOwed
    check MC_MirrorPersist_Red_NotFoundFails           RED LIVE_CatchesUp
    check MC_MirrorPersist_Red_ManifestFirst           RED INV_ManifestNamesStored
    check MC_MirrorPersist_Red_DeleteFirst             RED INV_ManifestNamesStored
    check MC_MirrorPersist_Red_DeleteAfterFailedWrite  RED INV_ManifestNamesStored
    check MC_MirrorPersist_Red_NoRetry                 RED LIVE_CatchesUp
    check MC_MirrorPersist_Green_ReachesWaitBehindOlder RED NeverWaitsBehindOlder
    # The pessimistic lock table (plan 4.6, 4.10): one holder per key, no
    # lost wake-up, bounded bypass, a wait delivered to its own queue, and a
    # lock released when its commit lands. Lean: Regolith/LockTable.lean,
    # mutual_exclusion, retired_unused, no_stranded_front, handed_is_told,
    # bounded_bypass, note_on_own_queue, ready_by_own_queue,
    # isolation_at_landing.
    spec=LockTable
    check MC_LockTable_Green_Try                       GREEN
    check MC_LockTable_Green_Bound2                    GREEN
    check MC_LockTable_Green_Blocking                  GREEN
    check MC_LockTable_Green_Nowait                    GREEN
    check MC_LockTable_Green_Cancel                    GREEN
    check MC_LockTable_Green_Table                     GREEN
    check MC_LockTable_Green_Mixed                     GREEN
    check MC_LockTable_Green_MixedLive                 GREEN
    check MC_LockTable_Green_Four                      GREEN
    check MC_LockTable_Red_RetireNoCas                 RED MutualExclusion
    check MC_LockTable_Red_NoRecheck                   RED NoStrandedFront
    check MC_LockTable_Red_ReleaseNoWake               RED NoStrandedFront
    check MC_LockTable_Red_CancelNoPassOn              RED NoStrandedFront
    check MC_LockTable_Red_NoteAfterPoll               RED WokenIsTold
    check MC_LockTable_Red_UnboundedBarging            RED BoundedBypass
    check MC_LockTable_Red_WrongQueue                  RED OwnQueue
    check MC_LockTable_Red_CompleteOnWaker             RED OwnQueue
    check MC_LockTable_Red_ReleaseAtWrite              RED IsolationAtLanding
    check MC_LockTable_Red_ReleaseAtDelivery           RED HeldOnlyWhileNeeded
    check MC_LockTable_Green_EarlyPoll                 GREEN
    check MC_LockTable_Red_MineBeforeGrant             RED MutualExclusion
    spec=LockTableCalls
    check MC_LockTableCalls_Green                      GREEN
    check MC_LockTableCalls_Red_NewestShadows          RED OneQueuedWait
    check MC_LockTableCalls_Red_NoHandBack             RED NobodyStranded
    wait
    rm -rf ./*_TTrace_*.tla ./*_TTrace_*.bin
    cat "$results"/*.out
    failed=$(find "$results" -name '*.fail' | wc -l)
    echo "  $row rows, $failed with the wrong verdict"
    [ "$failed" -eq 0 ]

# `proofs/lean` is a Lake project named Regolith, plain Lean 4 core with no
# dependencies, so it builds offline. Each module proves for every size what
# a TLA+ model above checks for a few, and names that model in its header.
# The build treats every warning as an error, so a proof left open fails it;
# the grep refuses the placeholder words outright; and `Audit.lean` fails
# unless every Regolith declaration rests on Lean's three standard
# assumptions alone. elan provides the toolchain `lean-toolchain` pins.
# Build and audit the Lean proofs in proofs/lean.
lean:
    #!/usr/bin/env bash
    set -euo pipefail
    export PATH="$HOME/.elan/bin:$PATH"
    cd proofs/lean
    lake build
    if grep -rnwE --include='*.lean' --exclude-dir=.lake 'sorry|admit|axiom' .; then
        echo "error: proofs/lean holds sorry, admit or axiom; every proof must be complete" >&2
        exit 1
    fi
    lake env lean Audit.lean

# Every line of a TLA+ model, a TLC configuration and a Lean proof carries a
# plain-words comment directly above it, written for a reader who knows
# neither the language nor the code; `proofs/check-comments.sh` states the
# rule and fails on any line without one. No file is left out.
# Fail when a proof line has no comment above it.
proofs-comments:
    proofs/check-comments.sh

# Every formal check: the TLA+ models, the Lean proofs, and their comments.
proofs: tla lean proofs-comments

# ---------- consistency ----------

# Where the Elle recipes write histories and scratch databases, and which
# elle-cli jar they run. Both are absolute paths. The output goes under the
# harness's gitignored target/ rather than /tmp, which can be full or absent;
# set ELLE_OUT to move it. The jar defaults to where CI downloads it; set
# ELLE_CLI to run one kept elsewhere.
elle_out := env_var_or_default("ELLE_OUT", justfile_directory() / "harness/elle/target/elle-out")
elle_cli := env_var_or_default("ELLE_CLI", justfile_directory() / "harness/elle/elle-cli.jar")

# Elle consistency checking. `model` is the workload (list-append or
# rw-register); `level` is the consistency model to check it against.
#
# The two are separate axes and elle-cli spells both `--model`-ish, which
# is easy to get wrong: passing an isolation level as --model throws
# "No matching clause". Hence the explicit --consistency-models here.
elle model="list-append" level="snapshot-isolation" isolation="snapshot-isolation":
    mkdir -p "{{elle_out}}"
    cargo run --release --manifest-path harness/elle/Cargo.toml --bin elle-gen -- \
        --model {{model}} --isolation {{isolation}} \
        --threads 8 --txns 50 --keys 4 \
        --out "{{elle_out}}/regolith-history.json" --dir "{{elle_out}}/regolith-elle-db"
    java -jar "{{elle_cli}}" --model {{model}} --cycle-search-timeout 60000 \
        --consistency-models {{level}} "{{elle_out}}/regolith-history.json"

# The same, with the fault injection the harness supports.
elle-fault model="list-append" level="snapshot-isolation":
    mkdir -p "{{elle_out}}"
    cargo run --release --manifest-path harness/elle/Cargo.toml --bin elle-gen -- \
        --model {{model}} --isolation snapshot-isolation --faults all \
        --threads 8 --txns 50 --keys 4 \
        --out "{{elle_out}}/regolith-history-fault.json" --dir "{{elle_out}}/regolith-elle-fault-db"
    java -jar "{{elle_cli}}" --model {{model}} --cycle-search-timeout 60000 \
        --consistency-models {{level}} "{{elle_out}}/regolith-history-fault.json"

# Every level regolith claims, checked in one go.
#
# Each line prints elle-cli's verdict and whether elle-gen's built-in check
# passed; anything other than `true` with a passing built-in check fails the
# recipe. `:unknown` means elle-cli gave up a cycle search, so the per-SCC
# timeout below is raised to keep a slow runner from reporting a valid
# history as `:unknown`.
elle-matrix:
    #!/usr/bin/env bash
    set -uo pipefail
    out="{{elle_out}}"
    mkdir -p "$out"
    cd harness/elle
    fail=0
    cargo build --release --bin elle-gen
    # The harness is a workspace of its own, which the nextest profile does
    # not reach: its 16 unit tests measured at 2.4 s with TMPDIR on an nvme
    # disk at load 276 on 36 threads (0.4 s on tmpfs), times 25.
    cargo test --release --no-run || fail=1
    just bounded 1m elle-matrix cargo test --release || fail=1
    check() {
        local name="$1" model="$2" level="$3"; shift 3
        local built_in=pass
        if ! ./target/release/elle-gen --model "$model" "$@" \
            --out "$out/elle-$name.json" --dir "$out/elle-db-$name" >/dev/null; then
            built_in=fail
            fail=1
        fi
        # elle-cli prints "<path>\t<true|false|:unknown>"; take the last
        # field and strip surrounding whitespace, so a stray tab cannot read
        # as a failure on a history that actually passed.
        local v
        v=$(java -jar "{{elle_cli}}" --model "$model" --cycle-search-timeout 60000 \
            --consistency-models "$level" "$out/elle-$name.json" \
            | tail -1 | awk '{print $NF}')
        printf '  %-42s %s (built-in check: %s)\n' "$name [$level]" "$v" "$built_in"
        if [ "$v" != "true" ]; then fail=1; fi
    }
    # Optimistic transactions at snapshot isolation, checked at snapshot
    # isolation: a false here is a defect.
    check optimistic-si       list-append snapshot-isolation --isolation snapshot-isolation --threads 8 --txns 50 --keys 4 --seed 4
    check optimistic-rw       rw-register snapshot-isolation --isolation snapshot-isolation --threads 8 --txns 50 --keys 4 --seed 5
    # Pessimistic transactions are checked at the level they request: these
    # rows run regolith's ReadCommitted level. Their plans include a read-only
    # get_for_update, the one read that level leaves unvalidated and
    # SnapshotIsolation validates.
    check pessimistic-rc      list-append read-committed     --isolation read-committed --threads 8 --txns 50 --keys 4 --seed 2
    # The same level over registers: a lock that times out aborts its
    # transaction, and a value an aborted transaction wrote must never be read.
    check pessimistic-rc-rw   rw-register read-committed     --isolation read-committed --threads 8 --txns 50 --keys 4 --seed 3
    check pessimistic-hotkey  list-append read-committed     --isolation read-committed --threads 8 --txns 50 --keys 1 --seed 1
    # Serializable validates the whole read set, so the strongest model
    # Elle offers must hold.
    check serializable        list-append strict-serializable --isolation serializable --threads 8 --txns 60 --keys 4 --seed 11
    check serializable-rw     rw-register strict-serializable --isolation serializable --threads 8 --txns 60 --keys 4 --seed 12
    # RepeatableRead validates every point read: Adya's PL-2.99, which is
    # what Elle's repeatable-read model checks. These workloads read through
    # `get` only, so the same histories also hold the strongest model.
    check repeatable-read     list-append repeatable-read    --isolation repeatable-read --threads 8 --txns 60 --keys 4 --seed 13
    check repeatable-read-rw  rw-register repeatable-read    --isolation repeatable-read --threads 8 --txns 60 --keys 4 --seed 14
    check repeatable-read-ss  list-append strict-serializable --isolation repeatable-read --threads 8 --txns 60 --keys 4 --seed 15
    # DefraLevel without a classifier validates point reads as RepeatableRead
    # and lets blind merges to one key all commit; the TLA+ model (proofs/tla,
    # MC_DefraLevel_*) proves those relaxations. list-append appends the
    # transaction has not read are merge operands, applied in commit order, so
    # a point workload is serializable in commit order and the strongest model
    # must hold. There is no snapshot-isolation row: two blind merges to one
    # key both commit, which first-committer-wins forbids.
    check defra-level         list-append strict-serializable --isolation defra-level --threads 8 --txns 60 --keys 4 --seed 16
    check defra-level-rw      rw-register strict-serializable --isolation defra-level --threads 8 --txns 60 --keys 4 --seed 17
    # DefraLevel with a key classifier (`--shape`, README.md). In the
    # `classified` shape every transaction also stores a content-addressed
    # block it read as missing, adds a key to a commutative prefix it scanned
    # and appends to a log. The classes say none of that conflicts, and none
    # of it may loosen the point operations beside it: the strongest model
    # must hold, and elle-gen itself fails the row when a commit left a head
    # without its log entry.
    check defra-classified    list-append strict-serializable --isolation defra-level --shape classified --threads 8 --txns 60 --keys 4 --seed 18
    check defra-classified-rw rw-register strict-serializable --isolation defra-level --shape classified --threads 8 --txns 60 --keys 4 --seed 19
    # Commit-ordered appends: a key is a log, an append is
    # `Transaction::append` and a read is the log in one snapshot. An append
    # is never validated at any level and positions follow commit order, so
    # every level must hold the strongest model, the pessimistic flavour too.
    check defra-appends       list-append strict-serializable --isolation defra-level --shape appends --threads 8 --txns 60 --keys 4 --seed 20
    check si-appends          list-append strict-serializable --isolation snapshot-isolation --shape appends --threads 8 --txns 60 --keys 4 --seed 21
    check pessimistic-appends list-append strict-serializable --isolation read-committed --shape appends --threads 8 --txns 60 --keys 4 --seed 22
    # Projected reads: a key is one part of a document, an append is an
    # operand on the part and a read is `get_parts` naming it. Two keys share
    # a document, so a reader of one part commits beside an append to the
    # other, and is refused by an append to its own.
    check defra-parts         list-append strict-serializable --isolation defra-level --shape parts --threads 8 --txns 60 --keys 4 --seed 23
    # A prefix scan: every read is one validated scan of all the keys,
    # recorded as a read of each. Scan-then-write is the shape of write skew,
    # which a validated scan refuses at every level. The rw-register row has
    # keys enough that most writes create one: a key that appears under a
    # scan, which only a validated range sees.
    check defra-scan          list-append strict-serializable --isolation defra-level --shape scan --threads 8 --txns 60 --keys 4 --seed 24
    check defra-scan-phantoms rw-register strict-serializable --isolation defra-level --shape scan --threads 8 --txns 60 --keys 64 --seed 25
    check repeatable-read-scan list-append strict-serializable --isolation repeatable-read --shape scan --threads 8 --txns 60 --keys 4 --seed 26
    exit $fail

# ---------- portability ----------

# Full wasm32-wasip1 lifecycle under wasmtime. Non-zero on the first wrong byte.

wasm records="5000" sustained="20000":
    #!/usr/bin/env bash
    set -euo pipefail
    # open, put, get, delete, batch, scan, snapshot, iterate, compact,
    # close, REOPEN, read back - then `--sustained` writes past the L0
    # stop trigger with a 32 KiB memtable and no explicit compaction,
    # which is the case that wedges when nothing compacts on the
    # calling thread.
    cargo build --release -p regolith-wasm-probe --target wasm32-wasip1
    # Both shipped profiles a wasm module can open with, on the real
    # target: `embedded` runs with no block cache, `wasm` with one.
    for profile in embedded wasm; do
        echo "== profile $profile =="
        d=$(mktemp -d)
        wasmtime run --dir="$d::/data" \
            target/wasm32-wasip1/release/regolith-wasm-probe.wasm -- \
            --profile "$profile" --records {{records}} --sustained {{sustained}} \
            --probe-host --report-memory
        rm -rf "$d"
    done

# The same lifecycle natively, to tell a regolith bug apart from a wasm one.

wasm-native records="5000" sustained="20000":
    #!/usr/bin/env bash
    set -euo pipefail
    for profile in embedded wasm; do
        echo "== profile $profile =="
        cargo run --release -p regolith-wasm-probe --bin regolith-wasm-probe -- \
            --profile "$profile" --records {{records}} --sustained {{sustained}} \
            --probe-host --report-memory
    done

# The OPFS contract against a real browser. `wasm-pack test` cannot
# drive these: it appends `--tests`, which builds every target in
# `tests/`, and all but the four `wasm_opfs*` files are native-only.
# The runner is named per target instead, so only the named test
# binaries are built.
#
# Needs `wasm-bindgen-test-runner` from the `wasm-bindgen-cli` release that
# Cargo.lock pins for `wasm-bindgen`, to the patch: the two share an
# unstable schema, and a runner of another version refuses the test binary
# with a schema error after the whole build. The recipe checks that first
# and says what to install.
wasm-browser:
    #!/usr/bin/env bash
    set -euo pipefail
    pinned=$(awk '/^name = "wasm-bindgen"$/ { found = 1; next } found && /^version = / { gsub(/"/, "", $3); print $3; exit }' Cargo.lock)
    if ! command -v wasm-bindgen-test-runner >/dev/null 2>&1; then
        echo "error: wasm-bindgen-test-runner is not on PATH; install it with: cargo install wasm-bindgen-cli --version $pinned --locked" >&2
        exit 1
    fi
    have=$(wasm-bindgen --version 2>/dev/null | awk '{ print $2 }')
    if [ "$have" != "$pinned" ]; then
        echo "error: wasm-bindgen-cli is ${have:-not installed} but Cargo.lock pins wasm-bindgen $pinned, and the two must match exactly; install it with: cargo install wasm-bindgen-cli --version $pinned --locked" >&2
        exit 1
    fi
    export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner
    # wasm-bindgen's default per-test budget is 20s, which a headless
    # browser on a shared runner can exceed on a test that mounts OPFS
    # and writes through real sync access handles. Exceeding it kills
    # the whole driver, so nine passing tests report as one failure with
    # no attribution.
    export WASM_BINDGEN_TEST_TIMEOUT=180
    # That budget covers the tests, not a driver or a browser that never
    # starts, so each suite is built first and then run under a hard limit
    # of twice the budget: the runner's own message comes first.
    for suite in wasm_opfs wasm_opfs_main wasm_opfs_memory wasm_opfs_transactions; do
        echo "== $suite =="
        cargo test --target wasm32-unknown-unknown --test "$suite" --no-run
        just bounded 6m wasm-browser cargo test --target wasm32-unknown-unknown --test "$suite"
    done

embedded:
    cargo bench --bench memory -- --profile embedded

# ---------- the gains figures ----------

# Collect every family into ONE run file and re-render. A PR runs this, then pastes.

gains: loadguard
    #!/usr/bin/env bash
    set -euo pipefail
    sha=$(git rev-parse --short HEAD)
    cargo bench -- --baseline pre --save-baseline "$sha"
    cargo bench --bench collect -- \
        --out {{gains_dir}}/runs/"$sha"-{{label}}.json \
        --commit "$sha" --label {{label}}
    just gains-render

gains-render:
    {{py}} {{gains_dir}}/render.py
    {{py}} {{gains_dir}}/render_rss.py

gains-diff base current:
    {{py}} {{gains_dir}}/render.py --baseline {{base}} --current {{current}}
    {{py}} {{gains_dir}}/render_rss.py --baseline {{base}} --current {{current}}

gains-list:
    {{py}} {{gains_dir}}/runs.py --list
