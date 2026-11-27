---- MODULE DetachedGrantRevocationFence ----
\* lean: Identity.DetachedGrantFence.decideGrant
\* crates: gents-store
\* models: Identity.DetachedGrantFence.recordRevocation, Identity.DetachedGrantFence.isRevokedSince
(***************************************************************************)
(* Detached relationship-grant completion (`ConfigAccess::complete_        *)
(* relationship_grants`) racing a concurrent owner revocation              *)
(* (`delete_actor_relationship` / `delete_resource_relationship` /         *)
(* transactional delete) and a denied/failed revocation attempt.           *)
(*                                                                         *)
(* After a write transaction commits a document, its queued detached       *)
(* relationship grants complete outside that transaction. A per-node       *)
(* monotonic revocation fence serializes detached completion and owner     *)
(* revocations on that node:                                               *)
(*  - `FenceEnabled`: detached completion asks Lean's `decideGrant`        *)
(*    whether the tuple was revoked since the transaction's baseline       *)
(*    epoch.                                                               *)
(*  - `SerializeGate`: the node's fence gate is held across check+add      *)
(*    and across delete+epoch-bump so no revocation commits in the         *)
(*    check-then-add window.                                               *)
(*  - `BumpOnlyOnSuccess`: only a succeeded native revocation advances     *)
(*    the tuple's revocation epoch; a denied revocation leaves it          *)
(*    unchanged.                                                           *)
(***************************************************************************)
EXTENDS Naturals, LeanDecision_Identity_DetachedGrantFence

CONSTANTS
  FenceEnabled,       \* detached completion checks the revocation fence via Lean decideGrant
  SerializeGate,      \* fence gate serializes check+add against delete+bump
  BumpOnlyOnSuccess   \* denied/failed revocations do not advance the fence epoch

VARIABLES
  fenceEpoch,         \* monotonic revocation epoch on the node
  tupleRevokedEpoch,  \* latest revocation epoch recorded for this tuple
  txCommitted,        \* creator transaction committed and queued the detached grant
  grantBaseline,      \* fenceEpoch observed when the transaction began
  compPhase,          \* completer: "idle" | "queued" | "checked" | "done"
  compDecision,       \* decision cached between check and native add
  gate,               \* node fence gate: "free" | "completer" | "revoker"
  revokerPhase,       \* owner revoker: "idle" | "started" | "done"
  failedRevokeDone,   \* a denied revocation attempt already ran
  retried,            \* completer already took one transient failure retry
  access,             \* whether the relationship currently stands in DefraDB
  revoked,            \* ghost: an authorized owner revocation succeeded
  resurrected         \* ghost: detached completion added access after revocation succeeded

vars == <<fenceEpoch, tupleRevokedEpoch, txCommitted, grantBaseline,
          compPhase, compDecision, gate, revokerPhase, failedRevokeDone,
          retried, access, revoked, resurrected>>

TypeOK ==
  /\ fenceEpoch \in 0..2
  /\ tupleRevokedEpoch \in 0..2
  /\ txCommitted \in BOOLEAN
  /\ grantBaseline \in 0..2
  /\ compPhase \in {"idle", "queued", "checked", "done"}
  /\ compDecision \in {"none", "grant", "skip_revoked", "refuse_actor_mismatch"}
  /\ gate \in {"free", "completer", "revoker"}
  /\ revokerPhase \in {"idle", "started", "done"}
  /\ failedRevokeDone \in BOOLEAN
  /\ retried \in BOOLEAN
  /\ access \in BOOLEAN
  /\ revoked \in BOOLEAN
  /\ resurrected \in BOOLEAN

Init ==
  /\ fenceEpoch = 0
  /\ tupleRevokedEpoch = 0
  /\ txCommitted = FALSE
  /\ grantBaseline = 0
  /\ compPhase = "idle"
  /\ compDecision = "none"
  /\ gate = "free"
  /\ revokerPhase = "idle"
  /\ failedRevokeDone = FALSE
  /\ retried = FALSE
  /\ access = FALSE
  /\ revoked = FALSE
  /\ resurrected = FALSE

\* Creator transaction begins, captures the node's baseline epoch, commits its
\* document, and queues the detached relationship grant.
CommitTx ==
  /\ ~txCommitted
  /\ compPhase = "idle"
  /\ txCommitted' = TRUE
  /\ grantBaseline' = fenceEpoch
  /\ compPhase' = "queued"
  /\ UNCHANGED <<fenceEpoch, tupleRevokedEpoch, compDecision, gate,
                 revokerPhase, failedRevokeDone, retried, access, revoked, resurrected>>

\* Detached completion enters the fence gate (when SerializeGate holds) and
\* consults Lean's `decideGrant`.
CompleteCheck ==
  /\ compPhase = "queued"
  /\ SerializeGate => gate = "free"
  /\ gate' = IF SerializeGate THEN "completer" ELSE gate
  /\ compDecision' =
       decideGrant("detached", TRUE,
                   IF FenceEnabled THEN tupleRevokedEpoch > grantBaseline ELSE FALSE)
  /\ compPhase' = "checked"
  /\ UNCHANGED <<fenceEpoch, tupleRevokedEpoch, txCommitted, grantBaseline,
                 revokerPhase, failedRevokeDone, retried, access, revoked, resurrected>>

\* Detached completion executes native add only when `compDecision = "grant"`,
\* otherwise leaves access unchanged, then releases the fence gate.
CompleteApply ==
  /\ compPhase = "checked"
  /\ IF compDecision = "grant" THEN
       /\ access' = TRUE
       /\ resurrected' = (resurrected \/ revoked)
     ELSE
       /\ access' = access
       /\ resurrected' = resurrected
  /\ compPhase' = "done"
  /\ gate' = IF SerializeGate THEN "free" ELSE gate
  /\ UNCHANGED <<fenceEpoch, tupleRevokedEpoch, txCommitted, grantBaseline,
                 compDecision, revokerPhase, failedRevokeDone, retried, revoked>>

\* A transient failure before native add succeeds returns
\* `CommittedRelationshipsIncomplete`, leaving the grant queued for a later retry.
CompleteTransientFail ==
  /\ compPhase = "checked"
  /\ compDecision = "grant"
  /\ ~retried
  /\ retried' = TRUE
  /\ compPhase' = "queued"
  /\ compDecision' = "none"
  /\ gate' = IF SerializeGate THEN "free" ELSE gate
  /\ UNCHANGED <<fenceEpoch, tupleRevokedEpoch, txCommitted, grantBaseline,
                 revokerPhase, failedRevokeDone, access, revoked, resurrected>>

\* Authorized owner begins a revocation of the tuple, acquiring the fence gate.
RevokeStart ==
  /\ txCommitted
  /\ revokerPhase = "idle"
  /\ SerializeGate => gate = "free"
  /\ gate' = IF SerializeGate THEN "revoker" ELSE gate
  /\ revokerPhase' = "started"
  /\ UNCHANGED <<fenceEpoch, tupleRevokedEpoch, txCommitted, grantBaseline,
                 compPhase, compDecision, failedRevokeDone, retried, access, revoked, resurrected>>

\* Owner's native delete succeeds and advances the tuple's revocation epoch
\* before releasing the fence gate.
RevokeCommit ==
  /\ revokerPhase = "started"
  /\ access' = FALSE
  /\ revoked' = TRUE
  /\ fenceEpoch' = fenceEpoch + 1
  /\ tupleRevokedEpoch' = fenceEpoch + 1
  /\ revokerPhase' = "done"
  /\ gate' = IF SerializeGate THEN "free" ELSE gate
  /\ UNCHANGED <<txCommitted, grantBaseline, compPhase, compDecision,
                 failedRevokeDone, retried, resurrected>>

\* An unauthorized or failed revocation attempt changes no relationship in
\* DefraDB and must not advance the fence epoch.
FailedRevoke ==
  /\ txCommitted
  /\ ~failedRevokeDone
  /\ SerializeGate => gate = "free"
  /\ failedRevokeDone' = TRUE
  /\ fenceEpoch' = IF BumpOnlyOnSuccess THEN fenceEpoch ELSE fenceEpoch + 1
  /\ tupleRevokedEpoch' = IF BumpOnlyOnSuccess THEN tupleRevokedEpoch ELSE fenceEpoch + 1
  /\ UNCHANGED <<txCommitted, grantBaseline, compPhase, compDecision, gate,
                 revokerPhase, retried, access, revoked, resurrected>>

Next ==
  \/ CommitTx
  \/ CompleteCheck
  \/ CompleteApply
  \/ CompleteTransientFail
  \/ RevokeStart
  \/ RevokeCommit
  \/ FailedRevoke

Fairness ==
  /\ WF_vars(CommitTx)
  /\ WF_vars(CompleteCheck)
  /\ WF_vars(CompleteApply)
  /\ WF_vars(RevokeCommit)

Spec == Init /\ [][Next]_vars /\ Fairness

\* Safety: a detached completion never re-establishes access after an owner
\* revocation has succeeded.
NoResurrectionAfterRevocation == ~resurrected

\* Safety: a denied or failed revocation never blocks a legitimate detached
\* completion when no owner revocation succeeded.
NoFalseFenceOnFailedRevoke == (compPhase = "done" /\ ~revoked) => access

\* Liveness: detached completion finishes under fair scheduling.
CompletionTerminates == <>(compPhase = "done")

====
