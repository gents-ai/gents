---- MODULE TranscriptPassiveWheelCapture ----
\* lean: none
\* crates: none
\* paths: apps/gents-desktop/src/ui/lib/transcriptList.ts, apps/gents-desktop/tests/playwright/transcript-older-pages.spec.ts
(***************************************************************************)
(* A reader wheels up from the foot of a transcript whose wheel listener   *)
(* is passive (transcriptList.ts).                                         *)
(*                                                                         *)
(* Actors: the compositor/scrolling thread, which may move `scrollTop`     *)
(* for a passive wheel before the main thread runs `onWheel` (e.g. WebKit  *)
(* threaded scrolling while a slow frame holds the main thread); the main  *)
(* thread's `onWheel` (`release` -> `capture`), `onScroll`, and layout     *)
(* measurement + `reconcile`.                                              *)
(*                                                                         *)
(* `SyncSeenOnRelease` is the safeguard: when `release` accounts for any   *)
(* unseen `scrollTop # seenTop` (and `capture` records `seenTop =          *)
(* scrollTop`), a subsequent `onScroll` sees zero residual movement and    *)
(* never subtracts the already-captured scroll from `mode.offset` a second *)
(* time. Without it, `capture` reads the row's screen offset at the        *)
(* post-scroll `scrollTop` while leaving `seenTop` at the pre-scroll       *)
(* position, so `onScroll` adds the wheel distance a second time and       *)
(* `reconcile` jumps the view by an extra wheel notch.                     *)
(***************************************************************************)
EXTENDS Integers

CONSTANTS Notches, InitScroll, InitRowTop, MeasureShrink, SyncSeenOnRelease

VARIABLES
  wheelsLeft,    \* notches the reader has yet to start
  compositorDue, \* wheel scroll steps queued on the compositor
  wheelEvents,   \* `wheel` events queued for the main thread
  scrollTop,     \* live DOM scrollTop
  seenTop,       \* owner's last accounted scrollTop
  rowTop,        \* anchor row's content top (shifts when rows above measure)
  measured,      \* whether a row above has measured once
  mode,          \* "follow" or "read"
  offset,        \* held screen offset (rowTop - scrollTop) when mode = "read"
  travelled      \* cumulative upward wheel travel performed by the compositor

vars ==
  <<wheelsLeft, compositorDue, wheelEvents, scrollTop, seenTop,
    rowTop, measured, mode, offset, travelled>>

Init ==
  /\ wheelsLeft = Notches
  /\ compositorDue = 0
  /\ wheelEvents = 0
  /\ scrollTop = InitScroll
  /\ seenTop = InitScroll
  /\ rowTop = InitRowTop
  /\ measured = FALSE
  /\ mode = "follow"
  /\ offset = 0
  /\ travelled = 0

\* Reader initiates an upward wheel notch: queues both compositor scroll and
\* main-thread passive `wheel` delivery, which may interleave in either order.
StartWheel ==
  /\ wheelsLeft > 0
  /\ wheelsLeft' = wheelsLeft - 1
  /\ compositorDue' = compositorDue + 1
  /\ wheelEvents' = wheelEvents + 1
  /\ UNCHANGED <<scrollTop, seenTop, rowTop, measured, mode, offset, travelled>>

\* Compositor advances scrollTop upward by 1 before or after `onWheel`.
CompositorScroll ==
  /\ compositorDue > 0
  /\ scrollTop > 0
  /\ compositorDue' = compositorDue - 1
  /\ scrollTop' = scrollTop - 1
  /\ travelled' = travelled + 1
  /\ UNCHANGED <<wheelsLeft, wheelEvents, seenTop, rowTop, measured, mode, offset>>

\* Main thread runs `onWheel` -> `release`.
HandleWheel ==
  /\ wheelEvents > 0
  /\ wheelEvents' = wheelEvents - 1
  /\ IF SyncSeenOnRelease
       THEN /\ IF scrollTop # seenTop
                 THEN /\ seenTop' = scrollTop
                      /\ IF mode = "read"
                           THEN /\ mode' = "read"
                                /\ offset' = offset - (scrollTop - seenTop)
                           ELSE /\ mode' = "read"
                                /\ offset' = rowTop - scrollTop
                 ELSE /\ seenTop' = seenTop
                      /\ IF mode = "read"
                           THEN /\ mode' = "read"
                                /\ offset' = offset
                           ELSE /\ mode' = "read"
                                /\ offset' = rowTop - scrollTop
       ELSE /\ seenTop' = seenTop
            /\ IF mode = "read"
                 THEN /\ mode' = "read"
                      /\ offset' = offset
                 ELSE /\ mode' = "read"
                      /\ offset' = rowTop - scrollTop
  /\ UNCHANGED <<wheelsLeft, compositorDue, scrollTop, rowTop, measured, travelled>>

\* Main thread runs `onScroll` when scrollTop differs from seenTop.
HandleScroll ==
  /\ scrollTop # seenTop
  /\ seenTop' = scrollTop
  /\ IF mode = "read"
       THEN /\ mode' = "read"
            /\ offset' = offset - (scrollTop - seenTop)
       ELSE /\ mode' = "read"
            /\ offset' = rowTop - scrollTop
  /\ UNCHANGED <<wheelsLeft, compositorDue, wheelEvents, scrollTop, rowTop, measured, travelled>>

\* A row above the reader measures, shrinking rows above by MeasureShrink,
\* and once the view rests `reconcile` writes `scrollTop = rowTop - offset`.
MeasureAndReconcile ==
  /\ ~measured
  /\ mode = "read"
  /\ scrollTop = seenTop
  /\ compositorDue = 0
  /\ wheelEvents = 0
  /\ measured' = TRUE
  /\ rowTop' = rowTop - MeasureShrink
  /\ scrollTop' = (rowTop - MeasureShrink) - offset
  /\ seenTop' = (rowTop - MeasureShrink) - offset
  /\ UNCHANGED <<wheelsLeft, compositorDue, wheelEvents, mode, offset, travelled>>

Next ==
  \/ StartWheel
  \/ CompositorScroll
  \/ HandleWheel
  \/ HandleScroll
  \/ MeasureAndReconcile

Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ wheelsLeft \in 0..Notches
  /\ compositorDue \in 0..Notches
  /\ wheelEvents \in 0..Notches
  /\ travelled \in 0..Notches
  /\ mode \in {"follow", "read"}
  /\ measured \in BOOLEAN

\* Whenever the owner has accounted for all scroll motion (`scrollTop = seenTop`),
\* the held screen offset equals the initial screen offset plus exact wheel travel,
\* and the anchor row's actual screen position matches it.
AnchorMatchesTravel ==
  (mode = "read" /\ scrollTop = seenTop) =>
    /\ offset = (InitRowTop - InitScroll) + travelled
    /\ (rowTop - scrollTop) = (InitRowTop - InitScroll) + travelled
====
