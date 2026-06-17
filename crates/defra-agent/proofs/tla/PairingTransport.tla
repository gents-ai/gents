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
(* succeeds (its `dial` is infallible). Neither models establishing the    *)
(* transport — so the live failure (the reconciler connects, subscribes    *)
(* the control-plane collections, but the replicator's transport dial      *)
(* times out and the replicator never installs) was outside the modeled    *)
(* world. This spec fills that gap.                                        *)
(*                                                                         *)
(* The headline result: replication liveness (the replicator eventually    *)
(* installs and a doc flows end-to-end) holds IFF the address the          *)
(* reconciler dials is actually dialable. With `Dialable = TRUE` (the      *)
(* shareable-address fix) TLC verifies liveness. With `Dialable = FALSE`   *)
(* (the observed bug: an under-specified listen-form address that resolves *)
(* to no dialable direct addr under no-relay/no-discovery) TLC returns the  *)
(* exact stuck partial trace. The reconciler cannot make `Dialable` true on *)
(* its own — it is a transport precondition the layer above must guarantee, *)
(* mirroring the Lean `convergence_requires_successful_install` obligation. *)
(***************************************************************************)

CONSTANTS
  Dialable   \* BOOLEAN: is the dialed address actually reachable? FALSE = the bug.

ASSUME DialableIsBool == Dialable \in BOOLEAN

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
(* succeeds (only when the address is `Dialable`) or fails (a dial timeout, *)
(* always possible). `Redial` retries after a failure. Retries are          *)
(* unbounded — the state space is finite without a counter — so under       *)
(* strong fairness on `DialSucceed`, a dialable address eventually          *)
(* connects, while an un-dialable one loops Connecting<->Failed forever.    *)
(***************************************************************************)

Dial ==
  /\ connState = "Disconnected"
  /\ connState' = "Connecting"
  /\ UNCHANGED <<subscribed, replicatorInstalled, docsReplicated>>

\* Succeeds ONLY when the address is genuinely dialable (the shareable form).
\* When Dialable = FALSE this action is never enabled — the heart of the bug.
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
(* Reconcile ops — both REQUIRE a live connection, mirroring the Lean      *)
(* `reconcileInstall*` guards (`pre.actual.connected = true`). Subscribe is *)
(* the control-plane collection (Replicate delivery's add_p2p_collections); *)
(* InstallReplicator is the action the live tick never reaches when the     *)
(* dial fails, producing the "subscribed but no replicator" partial state.  *)
(***************************************************************************)

Subscribe ==
  /\ connState = "Connected"
  /\ ~subscribed
  /\ subscribed' = TRUE
  /\ UNCHANGED <<connState, replicatorInstalled, docsReplicated>>

InstallReplicator ==
  /\ connState = "Connected"
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
(* (i.e. Dialable holds and we are Connecting) — it eventually fires       *)
(* despite intervening DialFail/Redial. SF on Dial/Redial keeps retrying.  *)
(* WF on the reconcile ops drives them once Connected. There is            *)
(* deliberately NO fairness on `DialFail`: failures are permitted, not     *)
(* forced. When Dialable = FALSE, `DialSucceed` is never enabled, so no     *)
(* fairness can rescue liveness — exactly the live hang.                   *)
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
(* installs. FALSE under `Dialable = FALSE`: TLC returns the stuck trace    *)
(* Disconnected -> Connecting -> Failed -> Connecting -> ... where the      *)
(* connection never reaches Connected, so InstallReplicator is never        *)
(* enabled — the formal counterpart of the live "replicator_addresses:      *)
(* null" hang.                                                             *)
(*                                                                         *)
(* EndToEndLiveness: a document actually flows. The thing delegation needs. *)
(***************************************************************************)

ReplicatorLiveness == <>replicatorInstalled
EndToEndLiveness == <>docsReplicated

(***************************************************************************)
(* Safety: the partial-applied state is never silently treated as done.    *)
(* Whenever we are Connected and the control-plane collection is subscribed *)
(* but the replicator is not yet installed, InstallReplicator must still be *)
(* ENABLED (progress remains possible). This catches a reconciler that      *)
(* subscribes and then declares victory without installing the replicator.  *)
(***************************************************************************)

PartialApplyHasProgress ==
  (connState = "Connected" /\ subscribed /\ ~replicatorInstalled)
    => ENABLED InstallReplicator

\* docsReplicated is only ever reachable through an installed replicator.
ReplicationImpliesReplicator ==
  docsReplicated => replicatorInstalled
====
