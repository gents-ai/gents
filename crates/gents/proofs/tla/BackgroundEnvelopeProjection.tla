---- MODULE BackgroundEnvelopeProjection ----
\* lean: TranscriptInputOrigin.classification
\* crates: gents-session, gents-desktop-bridge
(***************************************************************************)
(* A test observing how a session transcript attributes its inputs while    *)
(* background completions land beside a person's look-alike messages.       *)
(*                                                                          *)
(* Actors and their atomic steps:                                           *)
(*  - the session worker claims the head of the session queue in arrival    *)
(*    order and publishes its input message in the same transaction         *)
(*    (`Claim`); while a person's turn runs it may fold a queued person     *)
(*    request into that turn, publishing the folded input (`Fold`); it      *)
(*    finishes the turn (`Finish`). Only a person's turn heads a fold       *)
(*    (`heads_user_turn`).                                                  *)
(*  - the background job driver, once the accepted turn has spawned its     *)
(*    two jobs, publishes each completion notification (`Deliver`) and then *)
(*    enqueues the coalesced wake request `W` (`Wake`).                     *)
(*  - the test submits the person's byte-identical wake text `PW` and the   *)
(*    person's quoted envelope `PQ`, one transaction each (`Submit`).       *)
(*  - the observer, after submitting, reads the session after each update   *)
(*    and stops once its condition holds; the projection then renders each  *)
(*    published input by `TranscriptInputOrigin.classification` over the    *)
(*    facts of the request that authored it, and a queued request as        *)
(*    pending.                                                              *)
(*                                                                          *)
(* Property (`TestPasses`): when the observer stops, `PW` and `PQ` render   *)
(* as person messages, `W` as the background wake, and both notifications   *)
(* as background results.                                                   *)
(*                                                                          *)
(* The jobs can complete, and `W` arrive, before the test submits `PQ`;     *)
(* queue order then claims `W` before `PQ`. An observer that stops on the   *)
(* notifications and `W`'s input alone (`WaitForInputs = FALSE`, the test   *)
(* before this spec) can stop while `PQ` is still queued, and the correct   *)
(* product fails it. Waiting also for each submitted request's input        *)
(* message passes under every schedule. Classifying from the text instead   *)
(* of the facts (`ClassifyByText`) fails even the waiting observer: a       *)
(* person's quoted envelope reads as a result and their wake text as a      *)
(* wake. The facts exclude text by the Lean owner's docstring.              *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets, LeanDecision_TranscriptInputOrigin

CONSTANTS
  WaitForInputs,   \* the observer also waits for every submitted input message
  ClassifyByText   \* the projection classifies an input by its text

Requests == {"R0", "PW", "PQ", "W"}
Person == {"R0", "PW", "PQ"}
Submitted == <<"PW", "PQ">>

VARIABLES
  queue,       \* requests arrived and not yet claimed or folded, in arrival order
  published,   \* requests whose input message is in the transcript
  running,     \* the request whose turn runs, or "none"
  spawned,     \* the accepted turn has spawned its background jobs
  delivered,   \* completion notifications published, 0..2
  waked,       \* the wake request has arrived
  submitted,   \* how many of `Submitted` the test has enqueued
  stopped,     \* the observer has stopped and rendered
  rendered     \* input -> "none" | "pending" | a Lean `Kind`

vars == <<queue, published, running, spawned, delivered, waked, submitted,
          stopped, rendered>>

Kinds == {"none", "pending", "person", "background_wake", "background_result"}

TypeOK ==
  /\ queue \in Seq(Requests)
  /\ published \subseteq Requests
  /\ running \in Requests \cup {"none"}
  /\ spawned \in BOOLEAN
  /\ delivered \in 0..2
  /\ waked \in BOOLEAN
  /\ submitted \in 0..Len(Submitted)
  /\ stopped \in BOOLEAN
  /\ rendered \in [Requests \cup {"N"} -> Kinds]

InQueue(r) == \E i \in 1..Len(queue) : queue[i] = r

Remove(s, r) == SelectSeq(s, LAMBDA x : x # r)

Init ==
  /\ queue = <<"R0">>
  /\ published = {}
  /\ running = "none"
  /\ spawned = FALSE
  /\ delivered = 0
  /\ waked = FALSE
  /\ submitted = 0
  /\ stopped = FALSE
  /\ rendered = [r \in Requests \cup {"N"} |-> "none"]

Claim ==
  /\ ~stopped /\ running = "none" /\ queue # <<>>
  /\ running' = Head(queue)
  /\ published' = published \cup {Head(queue)}
  /\ queue' = Tail(queue)
  /\ spawned' = (spawned \/ Head(queue) = "R0")
  /\ UNCHANGED <<delivered, waked, submitted, stopped, rendered>>

Fold ==
  /\ ~stopped /\ running \in Person
  /\ \E p \in Person :
       /\ InQueue(p)
       /\ published' = published \cup {p}
       /\ queue' = Remove(queue, p)
  /\ UNCHANGED <<running, spawned, delivered, waked, submitted, stopped, rendered>>

Finish ==
  /\ ~stopped /\ running # "none"
  /\ running' = "none"
  /\ UNCHANGED <<queue, published, spawned, delivered, waked, submitted, stopped, rendered>>

Deliver ==
  /\ ~stopped /\ spawned /\ delivered < 2
  /\ delivered' = delivered + 1
  /\ UNCHANGED <<queue, published, running, spawned, waked, submitted, stopped, rendered>>

Wake ==
  /\ ~stopped /\ delivered = 2 /\ ~waked
  /\ waked' = TRUE
  /\ queue' = Append(queue, "W")
  /\ UNCHANGED <<published, running, spawned, delivered, submitted, stopped, rendered>>

Submit ==
  /\ ~stopped /\ submitted < Len(Submitted)
  /\ submitted' = submitted + 1
  /\ queue' = Append(queue, Submitted[submitted + 1])
  /\ UNCHANGED <<published, running, spawned, delivered, waked, stopped, rendered>>

(* The facts of each input, as the projection derives them: a notification *)
(* from its message key, the wake request from its queue source, a          *)
(* person's request from neither. `ClassifyByText` derives them from the    *)
(* text instead: `PW` is the wake prompt, `PQ` quotes an envelope.          *)
NotificationFact(r) == r = "N" \/ (ClassifyByText /\ r = "PQ")
WakeFact(r) == r = "W" \/ (ClassifyByText /\ r = "PW")

Classify(r) ==
  classification(TRUE, FALSE, NotificationFact(r), FALSE, WakeFact(r), FALSE, FALSE)

Render(r) ==
  IF r \in published THEN Classify(r)
  ELSE IF InQueue(r) THEN "pending" ELSE "none"

StopCondition ==
  /\ submitted = Len(Submitted)
  /\ delivered = 2
  /\ "W" \in published
  /\ WaitForInputs => {Submitted[i] : i \in 1..Len(Submitted)} \subseteq published

Observe ==
  /\ ~stopped /\ StopCondition
  /\ stopped' = TRUE
  /\ rendered' = [r \in Requests \cup {"N"} |->
                    IF r = "N" THEN Classify("N") ELSE Render(r)]
  /\ UNCHANGED <<queue, published, running, spawned, delivered, waked, submitted>>

Next == Claim \/ Fold \/ Finish \/ Deliver \/ Wake \/ Submit \/ Observe

Spec == Init /\ [][Next]_vars
          /\ WF_vars(Claim) /\ WF_vars(Finish) /\ WF_vars(Deliver)
          /\ WF_vars(Wake) /\ WF_vars(Submit) /\ WF_vars(Observe)

TestPasses ==
  stopped =>
    /\ rendered["PW"] = "person"
    /\ rendered["PQ"] = "person"
    /\ rendered["W"] = "background_wake"
    /\ rendered["N"] = "background_result"

ObserverSettles == <>stopped

====
