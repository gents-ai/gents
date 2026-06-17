---- MODULE PairingTransport ----
EXTENDS Naturals, FiniteSets, TLC

(***************************************************************************)
(* Connection establishment + replication liveness for ONE directed       *)
(* pairing edge (initiator -> target), the transport layer BELOW           *)
(* ReversePairing.tla.                                                     *)
(*                                                                         *)
(* ReversePairing models the abstract control-plane RPC convergence,       *)
(* ASSUMING the transport carries the install RPC. PairingReconcile (Lean) *)
(* models the desired->applied reconciliation, ASSUMING the connect/dial   *)
(* succeeds. Neither models ESTABLISHING the transport — so the live #511   *)
(* fleet hang was outside the modeled world. This spec fills that gap.     *)
(*                                                                         *)
(* THREE REAL FAILURE MODES (grounded in the production code — see README   *)
(* "PairingTransport derived requirements"):                               *)
(*                                                                         *)
(*   MODE A — connect-fails-first (the LITERAL #511 hang). reconcile_peer_  *)
(*   tick calls admin.connect(replicator_addresses) with `?` BEFORE the     *)
(*   diff. With an undialable listen-form ticket (parse_public_peer_addr    *)
(*   yields no direct addrs under no-relay/no-discovery) connect fails, the *)
(*   WHOLE tick aborts, and NOTHING is subscribed and NO applied row is     *)
(*   written. The sweep logs "tick failed" and retries forever. Modeled by  *)
(*   `Dialable = FALSE`: connState never reaches Connected, so `subscribed` *)
(*   stays FALSE — the counterexample is the connect-fails-first hang, not  *)
(*   a partial row.                                                        *)
(*                                                                         *)
(*   MODE B/C — connect OK, replicator install fails (the durable          *)
(*   "subscribed collections, replicator_addresses = null" PARTIAL ROW).    *)
(*   connect succeeds, the InstallCollection ops persist per-op (before the *)
(*   replicator op), then add_replicator fails — either its SEPARATE        *)
(*   transport dial times out (MODE B; same ticket as connect, so the       *)
(*   address fix covers it) or a pre-dial check fails: collection-cid       *)
(*   not_found / filter validation (MODE C; NOT covered by the address fix, *)
(*   a permanent dead-end). Modeled by `ReplicatorInstallable = FALSE`:     *)
(*   the edge reaches Connected + subscribed + ~replicatorInstalled and     *)
(*   STAYS there — exactly the partial row.                                *)
(*                                                                         *)
(* Headline result: end-to-end replication liveness holds IFF the ticket is *)
(* dialable AND the replicator can install. Both are preconditions the      *)
(* layer ABOVE the reconciler must supply (the invite/heartbeat ticket form *)
(* for Dialable; correct schema/filter materialization for                  *)
(* ReplicatorInstallable) — the reconcile loop's retries cannot manufacture *)
(* either, mirroring the Lean `convergence_requires_successful_install`.    *)
(***************************************************************************)

CONSTANTS
  Dialable,            \* BOOLEAN: is the dialed TICKET reachable? FALSE = MODE A.
  ReplicatorInstallable  \* BOOLEAN: can add_replicator succeed once connected? FALSE = MODE C.

ASSUME DialableIsBool == Dialable \in BOOLEAN
ASSUME ReplicatorInstallableIsBool == ReplicatorInstallable \in BOOLEAN

VARIABLES
  connState,           \* {"Disconnected","Connecting","Connected","Failed"}
  subscribed,          \* BOOLEAN: control-plane collection subscribed (add_p2p_collections)
  replicatorInstalled, \* BOOLEAN: the replicator the live bug fails to install
  docsReplicated       \* BOOLEAN: end-to-end — a document actually flowed initiator->target

vars == <<connState, subscribed, replicatorInstalled, docsReplicated>>

ConnStates == {"Disconnected", "Connecting", "Connected", "Failed"}

TypeOK ==
  /\ connState \in ConnStates
  /\ subscribed \in BOOLEAN
  /\ replicatorInstalled \in BOOLEAN
  /\ docsReplicated \in BOOLEAN

Init ==
  /\ connState = "Disconnected"
  /\ subscribed = FALSE
  /\ replicatorInstalled = FALSE
  /\ docsReplicated = FALSE

(***************************************************************************)
(* Connection establishment. `Dial` begins an attempt; the attempt either  *)
(* succeeds (only when the ticket is `Dialable`) or fails (a dial timeout,  *)
(* always possible). `Redial` retries after a failure. Retries are          *)
(* unbounded — the state space is finite without a counter — so under       *)
(* strong fairness on `DialSucceed`, a dialable ticket eventually connects,  *)
(* while an un-dialable one loops Connecting<->Failed forever (MODE A).      *)
(***************************************************************************)

Dial ==
  /\ connState = "Disconnected"
  /\ connState' = "Connecting"
  /\ UNCHANGED <<subscribed, replicatorInstalled, docsReplicated>>

\* Succeeds ONLY when the ticket is genuinely dialable (the shareable form).
\* When Dialable = FALSE this action is never enabled — the heart of MODE A.
DialSucceed ==
  /\ connState = "Connecting"
  /\ Dialable
  /\ connState' = "Connected"
  /\ UNCHANGED <<subscribed, replicatorInstalled, docsReplicated>>

\* A dial timeout — always possible from Connecting (models iroh's timeout).
DialFail ==
  /\ connState = "Connecting"
  /\ connState' = "Failed"
  /\ UNCHANGED <<subscribed, replicatorInstalled, docsReplicated>>

Redial ==
  /\ connState = "Failed"
  /\ connState' = "Connecting"
  /\ UNCHANGED <<subscribed, replicatorInstalled, docsReplicated>>

(***************************************************************************)
(* Reconcile ops. `Subscribe` is gated on `Connected` to mirror the        *)
(* engine's connect-FIRST ordering (reconcile_peer_tick connects before it  *)
(* applies any op) — NOT the transport reality, where add_p2p_collections   *)
(* is a local gossip-topic join that succeeds with zero connected peers.    *)
(* So in this model "subscribed" means "the connect gate passed", not "the  *)
(* link is up". `InstallReplicator` is gated on Connected AND subscribed to  *)
(* model the diff ordering (InstallCollection ops persist before the        *)
(* InstallReplicator op) that produces the durable partial row when the     *)
(* install then fails. It additionally requires `ReplicatorInstallable`:    *)
(* when that is FALSE the install can never succeed (MODE B/C — a separate  *)
(* replicator-dial timeout or a cid/filter failure), so the edge is stuck   *)
(* at Connected + subscribed + ~installed: the "subscribed collections,     *)
(* replicator_addresses = null" partial row.                               *)
(***************************************************************************)

Subscribe ==
  /\ connState = "Connected"
  /\ ~subscribed
  /\ subscribed' = TRUE
  /\ UNCHANGED <<connState, replicatorInstalled, docsReplicated>>

InstallReplicator ==
  /\ connState = "Connected"
  /\ subscribed
  /\ ReplicatorInstallable
  /\ ~replicatorInstalled
  /\ replicatorInstalled' = TRUE
  /\ UNCHANGED <<connState, subscribed, docsReplicated>>

\* End-to-end: once the replicator is installed and the link is up, a doc flows.
Replicate ==
  /\ connState = "Connected"
  /\ replicatorInstalled
  /\ ~docsReplicated
  /\ docsReplicated' = TRUE
  /\ UNCHANGED <<connState, subscribed, replicatorInstalled>>

Next ==
  \/ Dial
  \/ DialSucceed
  \/ DialFail
  \/ Redial
  \/ Subscribe
  \/ InstallReplicator
  \/ Replicate

(***************************************************************************)
(* Fairness. Strong fairness on `DialSucceed` so that — WHEN it is enabled *)
(* (Dialable holds and we are Connecting) — it eventually fires despite     *)
(* intervening DialFail/Redial. SF on Dial/Redial keeps retrying. WF on the *)
(* reconcile ops drives them once their guards hold. There is deliberately  *)
(* NO fairness on `DialFail`: failures are permitted, not forced. When       *)
(* Dialable = FALSE, `DialSucceed` is never enabled; when                   *)
(* ReplicatorInstallable = FALSE, `InstallReplicator` is never enabled — in *)
(* both cases no fairness can rescue liveness, exactly the live hangs.      *)
(***************************************************************************)

Fairness ==
  /\ SF_vars(Dial)
  /\ SF_vars(Redial)
  /\ SF_vars(DialSucceed)
  /\ WF_vars(Subscribe)
  /\ WF_vars(InstallReplicator)
  /\ WF_vars(Replicate)

Spec == Init /\ [][Next]_vars /\ Fairness

(***************************************************************************)
(* Liveness — the properties no existing spec expressed.                   *)
(*                                                                         *)
(* ReplicatorLiveness: the replicator the reconciler wants eventually       *)
(* installs. FALSE under MODE A (Dialable=FALSE: never Connected, so        *)
(* InstallReplicator never enabled — the connect-fails-first hang) AND      *)
(* under MODE B/C (ReplicatorInstallable=FALSE: Connected + subscribed but  *)
(* install never succeeds — the partial-row dead-end).                     *)
(*                                                                         *)
(* EndToEndLiveness: a document actually flows. The thing delegation needs. *)
(***************************************************************************)

ReplicatorLiveness == <>replicatorInstalled
EndToEndLiveness == <>docsReplicated

(***************************************************************************)
(* Safety / progress: the partial-applied state is never a SILENT dead end. *)
(* Whenever we are Connected and the control-plane collection is subscribed *)
(* but the replicator is not yet installed, InstallReplicator must still be *)
(* ENABLED (progress remains possible). This HOLDS on the healthy path      *)
(* (Dialable & ReplicatorInstallable) and is VACUOUSLY true under MODE A    *)
(* (the partial state is never reached). It is deliberately VIOLATED under  *)
(* MODE B/C (ReplicatorInstallable=FALSE): there the edge sits at           *)
(* Connected + subscribed + ~installed with InstallReplicator disabled —    *)
(* TLC then returns that exact partial row as the counterexample, which IS  *)
(* the "subscribed collections, replicator_addresses = null" live state.    *)
(***************************************************************************)

PartialApplyHasProgress ==
  (connState = "Connected" /\ subscribed /\ ~replicatorInstalled)
    => ENABLED InstallReplicator

\* docsReplicated is only ever reachable through an installed replicator.
ReplicationImpliesReplicator ==
  docsReplicated => replicatorInstalled
====
