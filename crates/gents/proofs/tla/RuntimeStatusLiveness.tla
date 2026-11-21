---- MODULE RuntimeStatusLiveness ----
\* lean: none
\* crates: gents-cli, gents-host, gents-session
(***************************************************************************)
(* `gents status` reporting a serving runtime's liveness on a loaded host  *)
(* (`status_liveness_surfaces_expired_processing_request_and_running_tool`)*)
(*                                                                          *)
(* The CLI asks the runtime's `/status` and waits. The runtime's handler    *)
(* reads the store under its probe budget (`STATUS_PROBE_BUDGET`): the read *)
(* finishes after `storeLeft` ticks of host time, or the budget fires at    *)
(* tick `Budget` first. It then does the rest of its bounded work (the p2p, *)
(* self-view and contract steps, `restLeft` ticks) and answers:             *)
(*  - "liveness": the store read finished; the liveness carries the        *)
(*    process's native executors, which live only in its memory.           *)
(*  - "executors": the budget fired, so there is no liveness, but the      *)
(*    answer reports the in-memory executors beside it.                    *)
(*  - "bare": the budget fired and the executors were reported only inside *)
(*    the liveness (`ExecutorsBudgeted`, the runtime before the fix).      *)
(* The CLI composes: the runtime's liveness, else its own store read with  *)
(* the runtime's executors, else its own store read with the executors     *)
(* unavailable (it never received the runtime's answer, or the answer had  *)
(* none).                                                                   *)
(*                                                                          *)
(* `storeLeft` and `restLeft` range over every load up to `MaxStore` and   *)
(* `MaxRest`; nothing else bounds how long a loaded host takes             *)
(* (`WallClockBound`). Time passes only while some step is in progress.    *)
(*                                                                          *)
(* The client's detector (`Detector`):                                     *)
(*   "none"   waits for the answer (the runtime bounds its own work).      *)
(*   "clock"  gives up at tick `Window` (the CLI's 2 s total timeout       *)
(*            before the fix) and reports the executors unavailable.       *)
(*                                                                          *)
(* `ExecutorsVisible`: a CLI that settled reports the runtime's executors. *)
(* `ClientSettles`: the CLI reaches a verdict.                             *)
(*                                                                          *)
(* GREEN `MCRuntimeStatusLiveness`: no detector, executors outside the     *)
(* budget. RED, one safeguard removed each: `ClientWindow` (the CI         *)
(* failure: a window even longer than the runtime's budget loses a live    *)
(* runtime's answer to a slow host) and `BudgetedExecutors` (a store read  *)
(* slower than the budget withholds the in-memory executors). Both violate *)
(* `ExecutorsVisible`.                                                     *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  MaxStore,          \* host ticks the store read may take under load
  MaxRest,           \* host ticks the rest of the handler may take
  Budget,            \* the handler's store-read budget, in ticks
  Detector,          \* "none" | "clock"
  Window,            \* tick at which a "clock" detector gives up
  ExecutorsBudgeted  \* executors are reported only inside the liveness

Cap == MaxStore + MaxRest + Window + 1

VARIABLES
  t,          \* ticks since the CLI sent the request
  store,      \* handler's store read: reading | ok | missed
  storeLeft,  \* ticks the store read still needs
  restLeft,   \* ticks the rest of the handler still needs
  answer,     \* none | liveness | executors | bare
  c           \* CLI: waiting | visible | hidden

vars == <<t, store, storeLeft, restLeft, answer, c>>

TypeOK ==
  /\ t \in 0..Cap
  /\ store \in {"reading", "ok", "missed"}
  /\ storeLeft \in 0..MaxStore
  /\ restLeft \in 0..MaxRest
  /\ answer \in {"none", "liveness", "executors", "bare"}
  /\ c \in {"waiting", "visible", "hidden"}

Init ==
  /\ t = 0
  /\ store = "reading"
  /\ storeLeft \in 1..MaxStore
  /\ restLeft \in 0..MaxRest
  /\ answer = "none"
  /\ c = "waiting"

StoreDone ==
  /\ store = "reading" /\ storeLeft = 0
  /\ store' = "ok"
  /\ UNCHANGED <<t, storeLeft, restLeft, answer, c>>

BudgetFires ==
  /\ store = "reading" /\ storeLeft > 0 /\ t >= Budget
  /\ store' = "missed"
  /\ UNCHANGED <<t, storeLeft, restLeft, answer, c>>

Answer ==
  /\ store # "reading" /\ restLeft = 0 /\ answer = "none"
  /\ answer' = IF store = "ok" THEN "liveness"
               ELSE IF ExecutorsBudgeted THEN "bare" ELSE "executors"
  /\ UNCHANGED <<t, store, storeLeft, restLeft, c>>

Receive ==
  /\ c = "waiting" /\ answer # "none"
  /\ c' = IF answer \in {"liveness", "executors"} THEN "visible" ELSE "hidden"
  /\ UNCHANGED <<t, store, storeLeft, restLeft, answer>>

GiveUp ==
  /\ Detector = "clock" /\ c = "waiting" /\ answer = "none" /\ t >= Window
  /\ c' = "hidden"
  /\ UNCHANGED <<t, store, storeLeft, restLeft, answer>>

Instant == StoreDone \/ BudgetFires \/ Answer \/ Receive \/ GiveUp

\* Host time passes only while the handler works and nothing is due now.
Tick ==
  /\ c = "waiting" /\ t < Cap
  /\ ~ENABLED Instant
  /\ t' = t + 1
  /\ IF store = "reading"
       THEN /\ storeLeft' = storeLeft - 1 /\ UNCHANGED restLeft
       ELSE /\ restLeft' = restLeft - 1 /\ UNCHANGED storeLeft
  /\ UNCHANGED <<store, answer, c>>

Next == Instant \/ Tick

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

ExecutorsVisible == c # "hidden"

ClientSettles == <>(c # "waiting")
====
