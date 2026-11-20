---- MODULE SettlementApproval ----
\* lean: Identity.EnrollmentGrant.settlementStep, Identity.EnrollmentGrant.afterRemoval, Identity.EnrollmentGrant.restorationStep
\* crates: gents-store
\* models: Identity.EnrollmentGrant.grant, Identity.EnrollmentGrant.settle, Identity.EnrollmentGrant.takeOver, Identity.EnrollmentGrant.restoreBy
(***************************************************************************)
(* The owner's owed-settlement pass withdrawing a member's Node            *)
(* `administrator` relation while an approval of that member runs          *)
(* (gents#3831). The withdrawal is owed (`oweRevoke`): the member holds    *)
(* the relation, a retired receipt owes its withdrawal and no live receipt *)
(* accounts for it.                                                        *)
(*                                                                          *)
(* The approval (`grant_with`) reads what the member holds and the         *)
(* receipts, writes its receipt in one transaction that also closes the    *)
(* owed receipts when it records the relation (Lean `grant`), then issues  *)
(* every tuple its receipt recorded. The pass (`settle_owed_withdrawals`)  *)
(* reads the receipts and decides (Lean `settlementStep`), removes the     *)
(* relation, reads the receipts again and decides (Lean `afterRemoval`),   *)
(* issues the relation again when told to and closes the owed receipts.    *)
(* Each step is one atomic read or write.                                  *)
(*                                                                          *)
(* The pass's reissue can fail (`ReissueFails`). The approval's takeover   *)
(* owes the relation's restoration (Lean `takeOver`) in its receipt write, *)
(* and a later pass restores it (Lean `restorationStep`, `Restore`).       *)
(*                                                                          *)
(* `Restores` is the safeguard: without it a failed reissue leaves the     *)
(* approved member without the relation and nothing owed.                  *)
(***************************************************************************)
EXTENDS LeanDecision_Identity_EnrollmentGrant

CONSTANTS Reissues, ReissueFails, Restores

VARIABLES held, owed, live, approver, seen, settler, decision, restore
vars == <<held, owed, live, approver, seen, settler, decision, restore>>

TypeOK ==
  /\ held \in BOOLEAN /\ owed \in BOOLEAN /\ live \in BOOLEAN
  /\ approver \in {"idle", "read", "recorded", "done"}
  /\ seen \in [held : BOOLEAN, owed : BOOLEAN, live : BOOLEAN]
  /\ settler \in {"idle", "decided", "removed", "reissue", "close", "done"}
  /\ restore \in BOOLEAN

Init ==
  /\ held = TRUE /\ owed = TRUE /\ live = FALSE
  /\ \/ approver = "idle" /\ seen = [held |-> TRUE, owed |-> TRUE, live |-> FALSE]
     \/ approver = "read" /\ seen = [held |-> TRUE, owed |-> FALSE, live |-> TRUE]
  /\ settler = "idle" /\ decision = "none" /\ restore = FALSE

ApproverRead ==
  /\ approver = "idle"
  /\ seen' = [held |-> held, owed |-> owed, live |-> live]
  /\ approver' = "read"
  /\ UNCHANGED <<held, owed, live, settler, decision, restore>>

\* Lean `record`: the relation is recorded unless held independently.
Records == ~seen.held \/ seen.live \/ seen.owed \/ live \/ owed

ApproverRecord ==
  /\ approver = "read"
  /\ IF Records
       THEN live' = TRUE /\ owed' = FALSE /\ restore' = (restore \/ (Restores /\ owed))
       ELSE UNCHANGED <<live, owed, restore>>
  /\ approver' = "recorded"
  /\ UNCHANGED <<held, seen, settler, decision>>

ApproverIssue ==
  /\ approver = "recorded"
  /\ IF live THEN held' = TRUE ELSE UNCHANGED held
  /\ approver' = "done"
  /\ UNCHANGED <<owed, live, seen, settler, decision, restore>>

SettlerRead ==
  /\ settler = "idle"
  /\ decision' = settlementStep(owed, live)
  /\ settler' = "decided"
  /\ UNCHANGED <<held, owed, live, approver, seen, restore>>

SettlerAct ==
  /\ settler = "decided"
  /\ CASE decision = "issue" -> held' = FALSE /\ owed' = owed /\ settler' = "removed"
       [] decision = "close" -> held' = held /\ owed' = FALSE /\ settler' = "done"
       [] OTHER -> held' = held /\ owed' = owed /\ settler' = "done"
  /\ UNCHANGED <<live, approver, seen, decision, restore>>

SettlerReread ==
  /\ settler = "removed"
  /\ settler' = IF Reissues /\ afterRemoval(live) = "reissue" THEN "reissue" ELSE "close"
  /\ UNCHANGED <<held, owed, live, approver, seen, decision, restore>>

SettlerReissue ==
  /\ settler = "reissue"
  /\ IF ReissueFails THEN UNCHANGED held ELSE held' = TRUE
  /\ settler' = "close"
  /\ UNCHANGED <<owed, live, approver, seen, decision, restore>>

SettlerClose ==
  /\ settler = "close"
  /\ owed' = FALSE /\ settler' = "done"
  /\ UNCHANGED <<held, live, approver, seen, decision, restore>>

\* A later pass of a manager over an owed restoration.
Restore ==
  /\ restore
  /\ settler = "done"
  /\ IF restorationStep(owed, held) = "restore" THEN held' = TRUE ELSE UNCHANGED held
  /\ restore' = FALSE
  /\ UNCHANGED <<owed, live, approver, seen, settler, decision>>

Next ==
  \/ Restore
  \/ ApproverRead \/ ApproverRecord \/ ApproverIssue
  \/ SettlerRead \/ SettlerAct \/ SettlerReread \/ SettlerReissue \/ SettlerClose

Spec == Init /\ [][Next]_vars

Quiet == approver \in {"idle", "done"} /\ settler = "done"

\* The relation ends held exactly when a live receipt accounts for it, once
\* no restoration is owed.
HeldExactlyWhenAccounted == (Quiet /\ ~restore) => (held = live)

\* An approved member's relation is held or its restoration is owed: never
\* silently lost.
NeverSilentlyLost == (Quiet /\ live) => (held \/ restore)

\* Nothing stays owed once the pass and the approval are done.
NothingStaysOwed == Quiet => ~owed
====
