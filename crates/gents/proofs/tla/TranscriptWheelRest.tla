---- MODULE TranscriptWheelRest ----
\* lean: none
\* crates: none
\* models: ClientShell.ViewRest.rests
\* paths: apps/gents-desktop/src/ui/lib/transcriptList.ts, apps/gents-desktop/src/ui/lib/viewRest.ts, apps/gents-desktop/tests/playwright/transcript-older-pages.spec.ts
(***************************************************************************)
(* A reader wheels up a transcript in WebKit while the scroll owner holds  *)
(* their row (transcriptList.ts).                                          *)
(*                                                                         *)
(* Actors: the reader, who sends `Notches` wheels at any moment; WebKit,   *)
(* which moves each wheel's animated scroll one step per drawn frame and   *)
(* draws a frame every `FrameGap` ticks; the owner, whose rest timer and   *)
(* size observers ask it to write the held row's place back whenever the   *)
(* rest decision says the view rests. Premise: a position written while a  *)
(* wheel's animation still has steps to go drops those steps. A frame that *)
(* finds the view where the last one left it counts as still; a wheel     *)
(* that moves the view, or a frame that moved it, restarts both counts.    *)
(*                                                                         *)
(* `StillFramesNeeded` is the safeguard: zero is the rest decision by time *)
(* alone, which writes between two frames of one wheel once frames come   *)
(* further apart than `RestTicks` (a loaded page). `FrameGap` below        *)
(* `RestTicks` is an unloaded page, where time alone happens to hold.      *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS Notches, Steps, RestTicks, FrameGap, StillFramesNeeded

Min(a, b) == IF a < b THEN a ELSE b

\* Lean `ClientShell.ViewRest.rests`, with nothing holding the view.
Rests(since, still) == since >= RestTicks /\ still >= StillFramesNeeded

VARIABLES wheels, left, travelled, since, still, frameIn
vars == <<wheels, left, travelled, since, still, frameIn>>

Init ==
  /\ wheels = Notches
  /\ left = 0
  /\ travelled = 0
  /\ since = RestTicks
  /\ still = StillFramesNeeded
  /\ frameIn = FrameGap

Wheel ==
  /\ wheels > 0
  /\ wheels' = wheels - 1
  /\ left' = left + Steps
  /\ since' = 0
  /\ still' = 0
  /\ UNCHANGED <<travelled, frameIn>>

Tick ==
  /\ frameIn > 0
  /\ frameIn' = frameIn - 1
  /\ since' = Min(since + 1, RestTicks)
  /\ UNCHANGED <<wheels, left, travelled, still>>

Frame ==
  /\ frameIn = 0
  /\ frameIn' = FrameGap
  /\ IF left > 0
       THEN /\ left' = left - 1
            /\ travelled' = travelled + 1
            /\ since' = 0
            /\ still' = 0
       ELSE /\ still' = Min(still + 1, StillFramesNeeded)
            /\ UNCHANGED <<left, travelled, since>>
  /\ UNCHANGED wheels

OwnerWrites ==
  /\ Rests(since, still)
  /\ left > 0
  /\ left' = 0
  /\ UNCHANGED <<wheels, travelled, since, still, frameIn>>

Next == Wheel \/ Tick \/ Frame \/ OwnerWrites

Spec == Init /\ [][Next]_vars

FairSpec == Spec /\ WF_vars(Tick) /\ WF_vars(Frame) /\ WF_vars(Wheel)

TypeOK ==
  /\ wheels \in 0..Notches
  /\ left \in 0..(Notches * Steps)
  /\ travelled \in 0..(Notches * Steps)
  /\ since \in 0..RestTicks
  /\ still \in 0..StillFramesNeeded
  /\ frameIn \in 0..FrameGap

\* Every step of every wheel is either travelled or still to come.
WholeTravel == travelled + left + wheels * Steps = Notches * Steps

\* Once the reader stops, the view comes to rest, so the owner's held
\* correction is written rather than carried forever.
ComesToRest == <>[](wheels = 0 /\ left = 0 /\ Rests(since, still))
====
