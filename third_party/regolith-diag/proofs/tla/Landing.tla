---- MODULE Landing ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A commit lands when it becomes visible to readers, or when a rollback
\* discards it. Some things wait for exactly that moment: the key locks of a
\* pessimistic transaction that committed without waiting (commit_nowait)
\* are released then, and its ticket is completed then. Each of them is a
\* listener on the commit's slot (Descriptor::listen, Descriptor::land,
\* src/engine/commit/descriptor.rs).
\*
\* Any thread may be the one that publishes a slot: the commit pipeline has
\* no leader, and a thread helps every slot before its own
\* (CommitPipeline.tla). So the rule cannot be "the owner tells its
\* listeners". It is:
\*
\*     a listener is told exactly once, after the commit is visible or
\*     discarded, by whichever thread lands the slot.
\*
\* Think of a parcel counter. People who wait for a parcel put their names
\* on a sheet at the counter. Whoever brings the parcel in (any clerk) first
\* puts it on the shelf where everybody can see it, then takes the sheet,
\* closes it for good with the same move, and calls the names on it. Someone
\* who comes to sign later finds the sheet closed, which tells them the
\* parcel is already on the shelf: they help themselves. Several clerks may
\* carry the same parcel in at once; only one of them gets the sheet, the
\* others find it taken and empty.
\*
\* Tiny example of what the closing prevents. Clerk 1 takes the sheet, empty
\* so far, and leaves it open. A customer signs. Clerk 1 marks the parcel as
\* handled; clerk 2 sees that and does nothing. Nobody ever calls that name:
\* the transaction's key locks are never released, and every later writer of
\* those keys waits for ever.
\*
\* What this model checks cannot happen:
\*   - a listener is told twice (ToldAtMostOnce): its locks released under a
\*     transaction that took them again;
\*   - a listener is told before the commit is visible or discarded
\*     (ToldAfterLanding): a waiter that then reads would not find the write;
\*   - a listener is never told (EveryListenerTold), with a publisher that
\*     stops for good before it took the sheet: the next one tells.
\*
\* WHAT IS MODELLED, and the code each piece mirrors:
\*   landed    what readers can tell of the slot: nothing yet, visible (the
\*             horizon passed it, ReadHorizon::publish) or discarded (a
\*             rollback marked it FAILED)
\*   frontier  the published frontier passed the slot (Ring::pass_published):
\*             from then on no thread looks at the slot again
\*   sheet     the listeners registered on the slot (Descriptor::landing)
\*   shut      the sheet is closed (SHUT): a later listen is refused
\*   hand      the listeners a publisher took and has not told yet
\*   told      how often each listener was told
\*   early     a ghost: the listeners told while nothing had landed
\*   ppc       where each publisher is: start, land (it published the
\*             horizon), tell (it took the sheet), pass (it told them all),
\*             done, or stopped for good
\*   lpc       where each listener is: idle, registered, refused (it will
\*             tell itself) or done
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - How the slot got decided, written and synced: CommitPipeline.tla.
\*   - What a listener does when told. It only matters that it is once.
\*   - Other slots: each has a sheet of its own.
\*
\* A STATED LIMIT, which the last RED row shows rather than hides. A
\* publisher that stops for good after it took the sheet and before it
\* called every name keeps those names: nobody else has them. Threads of one
\* process do not die one at a time, so this is a thread that is not
\* scheduled, and its listeners are told when it runs again; until then they
\* wait. It is the same limit as a blocking caller behind a claimed round
\* (CommitPipeline.tla, StopHoldsClaim).
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one promise
\* it names):
\*   MC_Landing_Green               two publishers, one of which may stop
\*                                  before it takes the sheet, and two
\*                                  listeners that register at any moment.
\*   MC_Landing_Green_Discarded     the same for a slot a rollback discards.
\*   MC_Landing_Red_TakeNoShut      the sheet is taken and left open: a
\*                                  listener that registers afterwards is
\*                                  never told, EveryListenerTold breaks.
\*   MC_Landing_Red_ReadThenClear   the sheet is read and cleared in two
\*                                  steps: two publishers read the same
\*                                  names, ToldAtMostOnce breaks.
\*   MC_Landing_Red_TellBeforeHorizon
\*                                  the names are called before the commit is
\*                                  visible: ToldAfterLanding breaks.
\*   MC_Landing_Red_FrontierFirst   the frontier passes before the names are
\*                                  called: a publisher that stops between
\*                                  the two leaves them to nobody,
\*                                  EveryListenerTold breaks.
\*   MC_Landing_Red_StopAfterTake   the stated limit: the real code, with a
\*                                  publisher that stops holding the names
\*                                  it took. EveryListenerTold breaks.
\*
\* Lean, proofs/lean/Regolith/Landing.lean, proves "at most once" for any
\* number of publishers and listeners.

\* Numbers and the size of a set.
EXTENDS Naturals, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* The threads that publish the slot: its owner, a helper, a queue's poll.
  Publishers,
  \* The things that wait for the commit to land.
  Listeners,
  \* The publishers that may stop for good.
  CanStop,
  \* Whether one may stop anywhere (TRUE), or only before it takes the sheet.
  StopAnywhere,
  \* What the slot becomes: "visible" or "discarded".
  Outcome,
  \* "none" for the real code, or the name of one planted bug.
  Mutant

\* The bug names this model knows.
ASSUME Mutant \in {"none", "TakeNoShut", "ReadThenClear", "TellBeforeHorizon", "FrontierFirst"}
\* A slot is published or discarded.
ASSUME Outcome \in {"visible", "discarded"}
\* Only publishers stop, and one that never stops is left.
ASSUME CanStop \subseteq Publishers /\ CanStop # Publishers
\* The switch is a truth value.
ASSUME StopAnywhere \in BOOLEAN

\* The state that changes from step to step.
VARIABLES
  \* "no", "visible" or "discarded": what readers can tell of the slot.
  landed,
  \* Whether the published frontier passed the slot.
  frontier,
  \* The listeners registered on the slot.
  sheet,
  \* Whether the sheet is closed.
  shut,
  \* [Publishers -> SUBSET Listeners]: taken, not told yet.
  hand,
  \* [Listeners -> Nat]: how often each was told.
  told,
  \* Ghost: the listeners told while nothing had landed.
  early,
  \* [Publishers -> a step name]: where each publisher is.
  ppc,
  \* [Listeners -> a step name]: where each listener is.
  lpc

\* All of them, for "nothing else changes".
vars == <<landed, frontier, sheet, shut, hand, told, early, ppc, lpc>>

\* The start: a slot decided, written and synced, about to be published.
Init ==
  \* Nothing has landed, and the frontier is before the slot.
  /\ landed = "no" /\ frontier = FALSE
  \* The sheet is open and empty.
  /\ sheet = {} /\ shut = FALSE
  \* No publisher holds a name, and nobody was told.
  /\ hand = [p \in Publishers |-> {}] /\ told = [l \in Listeners |-> 0] /\ early = {}
  \* Every publisher is about to publish the slot; no listener has come yet.
  /\ ppc = [p \in Publishers |-> "start"] /\ lpc = [l \in Listeners |-> "idle"]

\* ---------------------------------------------------------------------------
\* A publisher (publish_upto, and roll_back for a slot it discards)
\* ---------------------------------------------------------------------------

\* The slot lands for readers: the horizon passes it, or the rollback marks
\* it failed. Idempotent: a second publisher changes nothing.
Lands == IF landed = "no" THEN Outcome ELSE landed

\* A publisher reaches the slot. One the frontier already passed has nothing
\* to do. Otherwise the horizon comes first (P3); the planted bug
\* TellBeforeHorizon calls the names first.
Start(p) ==
  \* The publisher is at the slot.
  /\ ppc[p] = "start"
  \* Has the frontier passed it?
  /\ IF frontier
       \* Yes: another thread published it. Nothing to do.
       THEN /\ ppc' = [ppc EXCEPT ![p] = "done"]
            \* Nothing lands by this step.
            /\ UNCHANGED landed
       \* No: the commit becomes visible (or is marked discarded) ...
       ELSE /\ landed' = IF Mutant = "TellBeforeHorizon" THEN landed ELSE Lands
            \* ... and the sheet comes next; the planted bug FrontierFirst
            \* passes the frontier first.
            /\ ppc' = [ppc EXCEPT ![p] = IF Mutant = "FrontierFirst" THEN "pass" ELSE "land"]
  \* Nothing else changes.
  /\ UNCHANGED <<frontier, sheet, shut, hand, told, early, lpc>>

\* The publisher takes the sheet and closes it, in one swap
\* (Stack::take(SHUT)). The planted bug TakeNoShut leaves it open; the
\* planted bug ReadThenClear reads the names and clears the sheet later.
Take(p) ==
  \* The publisher is at the sheet.
  /\ ppc[p] = "land"
  \* The names on it are in its hands.
  /\ hand' = [hand EXCEPT ![p] = sheet]
  \* The sheet is empty, unless the bug only read it.
  /\ sheet' = IF Mutant = "ReadThenClear" THEN sheet ELSE {}
  \* It is closed, unless a bug leaves it open.
  /\ shut' = (shut \/ Mutant \notin {"TakeNoShut", "ReadThenClear"})
  \* The names are called next.
  /\ ppc' = [ppc EXCEPT ![p] = "tell"]
  \* Nothing else changes.
  /\ UNCHANGED <<landed, frontier, told, early, lpc>>

\* The publisher tells one listener it took (Delivery::deliver).
Tell(p, l) ==
  \* It holds this name.
  /\ ppc[p] = "tell" /\ l \in hand[p]
  \* The listener is told once more.
  /\ told' = [told EXCEPT ![l] = @ + 1]
  \* The name leaves its hands.
  /\ hand' = [hand EXCEPT ![p] = @ \ {l}]
  \* Told before anything landed: noted.
  /\ early' = IF landed = "no" THEN early \cup {l} ELSE early
  \* Nothing else changes.
  /\ UNCHANGED <<landed, frontier, sheet, shut, ppc, lpc>>

\* Every name it took is called. The frontier comes next; under the planted
\* bug FrontierFirst it is already passed, and under TellBeforeHorizon the
\* commit becomes visible only now.
TellDone(p) ==
  \* Nothing is left in its hands.
  /\ ppc[p] = "tell" /\ hand[p] = {}
  \* The late horizon of the planted bug.
  /\ landed' = IF Mutant = "TellBeforeHorizon" THEN Lands ELSE landed
  \* On to the frontier, or done.
  /\ ppc' = [ppc EXCEPT ![p] = IF Mutant = "FrontierFirst" THEN "done" ELSE "pass"]
  \* Nothing else changes.
  /\ UNCHANGED <<frontier, sheet, shut, hand, told, early, lpc>>

\* The frontier passes the slot: no thread looks at it again.
Pass(p) ==
  \* The publisher is at the frontier.
  /\ ppc[p] = "pass"
  \* It passes.
  /\ frontier' = TRUE
  \* The planted bug ReadThenClear clears the sheet only here.
  /\ sheet' = IF Mutant = "ReadThenClear" THEN {} ELSE sheet
  \* The planted bug ReadThenClear closes it only here.
  /\ shut' = (shut \/ Mutant = "ReadThenClear")
  \* Done, or (the planted bug FrontierFirst) the sheet comes now.
  /\ ppc' = [ppc EXCEPT ![p] = IF Mutant = "FrontierFirst" THEN "land" ELSE "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<landed, hand, told, early, lpc>>

\* A publisher stops for good: before it takes the sheet, or anywhere when
\* the configuration says so (the stated limit).
Stop(p) ==
  \* It may stop, and has work left.
  /\ p \in CanStop /\ ppc[p] \notin {"done", "stopped"}
  \* Before the take, unless the configuration lets it stop anywhere.
  /\ StopAnywhere \/ ppc[p] \in {"start", "land"}
  \* It never takes another step.
  /\ ppc' = [ppc EXCEPT ![p] = "stopped"]
  \* Nothing else changes.
  /\ UNCHANGED <<landed, frontier, sheet, shut, hand, told, early, lpc>>

\* ---------------------------------------------------------------------------
\* A listener (Descriptor::listen, and its caller when it is refused)
\* ---------------------------------------------------------------------------

\* The listener puts its name on the sheet, with one push that fails on a
\* closed sheet.
Listen(l) ==
  \* It has not come yet.
  /\ lpc[l] = "idle"
  \* Is the sheet open?
  /\ IF ~shut
       \* Open: it is registered, and will be told.
       THEN /\ sheet' = sheet \cup {l}
            \* It waits to be told.
            /\ lpc' = [lpc EXCEPT ![l] = "registered"]
       \* Closed: the commit landed already, and its caller tells it.
       ELSE /\ UNCHANGED sheet
            \* It tells itself next.
            /\ lpc' = [lpc EXCEPT ![l] = "refused"]
  \* Nothing else changes.
  /\ UNCHANGED <<landed, frontier, shut, hand, told, early, ppc>>

\* A refused listener is told by its own caller (`if !listen(..)` then
\* release, src/transaction/nowait.rs).
Self(l) ==
  \* It was refused.
  /\ lpc[l] = "refused"
  \* It is told, by its caller.
  /\ told' = [told EXCEPT ![l] = @ + 1]
  \* Told before anything landed: noted.
  /\ early' = IF landed = "no" THEN early \cup {l} ELSE early
  \* It is through.
  /\ lpc' = [lpc EXCEPT ![l] = "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<landed, frontier, sheet, shut, hand, ppc>>

\* ---------------------------------------------------------------------------
\* The whole
\* ---------------------------------------------------------------------------

\* Every step a publisher takes of its own accord.
PStep(p) ==
  \* Reach the slot, take the sheet, finish telling, pass the frontier.
  \/ Start(p) \/ Take(p) \/ TellDone(p) \/ Pass(p)
  \* Tell one of the listeners it took.
  \/ \E l \in Listeners : Tell(p, l)

\* Every step a listener takes.
LStep(l) == Listen(l) \/ Self(l)

\* One step of the run.
Next ==
  \* A publisher's step, or its stopping.
  \/ \E p \in Publishers : PStep(p) \/ Stop(p)
  \* A listener's step.
  \/ \E l \in Listeners : LStep(l)

\* The run: the start, steps, and every thread that can go on does go on.
\* Stopping is never forced, and a stopped publisher has no step to take.
Spec ==
  \* The start and the steps.
  /\ Init /\ [][Next]_vars
  \* A publisher that can go on does.
  /\ \A p \in Publishers : WF_vars(PStep(p))
  \* A listener that can go on does.
  /\ \A l \in Listeners : WF_vars(LStep(l))

\* ---------------------------------------------------------------------------
\* What must be true
\* ---------------------------------------------------------------------------

\* Every variable holds the kind of thing its comment says.
TypeOK ==
  \* What landed, and the two flags.
  /\ landed \in {"no", "visible", "discarded"} /\ frontier \in BOOLEAN /\ shut \in BOOLEAN
  \* The sheet and the hands hold listeners.
  /\ sheet \subseteq Listeners /\ hand \in [Publishers -> SUBSET Listeners]
  \* Counts and the ghost.
  /\ told \in [Listeners -> Nat] /\ early \subseteq Listeners
  \* Every publisher is at a step this model names.
  /\ ppc \in [Publishers -> {"start", "land", "tell", "pass", "done", "stopped"}]
  \* Every listener too.
  /\ lpc \in [Listeners -> {"idle", "registered", "refused", "done"}]

\* A listener is never told twice. Rules out two publishers both calling
\* the names they read off the same sheet.
ToldAtMostOnce == \A l \in Listeners : told[l] <= 1

\* A listener is told only once the commit is visible or discarded. Rules
\* out calling the names before the parcel is on the shelf.
ToldAfterLanding == early = {}

\* A sheet that is closed stays empty: a name on it would never be called.
ClosedSheetEmpty == shut => sheet = {}

\* Every listener is told, in every run: registered in time and called by
\* whoever took the sheet, or refused and told by its caller. With a
\* publisher that stops before it takes the sheet.
EveryListenerTold == \A l \in Listeners : <>(told[l] >= 1)

====
