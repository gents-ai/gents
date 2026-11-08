---- MODULE BackgroundAdmissionContention ----
\* Concurrent writers of one request's `AgentRequest` row and session
\* documents during a turn (#3592).
\*
\* Each accepted `spawn_process` call runs three DefraDB transactions, in
\* order:
\*   1. dispatch start (`tool_call.start_running_canonical`): reads the
\*      request's lifecycle, generation and lease deadline to fence the call,
\*      then moves its own tool row from pending to running;
\*   2. admission (`tool_call.admit_spawned_background`): reads the request's
\*      background `AgentToolCall` rows to count live ones, reads the request
\*      fence (lease deadline included) and the `AgentSession`, bumps
\*      `AgentRequest.background_admission_sequence`, then creates the child;
\*      a full budget refuses before writing;
\*   3. receipt publication (`tool_call.complete_delivery`, or
\*      `tool_call.fail_raw_with_presentation` for a refusal): rereads the
\*      request's tool rows, takes the next `AgentMessage` session sequence and
\*      refreshes the `AgentSession` request observation.
\* A running child's start and completion write its own tool row and the
\* session observation.
\*
\* The request's execution lease renewal (`lifecycle.renew_execution_lease`,
\* `renew_in_transaction`) runs beside the turn on its own task. Each poll is
\* one transaction that reads the request's lifecycle, generation, deadline
\* and duration and CASes a new deadline. A renewal is due half a lease before
\* its deadline and polls run every quarter lease, so after the poll that
\* renewed it a lease period has one due poll with margin and one at its
\* deadline: `RenewTicks` is that one. `RedriveLostPoll` selects the renewal
\* owner's handling of a poll whose attempts all abort: TRUE re-drives it at
\* once while the lease is live; FALSE waits for the next poll, the code
\* before the fix.
\*
\* Keys: "req" is the `AgentRequest` document; "sess" abstracts the
\* session-scoped documents every call reads and writes (the request's
\* `AgentToolCall` rows, the `AgentMessage` sequence index entry and the
\* `AgentSession` document). Writers of one document conflict whichever of
\* its fields they change: an admission bump of
\* `background_admission_sequence` aborts an open renewal of
\* `execution_lease_expires_at`, and a renewal aborts an open admission
\* (`request_row_writers` tests). The receipt's observation refresh reads the
\* request row too, and is modeled as conflicting with it.
\*
\* DefraDB transactions are optimistic: a commit aborts when a key it read or
\* wrote changed after its snapshot. A predicate read does not see a sibling's
\* newly created row, which is why admission bumps the sequence. Every attempt
\* budget is bounded; exhausting it fails a call ("tool call persistence
\* failed: DefraDB transaction conflict") with no owner to re-drive it, and
\* ends a renewal poll, which only the renewal task can re-drive.
\*
\* `SequentialAdmission` selects the owned loop's order: it dispatches a
\* non-read call such as `spawn_process` alone, and a read batch
\* (`run_batch`) awaits each call's `on_tool_call` (dispatch start, admission
\* and receipt) before the next, so sibling calls of one turn never overlap.
\* FALSE lets every sibling run its transactions at once, the schedule the
\* regression drove.
\*
\* `RenewalSpaced` is the renewal cadence premise: consecutive renewals of a
\* lease are at least half its duration apart (Lean
\* `RequestExecutionLease.consecutive_renewals_obey_cadence`), longer than any
\* one call's bounded attempt series, so at most one renewal commits while a
\* call's transaction is retrying. FALSE lets renewals commit back to back.
EXTENDS Naturals, FiniteSets

CONSTANTS
  N,                    \* calls in one assistant turn, admitted in order 1..N
  Max,                  \* MAX_BACKGROUNDED_TOOLS_PER_PARENT
  Attempts,             \* attempts of a sibling-contended admission or receipt
  StartAttempts,        \* attempts of a dispatch start (Standard, 4)
  RenewAttempts,        \* attempts of one renewal poll (Standard, 4)
  Renewals,             \* renewal commits the turn may see
  RenewTicks,           \* polls of one lease period before it lapses
  SequentialAdmission,  \* the owned loop's per-call order
  BumpSequence,         \* admission writes the request admission sequence
  RedriveLostPoll,      \* the renewal re-drives an exhausted poll at once
  RenewalSpaced         \* the renewal cadence premise

ASSUME /\ N \in Nat \ {0}
       /\ Max \in Nat \ {0}
       /\ Attempts \in Nat \ {0}
       /\ StartAttempts \in Nat \ {0}
       /\ RenewAttempts \in Nat \ {0}
       /\ Renewals \in Nat
       /\ RenewTicks \in Nat \ {0}
       /\ SequentialAdmission \in BOOLEAN
       /\ BumpSequence \in BOOLEAN
       /\ RedriveLostPoll \in BOOLEAN
       /\ RenewalSpaced \in BOOLEAN

Calls == 1..N
Keys == {"req", "sess"}

\* Read-and-write sets. A dispatch start reads the request fence. Admission
\* reads the fence and the session and, when it bumps the sequence, writes the
\* request. A receipt reads the request and writes the session. A renewal
\* reads and writes the request only.
StartKeys == {"req"}
AdmitKeys == {"req", "sess"}
PublishKeys == {"req", "sess"}
RenewKeys == {"req"}

Active == {"start", "admit", "publish"}

VARIABLES
  ver,      \* committed version of each key
  phase,    \* "start" | "admit" | "publish" | "settled" | "lost"
  open,     \* an attempt of the call's current transaction is in flight
  snap,     \* key versions the open attempt observed
  liveSnap, \* live background count the open admission attempt observed
  tries,    \* attempts spent on the current transaction
  refused,  \* the admission refused on the budget
  child,    \* "none" | "running" | "done"
  receipts, \* receipts published for the call
  rseen,    \* a renewal committed during the call's current attempt series
  startConflicted, \* some dispatch start attempt aborted
  rOpen,    \* a renewal attempt is in flight
  rSnap,    \* key versions the open renewal attempt observed
  rTries,   \* attempts spent on the current renewal poll
  rCommits, \* renewals committed
  rTicks,   \* polls left before the lease lapses
  rLost,    \* renewal polls whose attempts all aborted
  lease     \* "live" | "lapsed"

callVars == <<phase, open, snap, liveSnap, tries, refused, child, receipts>>
renewVars == <<rOpen, rSnap, rTries, rCommits, rTicks, rLost, lease>>
vars == <<ver, callVars, rseen, startConflicted, renewVars>>

Live == Cardinality({c \in Calls : child[c] = "running"})

TurnLive == \E c \in Calls : phase[c] \in Active

KeysOf(c) ==
  CASE phase[c] = "start" -> StartKeys
    [] phase[c] = "admit" -> AdmitKeys
    [] OTHER -> PublishKeys

BudgetOf(c) == IF phase[c] = "start" THEN StartAttempts ELSE Attempts

\* The call has begun its current transaction and not yet committed it.
InSeries(c) == phase[c] \in Active /\ tries[c] > 0

TypeOK ==
  /\ ver \in [Keys -> Nat]
  /\ phase \in [Calls -> Active \cup {"settled", "lost"}]
  /\ open \in [Calls -> BOOLEAN]
  /\ snap \in [Calls -> [Keys -> Nat]]
  /\ liveSnap \in [Calls -> Nat]
  /\ tries \in [Calls -> Nat]
  /\ refused \in [Calls -> BOOLEAN]
  /\ child \in [Calls -> {"none", "running", "done"}]
  /\ receipts \in [Calls -> Nat]
  /\ rseen \in [Calls -> BOOLEAN]
  /\ startConflicted \in BOOLEAN
  /\ rOpen \in BOOLEAN
  /\ rSnap \in [Keys -> Nat]
  /\ rTries \in Nat
  /\ rCommits \in 0..Renewals
  /\ rTicks \in 0..RenewTicks
  /\ rLost \in Nat
  /\ lease \in {"live", "lapsed"}

Init ==
  /\ ver = [k \in Keys |-> 0]
  /\ phase = [c \in Calls |-> "start"]
  /\ open = [c \in Calls |-> FALSE]
  /\ snap = [c \in Calls |-> [k \in Keys |-> 0]]
  /\ liveSnap = [c \in Calls |-> 0]
  /\ tries = [c \in Calls |-> 0]
  /\ refused = [c \in Calls |-> FALSE]
  /\ child = [c \in Calls |-> "none"]
  /\ receipts = [c \in Calls |-> 0]
  /\ rseen = [c \in Calls |-> FALSE]
  /\ startConflicted = FALSE
  /\ rOpen = FALSE
  /\ rSnap = [k \in Keys |-> 0]
  /\ rTries = 0
  /\ rCommits = 0
  /\ rTicks = RenewTicks
  /\ rLost = 0
  /\ lease = "live"

\* The owned loop starts call c only after every earlier call's
\* `on_tool_call` returned its receipt.
MayStart(c) ==
  \/ ~SequentialAdmission
  \/ \A d \in Calls : d < c => phase[d] \in {"settled", "lost"}

\* A lapsed lease fails every fence, so no call begins after it.
Begin(c) ==
  /\ phase[c] \in Active
  /\ ~open[c]
  /\ MayStart(c)
  /\ lease = "live"
  /\ open' = [open EXCEPT ![c] = TRUE]
  /\ snap' = [snap EXCEPT ![c] = ver]
  /\ liveSnap' = [liveSnap EXCEPT ![c] = Live]
  /\ tries' = [tries EXCEPT ![c] = @ + 1]
  /\ UNCHANGED <<ver, phase, refused, child, receipts, rseen, startConflicted,
                 renewVars>>

Conflicts(c) == \E k \in KeysOf(c) : ver[k] # snap[c][k]

\* A conflicting commit aborts. The attempt loop retries only while its
\* budget lasts; the last abort fails the call with no owner to re-drive it.
Abort(c) ==
  /\ open[c]
  /\ Conflicts(c)
  /\ open' = [open EXCEPT ![c] = FALSE]
  /\ phase' = IF tries[c] >= BudgetOf(c)
                THEN [phase EXCEPT ![c] = "lost"]
                ELSE phase
  /\ startConflicted' = (startConflicted \/ phase[c] = "start")
  /\ UNCHANGED <<ver, snap, liveSnap, tries, refused, child, receipts, rseen,
                 renewVars>>

Bump(keys) == [k \in Keys |-> IF k \in keys THEN ver[k] + 1 ELSE ver[k]]

\* Committing ends the call's attempt series.
Settle(c, next) ==
  /\ open' = [open EXCEPT ![c] = FALSE]
  /\ phase' = [phase EXCEPT ![c] = next]
  /\ tries' = [tries EXCEPT ![c] = 0]
  /\ rseen' = [rseen EXCEPT ![c] = FALSE]

\* The dispatch start writes only the call's own tool row.
CommitStart(c) ==
  /\ open[c]
  /\ phase[c] = "start"
  /\ ~Conflicts(c)
  /\ Settle(c, "admit")
  /\ UNCHANGED <<ver, snap, liveSnap, refused, child, receipts,
                 startConflicted, renewVars>>

\* A refusal returns before any write, so it commits nothing and moves to
\* the refusal receipt.
CommitAdmission(c) ==
  /\ open[c]
  /\ phase[c] = "admit"
  /\ ~Conflicts(c)
  /\ Settle(c, "publish")
  /\ IF liveSnap[c] < Max
       THEN /\ child' = [child EXCEPT ![c] = "running"]
            /\ ver' = IF BumpSequence THEN Bump({"req"}) ELSE ver
            /\ UNCHANGED refused
       ELSE /\ refused' = [refused EXCEPT ![c] = TRUE]
            /\ UNCHANGED <<child, ver>>
  /\ UNCHANGED <<snap, liveSnap, receipts, startConflicted, renewVars>>

CommitReceipt(c) ==
  /\ open[c]
  /\ phase[c] = "publish"
  /\ ~Conflicts(c)
  /\ Settle(c, "settled")
  /\ ver' = Bump({"sess"})
  /\ receipts' = [receipts EXCEPT ![c] = @ + 1]
  /\ UNCHANGED <<snap, liveSnap, refused, child, startConflicted, renewVars>>

\* A running child finishes and writes its row and the session observation.
\* Background completion recovery re-drives its transaction, so it is modeled
\* as one commit.
ChildCompletes(c) ==
  /\ child[c] = "running"
  /\ child' = [child EXCEPT ![c] = "done"]
  /\ ver' = Bump({"sess"})
  /\ UNCHANGED <<phase, open, snap, liveSnap, tries, refused, receipts, rseen,
                 startConflicted, renewVars>>

\* A renewal poll that is due begins an attempt while the turn runs.
RenewBegin ==
  /\ ~rOpen
  /\ lease = "live"
  /\ rCommits < Renewals
  /\ TurnLive
  /\ rOpen' = TRUE
  /\ rSnap' = ver
  /\ rTries' = rTries + 1
  /\ UNCHANGED <<ver, callVars, rseen, startConflicted, rCommits, rTicks,
                 rLost, lease>>

RenewConflicts == \E k \in RenewKeys : ver[k] # rSnap[k]

\* A poll whose attempts all abort returns an error. Re-driven, the renewal
\* starts a fresh poll at once and the lease stays live: the turn's sibling
\* writes of the request are finite, and none outlasts the quarter lease left
\* after the due poll. Otherwise the poll is lost until the next one, and the
\* lease lapses when the period has no poll left.
RenewAbort ==
  /\ rOpen
  /\ RenewConflicts
  /\ rOpen' = FALSE
  /\ IF rTries >= RenewAttempts
       THEN /\ rTries' = 0
            /\ rLost' = rLost + 1
            /\ IF RedriveLostPoll
                 THEN UNCHANGED <<rTicks, lease>>
                 ELSE /\ rTicks' = rTicks - 1
                      /\ lease' = IF rTicks = 1 THEN "lapsed" ELSE "live"
       ELSE UNCHANGED <<rTries, rLost, rTicks, lease>>
  /\ UNCHANGED <<ver, callVars, rseen, startConflicted, rSnap, rCommits>>

\* Under the cadence premise a renewal does not commit twice within one
\* call's attempt series.
CadenceAllows ==
  RenewalSpaced => \A c \in Calls : InSeries(c) => ~rseen[c]

RenewCommit ==
  /\ rOpen
  /\ ~RenewConflicts
  /\ CadenceAllows
  /\ rOpen' = FALSE
  /\ rTries' = 0
  /\ rCommits' = rCommits + 1
  /\ rTicks' = RenewTicks
  /\ ver' = Bump(RenewKeys)
  /\ rseen' = [c \in Calls |-> rseen[c] \/ InSeries(c)]
  /\ UNCHANGED <<callVars, startConflicted, rSnap, rLost, lease>>

Next ==
  \/ \E c \in Calls :
       \/ Begin(c)
       \/ Abort(c)
       \/ CommitStart(c)
       \/ CommitAdmission(c)
       \/ CommitReceipt(c)
       \/ ChildCompletes(c)
  \/ RenewBegin
  \/ RenewAbort
  \/ RenewCommit

\* The renewal task is the environment: no call waits for it, so it carries
\* no fairness obligation.
Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ \A c \in Calls :
       /\ WF_vars(Begin(c))
       /\ WF_vars(Abort(c))
       /\ WF_vars(CommitStart(c))
       /\ WF_vars(CommitAdmission(c))
       /\ WF_vars(CommitReceipt(c))

\* The budget never admits more than Max live children.
BudgetBound == Live <= Max

\* No accepted call loses its dispatch start, admission or receipt to a
\* storage conflict.
NoConflictLoss == \A c \in Calls : phase[c] # "lost"

\* Each accepted call publishes at most one receipt, and only once settled.
ReceiptOnce ==
  \A c \in Calls :
    /\ receipts[c] <= 1
    /\ (receipts[c] = 1) <=> (phase[c] = "settled")

\* No renewal poll loses all its attempts to a sibling writer. Production
\* does not guarantee this: admission bumps write the request row, so a burst
\* of them exhausts a poll.
RenewalNeverLost == rLost = 0

\* The lease never lapses while the turn runs: the weaker guarantee
\* production gives, by re-driving an exhausted poll.
LeaseLive == lease = "live"

\* Diagnostic: a renewal commit never aborts a dispatch start. Violated by
\* design: the traced conflict, which the start's attempts absorb.
StartNeverConflicts == ~startConflicted

\* Every accepted call eventually publishes its one receipt.
EveryCallSettles == <>(\A c \in Calls : phase[c] = "settled")

====
