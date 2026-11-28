---- MODULE RetryBackoffLease ----
\* lean: RequestExecutionLease.retryWakeDispatches
\* crates: gents, gents-loop, gents-session
\* models: RequestExecutionLease.renewalDue
\* traces: violates
\* One request's execution lease while its owner backs off a failed provider
\* call, and the e2e test `retry_backoff_cannot_renew_an_expired_execution_lease`
\* that stands in for the lease running out.
\*
\* Actors and their atomic steps, as the code has them:
\*  - The owner's provider work (`gents_loop::loop_stream`). Its first call
\*    fails and its `InferenceCall` row is durably `failed`
\*    (`AdmittedCompletionModel::admit`) before the retry decision; the loop
\*    then sleeps the backoff (`sleep_retry_delay`) and wakes to dispatch the
\*    retry. With `WakeFence` the wake yields `LoopStreamItem::RetryWake`,
\*    which the stream processor authorizes against the lease
\*    (`validate_owned_execution`, Lean `retryWakeDispatches`) before the
\*    loop is polled again; without it the wake dispatches at once.
\*  - The inference drive's lease poll (`drive_inference_stream`), every
\*    `DrivePoll` ticks while the loop is pending: a lease that is no longer
\*    live drops the loop stream, so the retry never wakes.
\*  - The renewal task (`RenewalTask::start`): a poll every quarter lease,
\*    a renewal once half a lease or less remains (Lean `renewalDue`), lost
\*    once the deadline has passed. A renewal is one transaction: DefraDB's
\*    optimistic commit aborts it when the request changed after its read.
\*  - Recovery (`canonical_recovery`) fails a processing request whose lease
\*    has expired.
\*  - The test's fixture. Once it observes the failed row it reads the
\*    request's lifecycle, generation and deadline, and later, in a separate
\*    write, sets the deadline to the past filtered on the generation it read
\*    and, when `FixtureCasOnDeadline`, also on the deadline it read. The
\*    fixture is slower or faster than any schedule: it may act at any tick.
\*
\* `Backoff` is the retry delay in ticks: shorter than the horizon is the
\* scheduled ladder's first step (5 s against a 30 s lease), longer is an
\* unreached backoff (`gents_store::test_support::UNREACHED_DEADLINE`).
\*
\* `DispatchOnlyLive` is the product's promise (Lean
\* `expired_rejects_all_producer_actions` for `dispatch`); the test's two
\* observations are `FixtureExpires` (its expiry write lands) and
\* `NoRetryAfterExpiry` (an execution whose lease the fixture expired never
\* dispatches its retry).
EXTENDS Naturals, LeanDecision_RequestExecutionLease

CONSTANTS
  Lease,                \* lease duration in ticks, a multiple of 4
  DrivePoll,            \* the drive's lease poll period in ticks
  Backoff,              \* retry delay in ticks
  Horizon,              \* ticks modeled
  WakeFence,            \* the retry wake is authorized against the lease
  FixtureCasOnDeadline  \* the fixture's expiry write also matches the deadline it read

ASSUME /\ Lease \in Nat \ {0}
       /\ Lease % 4 = 0
       /\ DrivePoll \in Nat \ {0}
       /\ Backoff \in Nat \ {0}
       /\ Horizon \in Nat
       /\ WakeFence \in BOOLEAN
       /\ FixtureCasOnDeadline \in BOOLEAN

PollEvery == Lease \div 4
RenewalInterval == Lease \div 2

VARIABLES
  now,          \* wall clock
  request,      \* "processing" | "completed" | "failed"
  deadline,     \* execution_lease_expires_at
  nextRenewal,  \* next renewal poll
  renewing,     \* the renewal task has not ended as lost
  pc,           \* owner: "calling" | "backoff" | "dispatched" | "stopped"
  wakeAt,       \* when the backoff ends
  nextDrive,    \* next drive lease poll
  fixture,      \* "waiting" | "read" | "done"
  readDeadline, \* the deadline the fixture read
  expiry,       \* the fixture's write: "none" | "expired" | "missed"
  dispatchLive  \* ghost: the lease was live when the retry dispatched

vars == <<now, request, deadline, nextRenewal, renewing, pc, wakeAt, nextDrive,
          fixture, readDeadline, expiry, dispatchLive>>

TypeOK ==
  /\ now \in 0..Horizon
  /\ request \in {"processing", "completed", "failed"}
  /\ deadline \in 0..(Horizon + Lease)
  /\ nextRenewal \in 0..(Horizon + PollEvery)
  /\ renewing \in BOOLEAN
  /\ pc \in {"calling", "backoff", "dispatched", "stopped"}
  /\ wakeAt \in 0..(Horizon + Backoff)
  /\ nextDrive \in 0..(Horizon + DrivePoll)
  /\ fixture \in {"waiting", "read", "done"}
  /\ readDeadline \in 0..(Horizon + Lease)
  /\ expiry \in {"none", "expired", "missed"}
  /\ dispatchLive \in BOOLEAN

Init ==
  /\ now = 0
  /\ request = "processing"
  /\ deadline = Lease
  /\ nextRenewal = PollEvery
  /\ renewing = TRUE
  /\ pc = "calling"
  /\ wakeAt = 0
  /\ nextDrive = 0
  /\ fixture = "waiting"
  /\ readDeadline = 0
  /\ expiry = "none"
  /\ dispatchLive = FALSE

Live == request = "processing" /\ now < deadline

\* The first call fails; its failed row is what the fixture waits for.
FirstCallFails ==
  /\ pc = "calling"
  /\ pc' = "backoff"
  /\ wakeAt' = now + Backoff
  /\ nextDrive' = now + DrivePoll
  /\ UNCHANGED <<now, request, deadline, nextRenewal, renewing, fixture,
                 readDeadline, expiry, dispatchLive>>

DriveDue == pc = "backoff" /\ now >= nextDrive

DrivePollStep ==
  /\ DriveDue
  /\ IF Live
       THEN /\ nextDrive' = now + DrivePoll
            /\ UNCHANGED pc
       ELSE /\ pc' = "stopped"
            /\ UNCHANGED nextDrive
  /\ UNCHANGED <<now, request, deadline, nextRenewal, renewing, wakeAt, fixture,
                 readDeadline, expiry, dispatchLive>>

\* The backoff ends. The fenced wake asks the Lean decision.
Wake ==
  /\ pc = "backoff"
  /\ now >= wakeAt
  /\ IF WakeFence => retryWakeDispatches(TRUE, request = "processing", now < deadline)
       THEN /\ pc' = "dispatched"
            /\ dispatchLive' = Live
       ELSE /\ pc' = "stopped"
            /\ UNCHANGED dispatchLive
  /\ UNCHANGED <<now, request, deadline, nextRenewal, renewing, wakeAt, nextDrive,
                 fixture, readDeadline, expiry>>

\* The dispatched retry answers; its publication and terminal are fenced on a
\* live lease (Lean `expired_rejects_all_producer_actions`).
Complete ==
  /\ pc = "dispatched"
  /\ Live
  /\ request' = "completed"
  /\ UNCHANGED <<now, deadline, nextRenewal, renewing, pc, wakeAt, nextDrive,
                 fixture, readDeadline, expiry, dispatchLive>>

RenewalDue == renewing /\ now >= nextRenewal

Renew ==
  /\ RenewalDue
  /\ IF ~Live
       THEN /\ renewing' = FALSE
            /\ UNCHANGED <<deadline, nextRenewal>>
       ELSE /\ deadline' = IF now + RenewalInterval >= deadline
                             THEN now + Lease
                             ELSE deadline
            /\ nextRenewal' = now + PollEvery
            /\ UNCHANGED renewing
  /\ UNCHANGED <<now, request, pc, wakeAt, nextDrive, fixture, readDeadline,
                 expiry, dispatchLive>>

Recover ==
  /\ request = "processing"
  /\ now >= deadline
  /\ request' = "failed"
  /\ UNCHANGED <<now, deadline, nextRenewal, renewing, pc, wakeAt, nextDrive,
                 fixture, readDeadline, expiry, dispatchLive>>

FixtureRead ==
  /\ fixture = "waiting"
  /\ pc # "calling"
  /\ fixture' = "read"
  /\ readDeadline' = deadline
  /\ UNCHANGED <<now, request, deadline, nextRenewal, renewing, pc, wakeAt,
                 nextDrive, expiry, dispatchLive>>

FixtureWrite ==
  /\ fixture = "read"
  /\ fixture' = "done"
  /\ IF request = "processing" /\ (FixtureCasOnDeadline => deadline = readDeadline)
       THEN /\ deadline' = now
            /\ expiry' = "expired"
       ELSE /\ expiry' = "missed"
            /\ UNCHANGED deadline
  /\ UNCHANGED <<now, request, nextRenewal, renewing, pc, wakeAt, nextDrive,
                 readDeadline, dispatchLive>>

\* The clock does not pass a due renewal or drive poll: the runtime schedules
\* both (the lease's scheduling premise, `LeaseRenewalStarvation`).
Tick ==
  /\ now < Horizon
  /\ ~RenewalDue
  /\ ~DriveDue
  /\ now' = now + 1
  /\ UNCHANGED <<request, deadline, nextRenewal, renewing, pc, wakeAt, nextDrive,
                 fixture, readDeadline, expiry, dispatchLive>>

Next ==
  \/ FirstCallFails
  \/ DrivePollStep
  \/ Wake
  \/ Complete
  \/ Renew
  \/ Recover
  \/ FixtureRead
  \/ FixtureWrite
  \/ Tick

Spec == Init /\ [][Next]_vars

\* The product's promise: a retry is dispatched only under a live lease.
DispatchOnlyLive == pc = "dispatched" => dispatchLive

\* The test's first observation: its expiry write lands.
FixtureExpires == expiry # "missed"

\* The test's second observation: once the fixture expired the lease, the
\* execution never dispatched its retry.
NoRetryAfterExpiry == expiry = "expired" => pc # "dispatched"

====
