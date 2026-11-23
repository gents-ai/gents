---- MODULE RecordedNodeStart ----
\* lean: none
\* crates: gents-service, gents-cli, gents-host
\* models: InstallRepair.startAction
(***************************************************************************)
(* `gents -p` against a home whose node was started by hand and restarted  *)
(* on the same port (`init_pins_the_tool_root_under_an_explicit_workspace_ *)
(* root_policy`).                                                           *)
(*                                                                          *)
(* Actors and their atomic steps, from the code:                           *)
(*  - Server (`gents runtime server`, `serve.rs`): binds its port and      *)
(*    answers `/status` with lifecycle `starting` and its node; later      *)
(*    writes `runtime.json`, then marks itself ready, then prints its      *)
(*    `serving` line. The previous server of the same home was killed and  *)
(*    left a `runtime.json` naming the same GraphQL URL.                    *)
(*  - Test: waits for a signal, then runs `gents -p`.                       *)
(*  - CLI (`repair::start`): reads `runtime.json` and asks its `/status`.  *)
(*    The Lean decision `InstallRepair.startAction` maps what it sees to   *)
(*    using the node, waiting for it, or reaching for the service manager. *)
(*                                                                          *)
(* `Wait`: "serving" waits for the server's own `serving` line; "file"     *)
(* (the test before the fix) waits for a `runtime.json` naming the URL and *)
(* for a ready readiness row, both left by the killed server. `Starting`:  *)
(* "await" (the decision) or "service" (the CLI before the fix, which took *)
(* a starting node for an absent one).                                     *)
(*                                                                          *)
(* `NeverTouchesService`: the CLI never reaches the service manager while  *)
(* this home's node answers. `PromptAnswered`: the prompt reaches the node.*)
(*                                                                          *)
(* GREEN `MCRecordedNodeStart`. RED: `StaleFile` (the CI failure: the test *)
(* runs the CLI while the node is starting, and the CLI reaches the        *)
(* service) and `StartingIsAbsent` (a CLI run while the node starts        *)
(* reaches the service even when the test waited right).                   *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Wait,      \* "serving" | "file"
  Starting,  \* "await" | "service"
  EarlyCli   \* the CLI may also run before the test's wait (a user typing)

VARIABLES
  server,    \* "down" | "starting" | "recorded" | "ready" | "printed"
  recorded,  \* runtime.json names the URL (the killed server's, or this one's)
  test,      \* "waiting" | "ran"
  cli,       \* "idle" | "waiting" | "used" | "service"
  early      \* an early CLI run has been made

vars == <<server, recorded, test, cli, early>>

Init ==
  /\ server = "down"
  /\ recorded = TRUE
  /\ test = "waiting"
  /\ cli = "idle"
  /\ early = FALSE

Bind == server = "down" /\ server' = "starting" /\ UNCHANGED <<recorded, test, cli, early>>
Record == server = "starting" /\ server' = "recorded" /\ UNCHANGED <<recorded, test, cli, early>>
MarkReady == server = "recorded" /\ server' = "ready" /\ UNCHANGED <<recorded, test, cli, early>>
Print == server = "ready" /\ server' = "printed" /\ UNCHANGED <<recorded, test, cli, early>>

Answers == server # "down"
IsReady == server \in {"ready", "printed"}

Waited ==
  IF Wait = "serving" THEN server = "printed" ELSE recorded /\ Answers

\* `startAction`: a node that answers ready is used, one that answers
\* starting is awaited (or, before the fix, taken for absent), and with no
\* answer the service manager starts one.
Decide ==
  IF ~Answers THEN "service"
  ELSE IF IsReady THEN "used"
  ELSE IF Starting = "await" THEN "waiting" ELSE "service"

RunCli ==
  /\ test = "waiting" /\ Waited /\ cli = "idle"
  /\ test' = "ran"
  /\ cli' = Decide
  /\ UNCHANGED <<server, recorded, early>>

RunEarly ==
  /\ EarlyCli /\ ~early /\ cli = "idle" /\ Answers
  /\ early' = TRUE
  /\ cli' = Decide
  /\ UNCHANGED <<server, recorded, test>>

Awaited ==
  /\ cli = "waiting" /\ IsReady
  /\ cli' = "used"
  /\ UNCHANGED <<server, recorded, test, early>>

Next == Bind \/ Record \/ MarkReady \/ Print \/ RunCli \/ RunEarly \/ Awaited

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

TypeOK ==
  /\ server \in {"down", "starting", "recorded", "ready", "printed"}
  /\ recorded \in BOOLEAN
  /\ test \in {"waiting", "ran"}
  /\ cli \in {"idle", "waiting", "used", "service"}
  /\ early \in BOOLEAN

\* Every CLI run here happens while the node answers.
NeverTouchesService == cli # "service"

PromptAnswered == <>(cli = "used")
====
