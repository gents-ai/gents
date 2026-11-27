---- MODULE SessionPatchFence ----
\* lean: ClientDataPublication.reach
\* crates: gents-observe, gents-desktop-bridge
\* The fence of a selected session's patch (`ClientRuntime::refresh_local_request`,
\* `observe/projection_store.rs`) beside the observer's session scope reloads.
\* One node's store.
\*
\* Runtime. The node's runtime commits projected writes to the rows of each
\* session scope (`db`). A commit's change event names its scope and is
\* delivered some time after the commit is visible to a read. `Renew` is a
\* write the projection does not show (an execution-lease renewal): it emits
\* an event without a projected version. At most `K` renewals land while one
\* patch is being read. Sessions in `Quiet` are neither written nor renewed.
\*
\* Observer. A delivered event queues its scope. The queued scopes start the
\* node's one partial read, which captures the reload fence, reads every
\* queued scope and publishes them unless a publication since its capture
\* rejected it; a rejected read's scopes are queued again.
\*
\* Patch. A reader captures the patch fence, reads one session scope and
\* merges what it read. A rejected patch is read again by its reader; every
\* step that rejects it either queued its own read or published newer rows.
\* With `RejectResyncs` it also asks the observer for the node, which reloads
\* every scope (before the change). A
\* patch that would publish the rows the store already shows, after a patch
\* of that scope has published, publishes nothing (the patch cache).
\*
\* Fences. A step rejects the patches in flight that it reaches. With
\* `Fence = "scope"` what a step reaches is Lean `ClientDataPublication.reach`,
\* read from the generated table: a scope event, a scope reload and a patch
\* reach the patches of the scope they name. With `Fence = "store"` every
\* event and every publication reaches every patch (before the change). With
\* `Fence = "none"` nothing does. `PatchMovesScope` and `ReloadMovesScope`
\* FALSE take a patch's or a scope reload's publication off the patch fence.
EXTENDS Naturals, FiniteSets, LeanDecision_ClientDataPublication

CONSTANTS
  Sessions,         \* session scopes of the node
  Quiet,            \* scopes no runtime write or renewal touches
  Readers,          \* concurrent patch reads
  MaxWrites,        \* projected writes per scope
  K,                \* renewals per patch read
  Fence,            \* "scope", "store" or "none"
  PatchMovesScope,  \* an accepted patch rejects the patches of its scope
  ReloadMovesScope, \* a scope reload rejects the patches of its scope
  RejectResyncs     \* a rejected patch asks the observer for the node

ASSUME /\ Quiet \subseteq Sessions
       /\ Fence \in {"scope", "store", "none"}
       /\ PatchMovesScope \in BOOLEAN /\ ReloadMovesScope \in BOOLEAN
       /\ RejectResyncs \in BOOLEAN

None == [scope |-> "none"]
Idle == [scopes |-> {}]

VARIABLES
  db,        \* scope -> projected version committed on the runtime
  events,    \* scopes with an undelivered change event
  shown,     \* scope -> version the store shows
  owed,      \* scopes queued for the observer's reload
  reload,    \* the scope reload in flight; no scopes when there is none
  patch,     \* reader -> its patch read in flight, or None
  renewals,  \* reader -> renewals landed during its read
  patched    \* scopes a patch has published or found current

vars == <<db, events, shown, owed, reload, patch, renewals, patched>>

Init ==
  /\ db = [s \in Sessions |-> 0]
  /\ events = {}
  /\ shown = [s \in Sessions |-> 0]
  /\ owed = {}
  /\ reload = Idle
  /\ patch = [r \in Readers |-> None]
  /\ renewals = [r \in Readers |-> 0]
  /\ patched = {}

\* Whether a step of kind `step` naming the scopes `named` rejects a patch of
\* scope `carried`.
Reaches(step, named, carried) ==
  CASE Fence = "none" -> FALSE
    [] Fence = "store" -> TRUE
    [] OTHER ->
         \/ reach(step) = "whole"
         \/ reach(step) = "named" /\ carried \in named

\* The patches in flight after a step that reaches them, except reader `own`'s.
Rejecting(step, named, own) ==
  [r \in Readers |->
    IF r # own /\ patch[r] # None /\ Reaches(step, named, patch[r].scope)
    THEN [patch[r] EXCEPT !.stale = TRUE]
    ELSE patch[r]]

Write(s) ==
  /\ s \notin Quiet /\ db[s] < MaxWrites
  /\ db' = [db EXCEPT ![s] = @ + 1]
  /\ events' = events \cup {s}
  /\ UNCHANGED <<shown, owed, reload, patch, renewals, patched>>

Renew(s, r) ==
  /\ s \notin Quiet /\ patch[r] # None /\ renewals[r] < K
  /\ renewals' = [renewals EXCEPT ![r] = @ + 1]
  /\ events' = events \cup {s}
  /\ UNCHANGED <<db, shown, owed, reload, patch, patched>>

Deliver(s) ==
  /\ s \in events
  /\ events' = events \ {s}
  /\ owed' = owed \cup {s}
  /\ patch' = Rejecting("scope_event", {s}, "none")
  /\ UNCHANGED <<db, shown, reload, renewals, patched>>

ReloadStart ==
  /\ owed # {} /\ reload = Idle
  /\ owed' = {}
  /\ reload' = [scopes |-> owed, value |-> db, stale |-> FALSE]
  /\ UNCHANGED <<db, events, shown, patch, renewals, patched>>

ReloadEnd ==
  /\ reload # Idle
  /\ reload' = Idle
  /\ IF reload.stale
     THEN /\ owed' = owed \cup reload.scopes
          /\ UNCHANGED <<shown, patch>>
     ELSE /\ shown' = [s \in Sessions |->
                        IF s \in reload.scopes THEN reload.value[s] ELSE shown[s]]
          /\ patch' = IF ReloadMovesScope
                      THEN Rejecting("scope_reload", reload.scopes, "none")
                      ELSE patch
          /\ UNCHANGED owed
  /\ UNCHANGED <<db, events, renewals, patched>>

PatchStart(r, s) ==
  /\ patch[r] = None
  /\ patch' = [patch EXCEPT ![r] = [scope |-> s, value |-> db[s], stale |-> FALSE]]
  /\ renewals' = [renewals EXCEPT ![r] = 0]
  /\ UNCHANGED <<db, events, shown, owed, reload, patched>>

\* A patch that publishes moves the store: it rejects the reload in flight and
\* the patches it reaches.
PatchEnd(r) ==
  LET p == patch[r]
      cached == p.scope \in patched /\ p.value = shown[p.scope]
  IN
  /\ p # None
  /\ IF p.stale
     THEN /\ owed' = IF RejectResyncs THEN Sessions ELSE owed
          /\ patch' = [patch EXCEPT ![r] = None]
          /\ UNCHANGED <<shown, reload, patched>>
     ELSE /\ patched' = patched \cup {p.scope}
          /\ IF cached
             THEN /\ patch' = [patch EXCEPT ![r] = None]
                  /\ UNCHANGED <<shown, reload>>
             ELSE /\ shown' = [shown EXCEPT ![p.scope] = p.value]
                  /\ reload' = IF reload = Idle THEN Idle
                               ELSE [reload EXCEPT !.stale = TRUE]
                  /\ patch' = [(IF PatchMovesScope
                                THEN Rejecting("patch", {p.scope}, r)
                                ELSE patch) EXCEPT ![r] = None]
          /\ UNCHANGED owed
  /\ UNCHANGED <<db, events, renewals>>

Next ==
  \/ \E s \in Sessions : Write(s) \/ Deliver(s)
  \/ ReloadStart
  \/ \E s \in Sessions, r \in Readers : Renew(s, r) \/ PatchStart(r, s)
  \/ ReloadEnd
  \/ \E r \in Readers : PatchEnd(r)

\* A reader keeps reading the quiet scopes, as the open session's view does.
Fairness ==
  /\ \A s \in Sessions : WF_vars(Deliver(s))
  /\ WF_vars(ReloadStart) /\ WF_vars(ReloadEnd)
  /\ \A r \in Readers :
       /\ WF_vars(PatchEnd(r))
       /\ \A s \in Quiet : SF_vars(PatchStart(r, s))

Spec == Init /\ [][Next]_vars /\ Fairness

----

TypeOK ==
  /\ db \in [Sessions -> 0..MaxWrites]
  /\ shown \in [Sessions -> 0..MaxWrites]
  /\ events \subseteq Sessions /\ owed \subseteq Sessions /\ patched \subseteq Sessions
  /\ renewals \in [Readers -> 0..K]
  /\ reload.scopes \subseteq Sessions
  /\ \A r \in Readers : patch[r] = None \/ patch[r].scope \in Sessions

\* NoOlderRow: neither a patch nor a reload replaces a scope's rows with older ones.
NoOlderRow == [][\A s \in Sessions : shown'[s] >= shown[s]]_vars

\* Convergence: the shown version eventually stays the runtime's version.
Convergence == \A s \in Sessions : <>[](shown[s] = db[s])

\* QuietPatchPublishes: a quiet scope's patch is accepted, however many events
\* of other scopes arrive while it is read.
QuietPatchPublishes == \A s \in Quiet : <>(s \in patched)

====
