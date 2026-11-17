---- MODULE TranscriptScrollObservation ----
\* lean: none
\* crates: none
\* paths: apps/gents-desktop/src/ui/lib/transcriptList.ts, apps/gents-desktop/tests/playwright/transcript-scroll-cost.spec.ts, apps/gents-desktop/tests/playwright/transcript-wheel.spec.ts, apps/gents-desktop/tests/ui-harness/desktopHarness.ts, apps/gents-desktop/src/ui/screens/StreamText.tsx
(***************************************************************************)
(* What two desktop Playwright tests observe of the transcript.            *)
(*                                                                         *)
(* Chunk side (transcript-scroll-cost.spec.ts, the following test). The    *)
(* driver pauses the fake clock, streams one chunk and steps the clock     *)
(* (`page.clock.runFor(16)`). Actors: the harness notify, a fake timer     *)
(* (tests/ui-harness/desktopHarness.ts:1986) the first step fires; its     *)
(* delivery, the session read and the React commit (Commit); the reply's   *)
(* reveal, an animation-frame loop (src/ui/screens/StreamText.tsx:40-58)   *)
(* the paused clock runs only on a driver step (part of DriverStep); the   *)
(* list heights the revealed text commits (CommitHeight); reconciles from  *)
(* list and composer size changes (ExtraReconcile, transcriptList.ts:220,  *)
(* :933-941); the test's growth observer (Observe). After every commit the *)
(* owner reconciles (:915-917); following at rest it writes the foot       *)
(* unless scrollTop is within 1 px of it (:451-457).                       *)
(* Premises: from the notify to the commit no fake timer runs, so the      *)
(* commit interleaves with any number of driver steps; after the commit    *)
(* the reveal's frames are the only fake timer the growth waits on. A      *)
(* write leaves scrollTop at the foot it wrote. The owner's own write is   *)
(* not read as the reader's scroll (:540-544), so it arms no rest and no   *)
(* rest timer writes here.                                                 *)
(* The first test's unseen chunk, a reply growing in a row that is not     *)
(* drawn whose window closes on its commit, is outside the model.          *)
(*                                                                         *)
(* Wheel side (transcript-wheel.spec.ts, the long-output test). The click  *)
(* opens the step's collapsible panel (ToolStep in @gents/ui, a Base UI    *)
(* Collapsible.Panel): it renders in its starting style, height 0 with     *)
(* overflow hidden, and its height transition starts a frame later         *)
(* (PanelStarts). Until the transition reaches the pointer, the output box *)
(* has its layout rect but is clipped: the hit target at that rect is      *)
(* outside the panel. Once it does, the box is the hit target at its own   *)
(* rect (PanelOpens). The output's text wait and the bounding box both     *)
(* pass while the box is clipped. Premise: the transcript holds still      *)
(* between the rect read and the wheel, so the box's rect does not move.   *)
(* A Playwright action that scrolls (hover, click, scrolling into view) is *)
(* not a legal driver step between Measure and Wheel: its scroll is not    *)
(* the reader's, so the owner writes its place back and the box leaves     *)
(* the measured point.                                                     *)
(*                                                                         *)
(* Guards (each red config turns one off):                                 *)
(*   WaitForGrowth     the driver steps once to fire the notify, waits for *)
(*                     the commit, steps one frame at a time until the     *)
(*                     growth, then steps the two counted frames; without  *)
(*                     it the window closes after K steps (the fake-time   *)
(*                     deadline and frame cap before the fix).             *)
(*   NotifyStep        that first step; without it the notify never fires. *)
(*   StepAfterCommit   the frames until the growth; without them the       *)
(*                     reveal never runs.                                  *)
(*   WaitForHitTarget  the driver moves and wheels only once the box is    *)
(*                     the hit target at the pointer.                      *)
(*   FootGuard         the owner's 1 px guard.                             *)
(*                                                                         *)
(* FairSpec makes the drivers', the page's and the panel's steps weakly    *)
(* fair. Size-change reconciles and the fake-time budget are not: one is   *)
(* optional environment, the other a deadline the fix removed.             *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Heights,          \* list heights one chunk commits
  Extra,            \* reconciles from size changes during the window
  K,                \* driver steps the fake-time budget allows
  WaitForGrowth, NotifyStep, StepAfterCommit, WaitForHitTarget, FootGuard

Foots == 0..Heights

VARIABLES
  phase,    \* the chunk driver
  steps,    \* fake-clock steps taken, counted up to K
  frames,   \* counted frames after the growth
  chunk,    \* "queued": the notify timer is armed
  revealed, \* list heights the reveal's frames have shown
  heights,  \* list heights committed: the foot
  top,      \* scrollTop
  writes,   \* owner writes per foot
  extra,
  grown,    \* the window saw scrollHeight pass its start
  wstep,    \* the wheel driver
  panel,
  inside    \* the wheel's hit target was the box

chunkVars == <<phase, steps, frames, chunk, revealed, heights, top, writes, extra, grown>>
wheelVars == <<wstep, panel, inside>>
vars == <<chunkVars, wheelVars>>

TypeOK ==
  /\ phase \in {"idle", "streamed", "acted", "closed"}
  /\ steps \in 0..K
  /\ frames \in 0..2
  /\ chunk \in {"none", "queued", "notified", "committed"}
  /\ revealed \in 0..Heights
  /\ heights \in Foots /\ top \in Foots
  /\ writes \in [Foots -> Nat]
  /\ extra \in 0..Extra
  /\ grown \in BOOLEAN
  /\ wstep \in {"click", "wait", "measured", "wheeled"}
  /\ panel \in {"closed", "starting", "open"}
  /\ inside \in BOOLEAN

Init ==
  /\ phase = "idle"
  /\ steps = 0
  /\ frames = 0
  /\ chunk = "none"
  /\ revealed = 0
  /\ heights = 0
  /\ top = 0
  /\ writes = [f \in Foots |-> 0]
  /\ extra = 0
  /\ grown = FALSE
  /\ wstep = "click"
  /\ panel = "closed"
  /\ inside = FALSE

Reconcile(foot) ==
  IF ~FootGuard \/ top # foot
    THEN /\ writes' = [writes EXCEPT ![foot] = @ + 1]
         /\ top' = foot
    ELSE UNCHANGED <<writes, top>>

(* Chunk side *)

Stream ==
  /\ phase = "idle"
  /\ phase' = "streamed"
  /\ chunk' = "queued"
  /\ UNCHANGED <<steps, frames, revealed, heights, top, writes, extra, grown>>
  /\ UNCHANGED wheelVars

DriverStep ==
  /\ phase \in {"streamed", "acted"}
  /\ IF WaitForGrowth
       THEN \/ phase = "acted"
            \/ NotifyStep /\ chunk = "queued"
            \/ StepAfterCommit /\ chunk = "committed"
       ELSE steps < K
  /\ steps' = IF steps < K THEN steps + 1 ELSE steps
  /\ chunk' = IF chunk = "queued" THEN "notified" ELSE chunk
  /\ revealed' = IF chunk = "committed" /\ revealed < Heights THEN revealed + 1 ELSE revealed
  /\ frames' = IF phase = "acted" THEN frames + 1 ELSE frames
  /\ phase' = IF phase = "acted" /\ frames + 1 = 2 THEN "closed" ELSE phase
  /\ UNCHANGED <<heights, top, writes, extra, grown>>
  /\ UNCHANGED wheelVars

BudgetSpent ==
  /\ ~WaitForGrowth
  /\ phase \in {"streamed", "acted"}
  /\ steps = K
  /\ phase' = "closed"
  /\ UNCHANGED <<steps, frames, chunk, revealed, heights, top, writes, extra, grown>>
  /\ UNCHANGED wheelVars

Commit ==
  /\ chunk = "notified"
  /\ chunk' = "committed"
  /\ Reconcile(heights)
  /\ UNCHANGED <<phase, steps, frames, revealed, heights, extra, grown>>
  /\ UNCHANGED wheelVars

CommitHeight ==
  /\ chunk = "committed"
  /\ heights < revealed
  /\ heights' = heights + 1
  /\ Reconcile(heights + 1)
  /\ UNCHANGED <<phase, steps, frames, chunk, revealed, extra, grown>>
  /\ UNCHANGED wheelVars

ExtraReconcile ==
  /\ chunk # "none"
  /\ extra < Extra
  /\ extra' = extra + 1
  /\ Reconcile(heights)
  /\ UNCHANGED <<phase, steps, frames, chunk, revealed, heights, grown>>
  /\ UNCHANGED wheelVars

Observe ==
  /\ phase = "streamed"
  /\ heights > 0
  /\ grown' = TRUE
  /\ phase' = "acted"
  /\ UNCHANGED <<steps, frames, chunk, revealed, heights, top, writes, extra>>
  /\ UNCHANGED wheelVars

(* Wheel side *)

HitTarget == IF panel = "open" THEN "box" ELSE "outside"

PanelStarts ==
  /\ wstep = "click"
  /\ wstep' = "wait"
  /\ panel' = "starting"
  /\ UNCHANGED inside
  /\ UNCHANGED chunkVars

PanelOpens ==
  /\ panel = "starting"
  /\ panel' = "open"
  /\ UNCHANGED <<wstep, inside>>
  /\ UNCHANGED chunkVars

Measure ==
  /\ wstep = "wait"
  /\ WaitForHitTarget => HitTarget = "box"
  /\ wstep' = "measured"
  /\ UNCHANGED <<panel, inside>>
  /\ UNCHANGED chunkVars

Wheel ==
  /\ wstep = "measured"
  /\ wstep' = "wheeled"
  /\ inside' = (HitTarget = "box")
  /\ UNCHANGED panel
  /\ UNCHANGED chunkVars

Next ==
  \/ Stream \/ DriverStep \/ BudgetSpent \/ Commit \/ CommitHeight \/ ExtraReconcile \/ Observe
  \/ PanelStarts \/ PanelOpens \/ Measure \/ Wheel

Spec == Init /\ [][Next]_vars

FairSpec == Spec
  /\ WF_vars(Stream) /\ WF_vars(DriverStep) /\ WF_vars(Commit) /\ WF_vars(CommitHeight)
  /\ WF_vars(Observe)
  /\ WF_vars(PanelStarts) /\ WF_vars(PanelOpens) /\ WF_vars(Measure) /\ WF_vars(Wheel)

ChunkCounted == phase = "closed" => grown
OneWritePerFoot == \A f \in Foots : writes[f] <= 1
WheelInBox == wstep = "wheeled" => inside

WindowCloses == <>(phase = "closed")
WheelHappens == <>(wstep = "wheeled")

====
