---- MODULE LeaseRenewalStarvation ----
\* lean: none
\* crates: gents-session, gents-loop
\* models: RequestExecutionLease.renewalDue
\* traces: violates
\* One request's execution lease, its owner and its renewal task on the
\* owner's runtime, beside the wall clock (#3704).
\*
\* The owner claims the request (`RequestLifecycle::claim_at`): the claim
\* persists `execution_lease_expires_at = now + Lease` and starts the renewal
\* task (`RenewalTask::start`) on the runtime that polled the claim. The owner
\* then takes a producer action: it publishes provider output
\* (`publish_provider_turn_with_time`, `authorize_output_append`) or begins
\* execution (`begin_owned_execution`, `authorize_begin`). Either is admitted
\* only for the owning generation while the deadline is ahead of the clock
\* (Lean `RequestExecutionLease.expired_rejects_all_producer_actions`), so
\* `Publish` stands for both. Output never renews the lease (Lean
\* `append_output_is_identity_and_never_renews`).
\*
\* The renewal task polls every quarter lease (`renewal_poll_interval`, with
\* missed ticks skipped). A poll renews once half a lease or less remains
\* (`authorize_renewal`, Lean `renewalDue`) and ends the task as lost when it
\* finds the deadline passed (`renew_in_transaction`). Once the deadline has
\* passed, recovery may install a fresh generation (Lean
\* `expired_recovery_enabled`).
\*
\* The owner also runs one synchronous step of its own: work that does not
\* yield to the runtime, for as many ticks as the machine takes. The step
\* found in #3704 is a test's first read of the Lean contract snapshot
\* (`gents_lean_vocab::lean_contract_snapshot`), a process-wide `OnceLock`
\* whose initializer builds and runs the model with `lake`; every other task
\* that reads the snapshot meanwhile blocks on the same lock. On a
\* single-worker runtime (`#[tokio::test]`) the step holds the only thread,
\* so no renewal poll runs until it returns. Production never takes this
\* step: `gents-lean-vocab` is linked only through dev-dependencies and the
\* `test-fixtures` features, which no production dependency enables.
\*
\* Three tests took the `SyncInsideLease` order (#3704, #3891, #3892), so the
\* green order is now the accessor's: async code awaits
\* `gents_lean_vocab::snapshot()`, whose first load runs on Tokio's blocking
\* pool (the `OffRuntime` configuration), and the load stays lazy so only
\* tests that read the model run `lake`. `lease_holders_await_the_lean_snapshot`
\* refuses a synchronous model read after a lease starts without that await,
\* and `lean_generator_runs_stay_off_async_runtime_threads` keeps per-call
\* generator runs off async bodies. A fourth test took the order through a
\* file-local fixture the fence did not know by name (`plugin_job_dispatch`'s
\* `step0`, which claims through `job_chain`), so the fence now also counts
\* every function of a file that reaches a lease start.
\*
\* The lease's promise to its owner is conditional: an owner that holds a
\* live lease keeps it while its runtime can schedule the renewal
\* (`LeaseHeld`). TLC finds the bound on a synchronous step inside the lease:
\* shorter than half a lease it holds (`BoundedSync`, and at `Lease = 8`
\* with `MaxSync = 3`); half a lease begun as the renewal comes due spends
\* the whole margin (`MarginSpent`, and `Lease = 8` with `MaxSync = 4`). A
\* step of unbounded duration on the owner's runtime is outside the
\* contract. The fence keeps the unconditional promise to every other reader
\* (`SingleWriter`) whatever the owner's runtime does (`Fenced`).
\*
\* A host that stops the whole process (memory thrashing, a paused process)
\* is the same step with no worker free, so `SyncOffRuntime` does not apply:
\* a stall of half a lease or more fails a correct owner's request with
\* `execution lease expired` (`MarginSpent`). The lease is not the only such
\* bound: a stall inside a commit also outlasts the store's
\* `EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT`. Both are premises about the
\* host (`WallClockBound` `SlowHost`), not decisions a test can wait out.
\*
\* `Tick` carries the lease's runtime obligation
\* (`gents_loop::execution_policy`: the configured duration "must leave
\* scheduling and commit-latency margin"): the clock does not pass a due poll
\* while a worker is free to run it. The spec adds nothing to the Lean
\* decisions; it interleaves them with the clock and the held thread.
\*
\* `SyncInsideLease` places the synchronous step after the claim (TRUE, the
\* failing test's order) or before it (FALSE). `SyncOffRuntime` runs it off
\* the runtime workers (`spawn_blocking`), the repository rule for
\* synchronous work. `MaxSync` bounds its duration in ticks. `FenceDeadline`
\* is the publication's deadline check.
EXTENDS Naturals

CONSTANTS
  Lease,            \* lease duration in ticks, a multiple of 4
  Horizon,          \* ticks modeled
  MaxSync,          \* longest synchronous step in ticks
  SyncInsideLease,  \* the synchronous step runs between claim and publication
  SyncOffRuntime,   \* the synchronous step runs off the runtime workers
  FenceDeadline     \* publication rejects a passed deadline

ASSUME /\ Lease \in Nat \ {0}
       /\ Lease % 4 = 0
       /\ Horizon \in Nat
       /\ MaxSync \in Nat \ {0}
       /\ SyncInsideLease \in BOOLEAN
       /\ SyncOffRuntime \in BOOLEAN
       /\ FenceDeadline \in BOOLEAN

PollEvery == Lease \div 4
RenewalInterval == Lease \div 2

VARIABLES
  now,       \* wall clock
  pc,        \* owner: "init" | "claimed" | "syncing" | "published"
  ret,       \* owner state the synchronous step returns to
  synced,    \* the synchronous step has run
  syncTicks, \* ticks the running synchronous step has taken
  gen,       \* owning generation: 0 unclaimed, 1 the owner, 2 a recovery
  deadline,  \* execution_lease_expires_at
  claimedAt, \* when the renewal ticker started
  nextPoll,  \* next renewal poll
  renewing,  \* the renewal task has not ended as lost
  outcome,   \* publication: "none" | "accepted" | "rejected"
  live       \* ghost: the lease was the owner's and live at the acceptance

vars == <<now, pc, ret, synced, syncTicks, gen, deadline, claimedAt, nextPoll,
          renewing, outcome, live>>

TypeOK ==
  /\ now \in 0..Horizon
  /\ pc \in {"init", "claimed", "syncing", "published"}
  /\ ret \in {"init", "claimed"}
  /\ synced \in BOOLEAN
  /\ syncTicks \in 0..MaxSync
  /\ gen \in 0..2
  /\ deadline \in 0..(Horizon + Lease)
  /\ claimedAt \in 0..Horizon
  /\ nextPoll \in 0..(Horizon + PollEvery)
  /\ renewing \in BOOLEAN
  /\ outcome \in {"none", "accepted", "rejected"}
  /\ live \in BOOLEAN

Init ==
  /\ now = 0
  /\ pc = "init"
  /\ ret = "init"
  /\ synced = FALSE
  /\ syncTicks = 0
  /\ gen = 0
  /\ deadline = 0
  /\ claimedAt = 0
  /\ nextPoll = 0
  /\ renewing = FALSE
  /\ outcome = "none"
  /\ live = FALSE

\* A runtime worker is free to run the renewal task.
WorkerFree == SyncOffRuntime \/ pc # "syncing"

PollDue == renewing /\ now >= nextPoll

LeaseLive == gen = 1 /\ now < deadline

\* `claim_at`: the lease and its renewal ticker start together.
Claim ==
  /\ pc = "init"
  /\ SyncInsideLease \/ synced
  /\ pc' = "claimed"
  /\ gen' = 1
  /\ deadline' = now + Lease
  /\ claimedAt' = now
  /\ nextPoll' = now + PollEvery
  /\ renewing' = TRUE
  /\ UNCHANGED <<now, ret, synced, syncTicks, outcome, live>>

SyncStart ==
  /\ ~synced
  /\ pc = IF SyncInsideLease THEN "claimed" ELSE "init"
  /\ ret' = pc
  /\ pc' = "syncing"
  /\ syncTicks' = 0
  /\ UNCHANGED <<now, synced, gen, deadline, claimedAt, nextPoll, renewing,
                 outcome, live>>

SyncEnd ==
  /\ pc = "syncing"
  /\ pc' = ret
  /\ synced' = TRUE
  /\ UNCHANGED <<now, ret, syncTicks, gen, deadline, claimedAt, nextPoll,
                 renewing, outcome, live>>

\* `publish_provider_turn_with_time`: the generation always fences, the
\* deadline when `FenceDeadline`.
Publish ==
  /\ pc = "claimed"
  /\ synced
  /\ pc' = "published"
  /\ outcome' = IF gen = 1 /\ (FenceDeadline => now < deadline)
                  THEN "accepted"
                  ELSE "rejected"
  /\ live' = LeaseLive
  /\ UNCHANGED <<now, ret, synced, syncTicks, gen, deadline, claimedAt,
                 nextPoll, renewing>>

\* The ticker skips missed ticks and stays aligned to its start.
NextAligned == claimedAt + (((now - claimedAt) \div PollEvery) + 1) * PollEvery

\* `renew_in_transaction`: lost on a passed deadline or a foreign
\* generation, skipped before the renewal is due, otherwise the deadline
\* moves a full lease ahead of the clock.
Poll ==
  /\ PollDue
  /\ WorkerFree
  /\ IF ~LeaseLive
       THEN /\ renewing' = FALSE
            /\ UNCHANGED <<deadline, nextPoll>>
       ELSE /\ deadline' = IF now + RenewalInterval >= deadline
                             THEN now + Lease
                             ELSE deadline
            /\ nextPoll' = NextAligned
            /\ UNCHANGED renewing
  /\ UNCHANGED <<now, pc, ret, synced, syncTicks, gen, claimedAt, outcome, live>>

\* Recovery of an expired lease installs a fresh generation.
Recover ==
  /\ gen = 1
  /\ now >= deadline
  /\ gen' = 2
  /\ UNCHANGED <<now, pc, ret, synced, syncTicks, deadline, claimedAt,
                 nextPoll, renewing, outcome, live>>

\* The clock does not pass a due poll that a free worker can run, and the
\* synchronous step lasts at most `MaxSync` ticks.
Tick ==
  /\ now < Horizon
  /\ ~(PollDue /\ WorkerFree)
  /\ pc = "syncing" => syncTicks < MaxSync
  /\ now' = now + 1
  /\ syncTicks' = IF pc = "syncing" THEN syncTicks + 1 ELSE syncTicks
  /\ UNCHANGED <<pc, ret, synced, gen, deadline, claimedAt, nextPoll,
                 renewing, outcome, live>>

Next ==
  \/ Claim
  \/ SyncStart
  \/ SyncEnd
  \/ Publish
  \/ Poll
  \/ Recover
  \/ Tick

Spec == Init /\ [][Next]_vars

\* What the lease promises every reader of the request's output: output is
\* accepted only from the owning generation while its lease is live, so a
\* recovery never shares the request with the owner it replaced.
SingleWriter == outcome = "accepted" => live

\* What the failing test assumed: the owner's publication is accepted. The
\* lease grants it only to an owner whose runtime keeps running the renewal.
OwnerPublishes == outcome # "rejected"

\* The owner keeps its lease until it publishes.
LeaseHeld == pc = "claimed" => LeaseLive

====
