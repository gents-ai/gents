---- MODULE ExclusivePopoverLease ----
\* lean: none
\* crates: none
\* paths: apps/gents-desktop/src/ui/hooks/useExclusivePopover.ts, apps/gents-desktop/tests/bombadil/spec.ts
(***************************************************************************)
(* The desktop shell's one-popup lease (`useExclusivePopover`) for two     *)
(* owners, A and B, each a Base UI popover, combobox or dialog.            *)
(*                                                                          *)
(* Actors and their atomic steps, from the hook and Base UI 1.8:           *)
(*  - Request(o): the owner's `onOpenChange(true)`. With no lease holder   *)
(*    the owner takes the lease and its open state is set; with a holder   *)
(*    the owner is queued and the holder is asked to close              *)
(*    (`closeForPeer`): the holder's open state is cleared, and the lease *)
(*    is released at once unless the holder awaits its unmount.           *)
(*  - Commit(o): React commits the owner's open state. Base UI marks the  *)
(*    popup mounted in that render.                                        *)
(*  - Attach(o): the popup element attaches (`popupRef`). Base UI renders  *)
(*    it through a portal whose node is set in a layout effect, so the    *)
(*    attach follows the commit synchronously (`MountLag = FALSE`): no    *)
(*    other event runs between them. `MountLag = TRUE` lets other steps   *)
(*    interleave there.                                                    *)
(*  - Detach(o): a closed popup finishes its exit and unmounts; the lease *)
(*    is released and a queued owner takes it.                             *)
(* `Holds` is when a holder awaits its unmount: an attached popup         *)
(* (`"attached"`, the hook) or also a committed open (`"committed"`).      *)
(*                                                                          *)
(* `AtMostOneMounted`: at most one popup element is in the document, the   *)
(* Bombadil property `desktop_shell_does_not_stack_dialogs`.              *)
(* `RequestServed`: a queued request is eventually served.                *)
(*                                                                          *)
(* GREEN `MCExclusivePopoverLease` (the hook under Base UI's synchronous  *)
(* attach) and `MCExclusivePopoverLeaseCommitted` (a lease held from the  *)
(* committed open also holds when the attach lags). RED: `MountLag` (the  *)
(* hook's lease if the attach could lag its commit) and `ReleaseOnClose`  *)
(* (a lease released when its holder is asked to close, not on unmount).  *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
  MountLag,       \* other steps may run between a commit and its attach
  Holds,          \* "attached" | "committed"
  ReleaseOnClose  \* the lease is released when its holder is asked to close

Owners == {"A", "B"}

VARIABLES
  want,       \* the owner's open state as last set
  shown,      \* the owner's open state as last committed
  mounted,    \* Base UI keeps the owner's popup mounted (open committed, exit not done)
  attached,   \* the owner's popup element is in the document
  holder,     \* the lease holder, or "none"
  queued,     \* the owner waiting for the lease, or "none"
  requests    \* requests still to make, per owner

vars == <<want, shown, mounted, attached, holder, queued, requests>>

Init ==
  /\ want = [o \in Owners |-> FALSE]
  /\ shown = [o \in Owners |-> FALSE]
  /\ mounted = [o \in Owners |-> FALSE]
  /\ attached = [o \in Owners |-> FALSE]
  /\ holder = "none"
  /\ queued = "none"
  /\ requests = [o \in Owners |-> 1]

\* A commit whose attach has not run yet.
Lagging(o) == mounted[o] /\ ~attached[o]

\* With `MountLag = FALSE` an attach follows its commit before any other step.
Quiet == MountLag \/ \A o \in Owners : ~Lagging(o)

Awaits(o) ==
  attached[o] \/ (Holds = "committed" /\ mounted[o])

Request(o) ==
  /\ Quiet
  /\ requests[o] > 0 /\ holder # o
  /\ requests' = [requests EXCEPT ![o] = @ - 1]
  /\ IF holder = "none"
       THEN /\ holder' = o /\ want' = [want EXCEPT ![o] = TRUE]
            /\ UNCHANGED queued
       ELSE IF ~ReleaseOnClose /\ Awaits(holder)
         THEN /\ want' = [want EXCEPT ![holder] = FALSE]
              /\ queued' = o /\ UNCHANGED holder
         ELSE /\ want' = [want EXCEPT ![holder] = FALSE, ![o] = TRUE]
              /\ holder' = o /\ queued' = "none"
  /\ UNCHANGED <<shown, mounted, attached>>

Commit(o) ==
  /\ Quiet
  /\ shown[o] # want[o]
  /\ shown' = [shown EXCEPT ![o] = want[o]]
  /\ mounted' = [mounted EXCEPT ![o] = @ \/ want[o]]
  /\ UNCHANGED <<want, attached, holder, queued, requests>>

\* Base UI keeps a popup mounted from its open commit through its exit, so
\* a popup whose open committed attaches even if it closed since.
Attach(o) ==
  /\ mounted[o] /\ ~attached[o]
  /\ attached' = [attached EXCEPT ![o] = TRUE]
  /\ UNCHANGED <<want, shown, mounted, holder, queued, requests>>

Detach(o) ==
  /\ Quiet
  /\ attached[o] /\ ~want[o] /\ ~shown[o]
  /\ attached' = [attached EXCEPT ![o] = FALSE]
  /\ mounted' = [mounted EXCEPT ![o] = FALSE]
  /\ IF holder = o
       THEN IF queued # "none"
              THEN /\ holder' = queued /\ queued' = "none"
                   /\ want' = [want EXCEPT ![queued] = TRUE]
              ELSE /\ holder' = "none" /\ UNCHANGED <<queued, want>>
       ELSE UNCHANGED <<holder, queued, want>>
  /\ UNCHANGED <<shown, requests>>

Next == \E o \in Owners :
  Request(o) \/ Commit(o) \/ Attach(o) \/ Detach(o)

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

TypeOK ==
  /\ want \in [Owners -> BOOLEAN]
  /\ shown \in [Owners -> BOOLEAN]
  /\ mounted \in [Owners -> BOOLEAN]
  /\ attached \in [Owners -> BOOLEAN]
  /\ holder \in Owners \cup {"none"}
  /\ queued \in Owners \cup {"none"}
  /\ requests \in [Owners -> 0..1]

AtMostOneMounted == Cardinality({o \in Owners : attached[o]}) <= 1

RequestServed == queued # "none" ~> queued = "none"
====
