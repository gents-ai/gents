---- MODULE OwnToolsRebinding ----
\* lean: RuntimeReconcile.ToolRebinding.answer
\* crates: gents, gents-loop, gents-tools, gents-toolcalls
\* models: CompletionLoop.run
EXTENDS Naturals, LeanDecision_RuntimeReconcile_ToolRebinding

\* A request whose config call commits a change to its own Tools document,
\* the control watcher that resolves documents into generations, and another
\* writer of the same Tools. The request's first turn is dispatched with the
\* tools of the generation that claimed it (version 0); its config call
\* commits version 1, then observes each generation the watcher activates and
\* answers with Lean `answer`; the request's next turn is dispatched once the
\* turn's tool calls end. The watcher reads the document, may fail, and
\* activates a generation for what it read; a generation also activates for
\* other documents' changes, so one read before the commit can activate after
\* it. A generation runs the agent unless the version it read cannot run.
\*
\* Safeguards, each removed by one constant:
\*   CheckCurrency    a generation is adopted only when its Tools version is
\*                    current (Lean `configurationAdmission`);
\*   StageAtBoundary  the rebuilt toolset is staged and taken at the next turn
\*                    boundary (Lean `CompletionLoop.run`), not installed while
\*                    the turn's tool calls still run;
\*   AdoptPublished   the config call adopts a generation the watcher
\*                    published, never the document it wrote;
\*   Bounded          the config call stops waiting after `Bound` observations.

CONSTANTS CheckCurrency, StageAtBoundary, AdoptPublished, Bounded, Bound, MaxGen

None == 9

\* `unrunnable`: the committed version cannot run (its tools do not build).
VARIABLES unrunnable, doc, resolving, active, published, phase, dispatched,
          toolset, builtGen, staged, written, receipt, waited
vars == <<unrunnable, doc, resolving, active, published, phase, dispatched,
          toolset, builtGen, staged, written, receipt, waited>>

Runs(version) == ~(unrunnable /\ version = 1)

Init ==
  /\ unrunnable \in BOOLEAN
  /\ doc = 0
  /\ resolving = None
  /\ active = [gen |-> 0, version |-> 0, runs |-> TRUE]
  /\ published = {0}
  /\ phase = "turn"
  /\ dispatched = 0
  /\ toolset = 0
  /\ builtGen = 0
  /\ staged = None
  /\ written = None
  /\ receipt = "none"
  /\ waited = 0

\* The watcher reads the documents of a pending reconcile.
StartResolve ==
  /\ resolving = None /\ active.gen < MaxGen
  /\ resolving' = doc
  /\ UNCHANGED <<doc, active, published, phase, dispatched, toolset, builtGen,
                 staged, written, receipt, waited>>

Activate ==
  /\ resolving # None
  /\ active' = [gen |-> active.gen + 1, version |-> resolving, runs |-> Runs(resolving)]
  /\ published' = IF Runs(resolving) THEN published \cup {resolving} ELSE published
  /\ resolving' = None
  /\ UNCHANGED <<doc, phase, dispatched, toolset, builtGen, staged, written,
                 receipt, waited>>

FailResolve ==
  /\ resolving # None
  /\ resolving' = None
  /\ UNCHANGED <<doc, active, published, phase, dispatched, toolset, builtGen,
                 staged, written, receipt, waited>>

\* Another writer changes the same Tools after the request's commit.
OtherWrite ==
  /\ written # None /\ doc < 2
  /\ doc' = doc + 1
  /\ UNCHANGED <<resolving, active, published, phase, dispatched, toolset,
                 builtGen, staged, written, receipt, waited>>

\* The first turn's tool calls start; one is the config call.
StartTools ==
  /\ phase = "turn" /\ written = None
  /\ phase' = "call"
  /\ UNCHANGED <<doc, resolving, active, published, dispatched, toolset,
                 builtGen, staged, written, receipt, waited>>

Commit ==
  /\ phase = "call" /\ written = None
  /\ doc' = doc + 1
  /\ written' = doc + 1
  /\ IF AdoptPublished
       THEN UNCHANGED <<staged, receipt>>
       ELSE /\ staged' = doc + 1
            /\ receipt' = "applied"
  /\ UNCHANGED <<resolving, active, published, phase, dispatched, toolset,
                 builtGen, waited>>

Publication ==
  IF active.gen <= builtGen THEN "older"
  ELSE IF active.runs THEN "runs" ELSE "unavailable"

Expired == Bounded /\ waited >= Bound

Answer ==
  answer(Publication, TRUE, (~CheckCurrency) \/ active.version = doc, Expired)

Observe ==
  /\ phase = "call" /\ written # None /\ receipt = "none"
  /\ CASE Answer = "rebind" ->
            /\ receipt' = "applied"
            /\ builtGen' = active.gen
            /\ IF StageAtBoundary
                 THEN staged' = active.version /\ UNCHANGED toolset
                 ELSE toolset' = active.version /\ UNCHANGED staged
            /\ UNCHANGED waited
       [] Answer = "keep" ->
            /\ receipt' = "kept"
            /\ UNCHANGED <<builtGen, staged, toolset, waited>>
       [] Answer = "wait" ->
            /\ Bounded
            /\ waited' = waited + 1
            /\ UNCHANGED <<receipt, builtGen, staged, toolset>>
  /\ UNCHANGED <<doc, resolving, active, published, phase, dispatched, written>>

EndTools ==
  /\ phase = "call" /\ receipt # "none"
  /\ phase' = "boundary"
  /\ UNCHANGED <<doc, resolving, active, published, dispatched, toolset,
                 builtGen, staged, written, receipt, waited>>

\* The next turn takes a staged toolset and is dispatched with it.
Dispatch ==
  /\ phase = "boundary"
  /\ toolset' = IF staged # None THEN staged ELSE toolset
  /\ dispatched' = IF staged # None THEN staged ELSE toolset
  /\ staged' = None
  /\ phase' = "done"
  /\ UNCHANGED <<doc, resolving, active, published, builtGen, written, receipt,
                 waited>>

Request == StartTools \/ Commit \/ Observe \/ EndTools \/ Dispatch
Watcher == StartResolve \/ Activate \/ FailResolve
Next == (Request \/ Watcher \/ OtherWrite) /\ UNCHANGED unrunnable

Spec ==
  /\ Init /\ [][Next]_vars
  /\ WF_vars(Request /\ UNCHANGED unrunnable)
  /\ WF_vars(StartResolve /\ UNCHANGED unrunnable)
  /\ WF_vars((Activate \/ FailResolve) /\ UNCHANGED unrunnable)

\* Every turn is dispatched with a toolset a generation ran the agent from,
\* which a fresh request would get.
OfferedWasPublished == dispatched \in published

\* A turn's toolset does not change while its tool calls run.
TurnToolsetStable == phase \in {"turn", "call"} => toolset = dispatched

\* An applied receipt means the next turn holds the committed change.
ReceiptTruthful == (receipt = "applied" /\ phase = "done") => dispatched >= written

\* The config call always answers.
Answered == <>(receipt # "none")
====
