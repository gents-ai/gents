# Storage write-gate scheduling contract

## Failure and retained owners

A desktop demo held the canonical write gate in
`admission.persist_existing_call_terminal` while response progress, hydration,
and heartbeats waited. A deterministic negative-control regression reproduces
the Gents-level circular wait without DefraDB:

1. The stream's terminal finalizer acquires the write gate, then awaits storage.
2. The consumer's `select!` chooses its response-flush branch.
3. That branch awaits the same gate and stops polling `stream.next()`.
4. The finalizer cannot resume to release its gate—even when storage is ready.
   Its nested timeout also cannot be polled.

This establishes a polling-topology defect. It does not establish that the
underlying database primitive is non-yielding or cancellation-unsafe. The repair
keeps finalization independently scheduled, awaited before terminal publication,
and owned by the stream so dropping it invokes existing cancellation/repair.
It adds neither a write gate nor a durable lifecycle nor a second recovery owner.

## Model and scope

`Proofs/StorageWriteGate.lean` models one existing gate acquisition. Its phase
distinguishes pending storage, cancellation cleanup, future drop, permission to
release, and actual release. Scheduling is a separate fact: an independently
scheduled finalizer remains pollable while its consumer awaits the gate.

The model proves:

- Shared-task suspension admits an infinite stalled trace, not merely a missing
  happy path. Elapsed time alone does not observe a timeout or free the gate.
- Observing a storage deadline retains gate ownership through cleanup; observing
  a cleanup deadline still requires future drop to return.
- Given independent scheduling **and observed return**, completion, cleanup, or
  completed cancellation has a bounded transition path to release.

These are conditional transition guarantees, not scheduler fairness or
wall-clock progress. No state means "committed": gate release is not evidence
that a timed-out write failed, succeeded, rolled back, or may safely be retried.
Existing transaction and lifecycle owners retain those decisions.

## Conformance

`Proofs/Conformance/StorageWriteGate.lean` emits
`crates/gents/tests/fixtures/storage_write_gate.json`. Its expected gate states
are computed from the executable model, not separately authored in Rust.

`admission/stream_guard/conformance.rs` consumes all three cases:

| Scenario | Implementation exercised | Expected while consumer is not polling |
| --- | --- | --- |
| Shared-task completion suspended | Negative control: directly polled finalizer | Gate remains held despite storage return |
| Independent completion | Real `hold_stream_guard` | Gate released after storage return |
| Elapsed, no storage completion | Real `hold_stream_guard` | Gate still held; no unsafe force-unlock |

The probe substitutes a Tokio mutex and oneshot-controlled storage completion;
it exercises the production stream wrapper and executor polling, **not native
DefraDB execution or its internal locks**. The negative control is test-only.
The consumer waits on the actual held mutex, not on a copied state-machine flag.

`lean_independent_completion_releases_native_canonical_gate` additionally drives
the positive fixture through `ConfigAccess::transact_local` and
`write_local_response` on an embedded node. The finalizer deliberately waits
inside its transaction; the sibling canonical writer then completes without
further stream polls, and both durable rows are read back. This fences the Gents
adapter with actual native writes, while the deliberately delayed callback is
still an injected observation rather than a fault inside DefraDB.

Run the narrow generator and check the committed fixture byte-for-byte:

```sh
cd crates/gents/proofs
lake env lean --run Proofs/Conformance/StorageWriteGate.lean
lake build
```

From the worktree root, run the Rust bridge:

```sh
cargo test -p gents --lib lean_storage_gate_scenarios_drive_stream_finalization
cargo test -p gents --lib lean_independent_completion_releases_native_canonical_gate
cargo test -p gents --lib terminal_persistence_progresses_while_consumer_waits_for_shared_write_gate
```

## Remaining refinement obligations

- Executor service, yielding polls, and cancellation/drop return are external
  assumptions. Spawning the finalizer removes this circular wait, not arbitrary
  runtime starvation or a blocked synchronous destructor.
- Cleanup/drop model paths are not exercised by the three generated scheduling
  cases. Existing transaction cleanup and stream-drop tests must separately
  establish their relevant owner behavior; neither proves Defra internals.
- Native storage contention, response durability, terminal ordering, cancellation,
  and post-restart convergence need integration/live tests in addition to this
  isolated regression. A fresh-home chat completing once is not load assurance.
- A watchdog that persists through the same blocked gate cannot independently
  guarantee recovery. Do not infer operational liveness from an enum transition,
  an existential legal path, or a configured timeout duration.

The proof README already distinguishes existential paths from fair-scheduler
liveness. The enum witnesses formerly called `*_no_deadlocks` are renamed
`*_has_distinct_state`: they establish only that the enum has another value.
