<h1 align="center">
    <img src="https://github.com/sourcenetwork/regolith/raw/main/art/regolith_banner_2048x512.png"/>
</h1>
<div align="center">
 <strong>
   ACID, performance oriented, embedded key-value database engine for edge systems
 </strong>
<hr>

[![Crates.io](https://img.shields.io/crates/v/regolith.svg)](https://crates.io/crates/regolith)
[![Documentation](https://docs.rs/regolith/badge.svg)](https://docs.rs/regolith)

[![CI](https://github.com/sourcenetwork/regolith/actions/workflows/ci.yml/badge.svg)](https://github.com/sourcenetwork/regolith/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/sourcenetwork/regolith/branch/main/graph/badge.svg)](https://codecov.io/gh/sourcenetwork/regolith)
[![Transactional Verification](https://github.com/sourcenetwork/regolith/actions/workflows/transactional-verification.yml/badge.svg)](https://github.com/sourcenetwork/regolith/actions/workflows/transactional-verification.yml)
[![Durability](https://github.com/sourcenetwork/regolith/actions/workflows/durability.yml/badge.svg)](https://github.com/sourcenetwork/regolith/actions/workflows/durability.yml)
[![Concurrency Model Checking](https://github.com/sourcenetwork/regolith/actions/workflows/model-checking.yml/badge.svg)](https://github.com/sourcenetwork/regolith/actions/workflows/model-checking.yml)
[![Chaos](https://github.com/sourcenetwork/regolith/actions/workflows/chaos.yml/badge.svg)](https://github.com/sourcenetwork/regolith/actions/workflows/chaos.yml)
[![WASM](https://github.com/sourcenetwork/regolith/actions/workflows/wasm.yml/badge.svg)](https://github.com/sourcenetwork/regolith/actions/workflows/wasm.yml)
[![Embedded](https://github.com/sourcenetwork/regolith/actions/workflows/embedded.yml/badge.svg)](https://github.com/sourcenetwork/regolith/actions/workflows/embedded.yml)

</div>

> **Pre-1.0.** The public surface is small and stable-shaped, but breakage is allowed until 1.0.

## Install

```toml
[dependencies]
regolith = "0.2"
```

## Quick start

```rust
use regolith::{Db, Options, WriteBatch};

let db = Db::open("/tmp/my_db", Options::default())?;

db.put(b"hello", b"world")?;
assert_eq!(db.get(b"hello")?.as_deref(), Some(&b"world"[..]));
db.delete(b"hello")?;

// A batch applies atomically: all of it, or none of it.
let mut batch = WriteBatch::new();
batch.put(b"a", b"1");
batch.put(b"b", b"2");
db.write(batch)?;

// A snapshot is a point in time. Later writes are invisible to it.
let snap = db.snapshot();
db.put(b"a", b"changed")?;
assert_eq!(snap.get(b"a")?.as_deref(), Some(&b"1"[..]));

for (key, value) in db.scan(Some(b"a"), Some(b"z"))? {
    println!("{key:?} = {value:?}");
}
```

Reads that should not copy use `get_slice`, which borrows the bytes the database already
holds:

```rust
if let Some(slice) = db.get_slice(b"a")? {
    assert_eq!(&*slice, b"1");
}
```

A `DbSlice` also adopts a buffer you already own, without copying it, so bytes the
database handed back and bytes you assembled yourself travel as one type:

```rust
let assembled: DbSlice = b"built elsewhere".to_vec().into();
```

## Streaming

A cursor is lazy and iterates as an ordinary Rust iterator, so a scan holds one
entry rather than the whole range. Values come back as `DbSlice`, so iterating
copies keys but never value bytes:

```rust
let snap = db.snapshot();
for (key, value) in snap.owned_iter() {
    // `value` borrows the bytes the database already holds.
    println!("{} = {} bytes", String::from_utf8_lossy(&key), value.len());
}

// Positioned scans, reverse scans, and early exit all work:
let mut cursor = snap.owned_iter();
cursor.seek(b"user:");
let first_ten: Vec<_> = cursor.entries().take(10).collect();
```

`Db::scan` returns a `Vec` and reads the whole range up front. Prefer a cursor,
or `scan_page` for explicit page-sized reads.

regolith stays synchronous and needs no async runtime, so it does not implement
`Stream`. Building one is a one-liner where the async context already is:

```rust
let stream = futures::stream::iter(db.snapshot().owned_iter());
```

Writes stream too. A `WriteBatch` costs memory proportional to its input, which
is wrong for a stream whose length the caller does not control. A
`StreamingWriter` costs a fixed budget instead:

```rust
use regolith::StreamOptions;

let mut writer = db.streaming_writer(StreamOptions {
    max_buffered_bytes: 1 << 20,
    ..Default::default()
});
for (key, value) in huge_source {
    writer.put_owned(&key, value)?;  // takes the buffer, does not copy it
}
let sequence = writer.finish()?;
```

Peak footprint is the budget plus one operation, whatever the stream's length.
The tradeoff is explicit: each flush is atomic, the stream as a whole is not.
A crash partway through leaves a valid prefix of the stream, never a
half-applied flush. Work that must land all-or-nothing wants a single
`WriteBatch` and has to pay the memory for it.

## Reads that never block

A thread that must never wait on the disk reads through a `CacheOnly` handle.
Such a read never touches the device: when it needs a block the cache does not
hold, it returns `Error::WouldBlock` at once and records the block on the
thread's own I/O queue. The thread polls that queue when it has nothing else
to run, which reads the blocks it waits on, and then runs the read again:

```rust
use regolith::{Error, IoBudget, ReadMode, WouldBlock};

let mut queue = db.io_queue(); // one per thread
let snap = db.snapshot().with_read_mode(ReadMode::CacheOnly(queue.id()));
let value = loop {
    match snap.get(b"key") {
        Err(Error::WouldBlock(WouldBlock::Io(wait))) => {
            queue.poll(IoBudget::ALL); // or `wait.await` while other tasks run
        }
        other => break other?,
    }
};
```

A block several threads miss is read once, by whichever thread's poll claims
it first, and every waiting queue is told. A completion only ever reaches the
queue that asked, and `IoQueue::idle_waker` wakes an idle thread when one
arrives, never a busy one. Iterators, transaction cursors and streams pick up
exactly where a wait stopped them. Transactions take the mode from
`TxnOptions::read_mode`.

## Commits that never block

`Transaction::commit_nowait` hands back a `CommitTicket` at once. The ticket is a
`Future` of the commit's outcome, completed only at a poll of the queue named by
`TxnOptions::io_queue`, on the thread that owns it; with no queue the commit syncs
inline and the ticket is ready already. Commits from many threads still share one
log write and one fsync per group: the fsync is a unit that the first member thread
to poll claims and runs, and it tells every member's queue. At
`DurabilityMode::Immediate` a commit is visible only once that fsync returned.
A commit made through a `CacheOnly` handle validates from the cache on the calling
thread too; a block its validation misses is read at a poll of its queue, and the
commit goes on there. A miss never ends it: the transaction cannot commit twice.

```rust
use regolith::{OptimisticTransactionDb, Options, TxnOptions};

let db = OptimisticTransactionDb::open("/tmp/nowait_db", Options::default())?;
let mut queue = db.db().io_queue(); // one per thread
let txn = db.begin(&TxnOptions::new().io_queue(queue.id()));
txn.put(b"key", b"value")?;
let ticket = txn.commit_nowait();
ticket.on_complete(|outcome| println!("committed: {}", outcome.is_ok()));
let receipt = queue.block_on(ticket)?; // or `ticket.await` while other tasks run
```

The `on_commit`, `on_abort` and `on_complete` callbacks run exactly once, on the
thread that delivers the outcome. Dropping a ticket cancels nothing; dropping its
queue delivers what the queue holds on the dropping thread. `transact_async` is the
retrying form: its attempts read through a `CacheOnly` handle, a miss suspends the
attempt instead of ending it (`ready(|| txn.get(k)).await`), and it needs no runtime.

No write waits on a write stall. A write a stop stops applies nothing and returns
`Error::WouldBlock(WouldBlock::Stall(wait))`, whose wait completes on the writer's
own queue once background work clears the stall; `through_stalls` waits it out for
a blocking caller. With no compaction worker, `Options::inline_compaction(true)`
makes the stopped writer run the step that clears the stall on its own queue, or
inline with no queue. `compact_range`, `ingest_external_files` and `checkpoint`
return a `JobTicket` the same way, run on the worker or on the caller's queue.

## Transactions

Pick the isolation a unit of work actually needs. The level decides how much of the
transaction's footprint is validated at commit, so a stricter level refuses more and
commits fewer.

```rust
use regolith::{IsolationLevel, OptimisticTransactionDb, Options, TxnOptions};

let db = OptimisticTransactionDb::open("/tmp/txn_db", Options::default())?;

let mut txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
let balance = txn.get(b"account")?.unwrap_or_default();
txn.put(b"account", b"debited")?;
txn.commit()?;
```

| Level | Lost update | Read skew | Write skew |
|---|---|---|---|
| `ReadCommitted` | prevented | possible | possible |
| `SnapshotIsolation` (default) | prevented | prevented | possible |
| `RepeatableRead` | prevented | prevented | through a scan |
| `Serializable` | prevented | prevented | prevented |
| `DefraLevel` | prevented | prevented | through a scan |

At `Serializable`, every key a transaction reads through `get` or a transactional scan is
validated. At `RepeatableRead`, every key a `get` returned is validated and a scanned key is
not: Adya's PL-2.99 over snapshot isolation, with G1, G-SI and G2-item forbidden and an
anti-dependency through a scan allowed. It is the level for a scan over a set that
concurrent writers are allowed to change underneath the transaction, and it pays no commit
work per scanned key. Below `RepeatableRead`, a plain `get` is not validated either. A
written key is validated only in the optimistic flavour; the
pessimistic flavour never validates a written key at commit, since the key lock already
orders it. A `get_for_update` key is validated in both flavours from `SnapshotIsolation`
up. At every level a key a transactional scan walked that the transaction then writes is
validated as a read (at `DefraLevel`, a key in a commutative prefix is validated as a blind
write instead, and a content-addressed key follows the rules below), so a scan-then-write
never loses an update. A range is not validated
on its own: a key inserted into a scanned range by a concurrent transaction (a phantom) is
detected only when the transaction also writes it or validates it as a point read. A scan
at `Serializable` holds one read-set entry per key it yields until the transaction
resolves, and its commit checks each one while every writer waits; below it, a scan holds
one entry per stretch of keys it walked.

`DefraLevel` is `RepeatableRead` with four relaxations, which apply to optimistic
transactions only. A pessimistic transaction at this level validates as `RepeatableRead`.
What the commit validates, by kind of key (the first is `RepeatableRead` itself, the other
four are the relaxations):

- An ordinary key: every point read of it is validated, whether it found a value or not,
  and a put or delete of it conflicts with any commit to the key since the transaction
  began, unless the key already holds what the write would leave.
- A key the transaction only merges into (it did not read the key, did not walk it in a
  scan, and did not put or delete it) conflicts only with a newer put, delete or covering
  range delete of that key, never with a newer merge operand. Concurrent blind merges to
  one key all commit, and their operands apply in commit order. This needs no classifier.
- A key in a `KeyClass::CommutativePrefix`, with a `KeyClassifier` installed through
  `OptimisticTransactionDb::with_policy`: a scan stretch that stays inside one such prefix
  is not recorded, and a put or merge made there, before or after the scan, is validated as
  a blind write instead of as a read. Declare a prefix commutative only when nothing the
  transaction writes outside it depends on which keys of the prefix the scan returned, and
  its writes inside it are unique keys or identical rewrites.
- A key declared `KeyClass::ContentAddressed`, with the same classifier: a put or merge of
  it is never validated, nor is a read of a key the transaction puts or merges, so two
  transactions that create the same key both commit. A delete of it is validated like a
  delete of an ordinary key. A point read that found it is validated for presence only: it
  conflicts at commit when the key is gone, and not when a newer put or merge left it
  there. A read that found nothing is validated in full, so a newer put conflicts. The
  caller's contract is that the key determines its bytes, as a content hash does; regolith
  cannot check it, and a transaction that writes other bytes under the key commits too, the
  last commit to land winning.
- A key declared `KeyClass::Log`, with the same classifier: a read of it is never validated,
  and a put, delete or merge of it is refused with `Error::LogKeyWrite`. Only
  `Transaction::append` writes the keys of a commit-ordered log, and
  `Transaction::truncate_log` deletes its entry keys.

`Transaction::append(&log, entry, once_key)` adds an entry to a commit-ordered log, whose
keys a `LogLayout` names. At commit, in the ordered step after validation, the entry takes
the log's next position, dense from 1, and the entry, its once key and the head key are
written in the same atomic commit. Positions follow commit order, a transaction that
aborts takes none, an append is never validated so it never conflicts (at every level, for
both flavours), and an append whose once key already holds a position writes nothing.
`Transaction::truncate_log(&log, lo..=hi)` drops the entries at those positions in the same
atomic commit and leaves the head alone, so positions keep counting from where they were.
It refuses a range past the head the transaction reads, never conflicts (two truncates of
overlapping ranges both commit, and so do a truncate and an append), and a snapshot taken
before it still reads every entry it dropped.
`Db::allocate(key, n)` reserves `n` values of a `u64` counter in the same ordered step. It
is never a conflict, never waits on a write stall, and is logged before any commit that
uses one of its values, so after a crash no value a surviving commit used is returned
again. Values reserved by a transaction that aborts are skipped.

A transaction reads its own puts, deletes and merges in the order it made them, and the
commit applies them in that order: an operand applies to the put or delete before it, and a
put or delete replaces every operand before it. This changes what a commit stores for a
merge made before a put of the same key. Earlier versions applied every merge on top of the
puts, whatever order they were made in; the commit now stores the put alone.

A key that is only merged into does not get snapshot isolation. When every transaction uses
only point reads, puts, deletes and merges, the committed transactions are serializable in
commit order, because every point read is validated; a content-addressed key is outside that
statement. The keys a scan returns are not validated, as at `RepeatableRead`, so phantoms are
possible: a key another transaction inserts into a scanned range is detected only when this
transaction also reads or writes it.

A compaction filter runs in every snapshot stripe, so a filter that changes or removes a
value changes what a live snapshot, a transaction's included, reads from then on. No commit
check sees it.

`TransactionDb` is the pessimistic flavour: it takes key locks, so contention waits
instead of retrying. `OptimisticTransactionDb` validates at commit and retries.

The key locks live in a lock-free table: taking a free lock is one compare-and-swap, and no
call takes a mutex or waits on a condition variable. A free lock goes to whoever asks first,
even while others wait; a waiter passed over `regolith::sync::MAX_BYPASS` times is handed the
lock, so none starves. `get_for_update`, `put`, `delete` and `merge` wait for a lock another
transaction holds, up to `with_lock_timeout`, and then fail with `TransactionError::Busy`,
which is also what ends a deadlock. `try_get_for_update` never waits: it returns
`WouldBlock::Locked` with a `LockWait`, completed on the transaction's own `IoQueue` at
that queue's poll once the lock may be free, and the call is then run again. A
`commit_nowait` releases its locks when the commit lands, not when its ticket is delivered,
so another transaction never waits for this one's owner to poll.

A commit that loses a race returns `TransactionError::Conflict`. Its `Conflict` names the
key, what the transaction did with it (`Access`) and the newer write that won
(`WriteKind`), with the two sequences. Its message gives the key's length and a short hash,
never its bytes, since a key can hold user data. An `EventListener::on_conflict` hears each
conflict once, after the commit released its locks, so a caller can count the reasons.

`transact` runs a closure in a transaction and commits it. After a conflict it runs the
closure again at once, in a fresh transaction, handing it the conflict it lost to, up to the
policy's `max_attempts`. The closure can settle the race from that reason instead of
redoing its work blindly. An error from the closure ends the attempt and is not retried.

```rust
use regolith::{RetryPolicy, TransactionError, WriteKind};

let (_, receipt) = db.transact(&RetryPolicy::default(), |txn, lost| {
    // `lost` is the conflict the previous attempt lost to, `None` on the first run.
    if lost.is_some_and(|conflict| conflict.theirs() == WriteKind::Delete) {
        return Ok(()); // the document was deleted by the writer that won
    }
    txn.get_for_update(b"doc")?;
    txn.delete(b"doc")?;
    Ok::<_, TransactionError>(())
})?;
println!("visible at sequence {}", receipt.seq());
```

`commit` returns a `CommitReceipt` whose `seq()` is the sequence its writes became visible
at, or the snapshot's sequence for a commit that wrote nothing. A sequence orders commits
within one database; do not keep it across `drop_all` or in a restored database.

Buffering a read or a write takes `&self`, so one transaction can be shared across
threads without a lock around it. The write buffer and the read set are lock-free, and
every read is folded into the commit-time validation set no matter which thread recorded
it. Only `commit`, `rollback`, and the savepoint calls need exclusive access, because
those are the points where the buffer stops changing.

```rust
use std::sync::Arc;
use regolith::{IsolationLevel, OptimisticTransactionDb, Options, TxnOptions};

let db = OptimisticTransactionDb::open("/tmp/shared_db", Options::default())?;

// A `Transaction` has no lifetime and borrows nothing from the database, so it can be
// boxed, shared, and moved freely, and it outlives the handle that began it.
let txn = Arc::new(db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable)));

std::thread::scope(|scope| {
    for thread in 0..4 {
        let txn = txn.clone();
        scope.spawn(move || txn.put(format!("k{thread}").as_bytes(), b"v"));
    }
});

txn.into_inner().expect("threads joined").commit()?;
```

Commits report the sequence they landed at, so an upper layer can order its own versions
against the store without a lock of its own:

```rust
let seq = db.write_sequenced(batch)?;
assert!(db.snapshot().sequence() >= seq);
```

## Durability

`DurabilityMode::Eventual` (default) hands every write to the kernel but does not fsync: a
committed write survives a process crash, not a power loss. `DurabilityMode::Immediate`
fsyncs, and every write that returned `Ok` survives a power cut.

Either way, recovery reaches a **valid prefix** of the write history: some number of
writes applied, in order, no gaps, no half-applied batch. A `WriteBatch` is atomic under
both modes.

Every log record carries the offset the last completed sync made durable. A power cut can
leave the end of the newest log torn, zeroed or garbled; recovery drops what no sync
covered, truncates it, and reports it (a warn line, the `regolith.wal.tail_discarded`
tickers and `EventListener::on_wal_tail_discarded`). Damage to bytes a surviving record
proves were synced refuses the open instead. Rot inside the last synced group of a crashed
log cannot be told from a torn write, so it is dropped the same way: reported, never
silent.

regolith 0.2 opens a database written by 0.1.x, and the open rewrites its log in format 2,
which 0.1.x refuses to open: the upgrade from 0.1.x to 0.2 is one-way. A database a 0.1.x
release left with tables of overlapping key ranges in one level opens on 0.2 too: the first
open moves that level and the
levels above it into level 0 and logs one warning (the `regolith.manifest.tables_demoted`
ticker and property count the tables moved). A database a 0.1.x release left with a range
delete below older keys it deletes reads them as deleted: the first open copies every range
delete that may sit behind such a key in front of every key, logs one warning, and records
the check so no later open repeats it (`regolith.manifest.range_deletes_copied` counts the
copies).

## Encryption at rest

```rust,ignore
let opts = Options::default().key_provider(Arc::new(MyKeys));
```

With a `KeyProvider` installed, every table block, log record and manifest batch is sealed
with AES-256-GCM-SIV under the provider's current key, and its tag takes the place of the
checksum: a random-access read still decrypts one block, and a frame whose tag fails is
treated exactly as a failed checksum is, the torn-tail rule of the log included. Values are
plaintext inside the engine, so merges, byte-equality checks, value-validated reads,
content-addressed keys, appends and allocations all work unchanged.

Each file names the key it was sealed under, so a rotated key keeps old data readable for
as long as the provider still provides it. An encrypted database opened without a provider,
or with a provider missing a key some file names, refuses to open. An unencrypted database
opens with a provider and is sealed as compaction rewrites it; `Db::compact_range` on an
encrypted database rewrites everything not sealed under the current key at once, which is
how a key is retired.

## Platforms

| Target | Status |
|---|---|
| Linux, macOS (x86_64, aarch64) | full |
| `wasm32-wasip1` | full, via a preopened directory |
| `wasm32-unknown-unknown` | full, via OPFS (`Options::wasm()`) |
| Embedded Linux (Cortex-A, ESP32-S3) | `Options::embedded()`, ~1-4 MiB working set |

Pure Rust throughout: no C toolchain, no FFI, no linker surprises. Compaction runs on an
ordinary OS thread; no async runtime is required. On a target without threads, set
`max_background_compactions(0)` and compaction runs on the calling thread.

## Configuration

```rust
use regolith::{CompressionType, Options};

let opts = Options::default()
    .write_buffer_size(64 * 1024 * 1024)
    .block_cache_size(512 * 1024 * 1024)
    .compression(CompressionType::Lz4);
```

Every option is a builder method named after it, so a new option in a later release is a
new method and an existing chain keeps compiling.

Three ready-made profiles: `Options::default()` for a server, `Options::embedded()` for a
1-4 MiB budget, and `Options::wasm()` for a browser or wasi module. Every value and the
reasoning behind it is documented on the profile itself.

Two knobs deserve a warning:

- **Value size.** `max_value_size` defaults to 64 MiB. Values are stored inline, with no
  key-value separation, so a large value is rewritten in full by every compaction that
  touches its key. A 1 GiB value peaked at 3719 MiB RSS.
- **Back-pressure under FIFO and universal compaction.** `level0_stop_writes_trigger`
  counts L0 files, and only `CompactionStyle::Level` reduces that count. Under the other
  styles the trigger can be reached and never relieved, and writes then fail with
  `Error::Busy` naming the knob. Set both L0 triggers to `0` there and bound memory with
  `max_write_buffer_number` and `hard_pending_compaction_bytes_limit`.

## How it works

```text
         writes ──> WAL ──> MemTable ──> reads
                             │ flush
                        ┌────▼─────┐
                        │  L0 SSTs │  may overlap
                        └────┬─────┘
                             │ compaction
                        ┌────▼─────┐
                        │ L1..L6   │  sorted, non-overlapping, 10x each
                        └──────────┘
```

**Write:** append to the WAL, insert into an arena-backed skip-list memtable, and when it
fills, flush to an L0 SSTable. Background compaction merges levels.

**Read:** active memtable, then frozen memtables, then L0 (bloom pre-check), then L1+ by
binary search. First hit wins.

**MVCC:** every write takes a sequence number. A snapshot captures the current one and
ignores anything newer, so reads need no locks.

## Testing

The suite is the argument for trusting any of the above.

```sh
just gate       # format, lint, docs, tests, dependency audit
just test       # the whole suite under cargo-nextest
just loom-all   # exhaustive model checking of the publication protocols
just tla        # TLA+ models: commit validation per isolation level, the storage engine
just lean       # Lean proofs behind the storage-engine models, for every size
just proofs     # both of the above
just elle       # Elle consistency checking of transaction histories
just chaos      # the full-size read-view chaos workload
just wasm       # the wasm32-wasip1 lifecycle under wasmtime
```

`just test` and the other recipes that run under a limit need GNU `timeout` (coreutils).

Each badge above is a workflow of its own, so a red one names exactly which property
regressed.

## License

Dual-licensed under Apache-2.0 or MIT, at your option.

<sub><sup>Here men from the [planet Earth](https://open.spotify.com/track/6qZthmNcaK0jlrkMZ3khmy) first set foot upon the Moon July 1969, A.D. We came in peace for all mankind.<sup><sub>
