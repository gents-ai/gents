---- MODULE TerminalizeRenewalContention ----
\* lean: TransactionReplay.disposition
\* crates: gents-session, gents-store
\* Interleaving of an execution owner's `lifecycle.terminalize_execution`
\* transaction (`RequestLifecycle::finish_natural_turn` / `terminalize_owned`)
\* and its own background `lifecycle.renew_execution_lease` task (`RenewalTask`)
\* on the same `AgentRequest` document.
\*
\* Both transactions read and update `AgentRequest`. DefraDB MVCC aborts
\* `terminalize_execution` at commit whenever a concurrent `renew_execution_lease`
\* commits after `terminalize_execution` opened its snapshot. If
\* `terminalize_execution` takes longer than the renewal cadence (or overlaps
\* repeated renewals), every attempt in `TermAttempts` aborts and the request
\* fails with `DefraDB transaction conflict`.
\*
\* `RenewalGate` models the leaf mutex shared between `RenewalTask` and
\* `RequestLifecycle`'s terminalization entry points:
\*   - TRUE: `terminalize_execution` acquires the gate before beginning its
\*     transaction (draining any in-flight renewal attempt first) and holds it
\*     across the transaction, preventing `RenewalTask` from starting a
\*     competing write against the same `AgentRequest`. On `Won`/`AlreadySame`,
\*     `RenewalTask` is stopped; on `PendingQueueChanged`, the gate is released
\*     with `RenewalTask` still live so the loop continues with renewal intact.
\*   - FALSE: `RenewalTask` runs concurrently with `terminalize_execution`
\*     without coordination (the behavior before the fix).
EXTENDS Naturals, LeanDecision_TransactionReplay

CONSTANTS
  TermAttempts,    \* bounded attempt budget of terminalize_execution
  MaxRenewals,     \* renewal commits allowed in the trace
  AllowRequeue,    \* whether PendingQueueChanged may occur once
  RenewalGate      \* TRUE = shared renewal gate; FALSE = ungated

ASSUME /\ TermAttempts \in Nat \ {0}
       /\ MaxRenewals \in Nat
       /\ AllowRequeue \in BOOLEAN
       /\ RenewalGate \in BOOLEAN

VARIABLES
  ver,        \* committed version of AgentRequest
  gate,       \* "free" | "term" | "renew"
  tPhase,     \* "idle" | "open" | "retry" | "done" | "requeued" | "lost"
  tSnap,      \* AgentRequest version observed by open terminalize attempt
  tTries,     \* attempts spent by terminalize_execution
  rOpen,      \* a renewal attempt is in flight
  rSnap,      \* AgentRequest version observed by open renewal attempt
  rCommits,   \* number of committed renewals
  rStopped    \* RenewalTask has been stopped after terminalization

vars == <<ver, gate, tPhase, tSnap, tTries, rOpen, rSnap, rCommits, rStopped>>

Replays(spent, budget) ==
  disposition("conflict_retry", "commit", "conflict", FALSE, TRUE,
              spent < budget) = "retry"

TypeOK ==
  /\ ver \in Nat
  /\ gate \in {"free", "term", "renew"}
  /\ tPhase \in {"idle", "open", "retry", "done", "requeued", "lost"}
  /\ tSnap \in Nat
  /\ tTries \in 0..TermAttempts
  /\ rOpen \in BOOLEAN
  /\ rSnap \in Nat
  /\ rCommits \in 0..MaxRenewals
  /\ rStopped \in BOOLEAN

Init ==
  /\ ver = 0
  /\ gate = "free"
  /\ tPhase = "idle"
  /\ tSnap = 0
  /\ tTries = 0
  /\ rOpen = FALSE
  /\ rSnap = 0
  /\ rCommits = 0
  /\ rStopped = FALSE

\* Terminalize acquires the renewal gate (when enabled) and opens attempt 1.
TermStart ==
  /\ tPhase = "idle"
  /\ (~RenewalGate \/ gate = "free")
  /\ gate' = IF RenewalGate THEN "term" ELSE gate
  /\ tPhase' = "open"
  /\ tSnap' = ver
  /\ tTries' = 1
  /\ UNCHANGED <<ver, rOpen, rSnap, rCommits, rStopped>>

\* Retry after a conflict while holding the gate (within transact_idempotent).
TermRetry ==
  /\ tPhase = "retry"
  /\ tPhase' = "open"
  /\ tSnap' = ver
  /\ tTries' = tTries + 1
  /\ UNCHANGED <<ver, gate, rOpen, rSnap, rCommits, rStopped>>

\* Conflict at commit: ver changed since tSnap.
TermAbort ==
  /\ tPhase = "open"
  /\ ver # tSnap
  /\ IF Replays(tTries, TermAttempts)
       THEN /\ tPhase' = "retry"
            /\ UNCHANGED gate
       ELSE /\ tPhase' = "lost"
            /\ gate' = IF RenewalGate THEN "free" ELSE gate
  /\ UNCHANGED <<ver, tSnap, tTries, rOpen, rSnap, rCommits, rStopped>>

\* Successful terminalize commit: bumps AgentRequest, stops RenewalTask, releases gate.
TermCommit ==
  /\ tPhase = "open"
  /\ ver = tSnap
  /\ ver' = ver + 1
  /\ tPhase' = "done"
  /\ gate' = IF RenewalGate THEN "free" ELSE gate
  /\ rStopped' = TRUE
  /\ UNCHANGED <<tSnap, tTries, rOpen, rSnap, rCommits>>

\* Natural finish observes PendingQueueChanged: aborts cleanly, releases gate,
\* and leaves RenewalTask alive for subsequent turns.
TermPendingChanged ==
  /\ AllowRequeue
  /\ tPhase = "open"
  /\ tPhase' = "requeued"
  /\ gate' = IF RenewalGate THEN "free" ELSE gate
  /\ UNCHANGED <<ver, tSnap, tTries, rOpen, rSnap, rCommits, rStopped>>

\* RenewalTask begins an attempt while not stopped.
RenewBegin ==
  /\ ~rStopped
  /\ ~rOpen
  /\ rCommits < MaxRenewals
  /\ (~RenewalGate \/ gate = "free")
  /\ gate' = IF RenewalGate THEN "renew" ELSE gate
  /\ rOpen' = TRUE
  /\ rSnap' = ver
  /\ UNCHANGED <<ver, tPhase, tSnap, tTries, rCommits, rStopped>>

\* Renewal commits if AgentRequest did not change since rSnap.
RenewCommit ==
  /\ rOpen
  /\ ver = rSnap
  /\ ver' = ver + 1
  /\ rOpen' = FALSE
  /\ rCommits' = rCommits + 1
  /\ gate' = IF RenewalGate THEN "free" ELSE gate
  /\ UNCHANGED <<tPhase, tSnap, tTries, rSnap, rStopped>>

\* Renewal aborts if AgentRequest changed since rSnap.
RenewAbort ==
  /\ rOpen
  /\ ver # rSnap
  /\ rOpen' = FALSE
  /\ gate' = IF RenewalGate THEN "free" ELSE gate
  /\ UNCHANGED <<ver, tPhase, tSnap, tTries, rSnap, rCommits, rStopped>>

Next ==
  \/ TermStart
  \/ TermRetry
  \/ TermAbort
  \/ TermCommit
  \/ TermPendingChanged
  \/ RenewBegin
  \/ RenewCommit
  \/ RenewAbort

Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ WF_vars(TermStart)
  /\ WF_vars(TermRetry)
  /\ WF_vars(TermAbort)
  /\ WF_vars(TermCommit)
  /\ WF_vars(RenewCommit)
  /\ WF_vars(RenewAbort)

\* Terminalize never exhausts its retry budget due to renewal conflict.
NoConflictLoss == tPhase # "lost"

\* PendingQueueChanged never stops the renewal task.
RenewalAliveAfterPendingChange == (tPhase = "requeued") => ~rStopped

\* Terminalize always finishes or returns PendingQueueChanged.
TerminalizesOrRequeues == <>(tPhase \in {"done", "requeued"})

====
