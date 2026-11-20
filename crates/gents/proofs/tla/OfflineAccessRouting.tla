---- MODULE OfflineAccessRouting ----
\* lean: none
\* crates: gents-cli, gents-host, gents-store
\* models: StoreHandoff.decide
(***************************************************************************)
(* A CLI command run against a home another process holds, such as         *)
(* `gents sessions fork` in the gents-cli `cli_session` suite (#3816).    *)
(*                                                                          *)
(* Actors and their atomic steps, as the code has them:                    *)
(*  - Holder: a `gents runtime server` (gents-host `serve.rs`) takes the   *)
(*    store lock (`acquire_store`), binds its HTTP port, and only once it  *)
(*    is ready writes `runtime.json` (`write_runtime_state`). A test       *)
(*    process holding the lock itself (`lock_home_store`) claims and never *)
(*    listens or publishes (`Serves = FALSE`).                              *)
(*  - Command (gents-cli `resolve_config_access_inner`): reads             *)
(*    `runtime.json` and, when it names an endpoint that answers, routes   *)
(*    to that runtime, which answers the command itself. Otherwise it      *)
(*    tries the store lock; a holder that has not declared its release is  *)
(*    refused at once naming the holder and the `--graphql` escape (Lean   *)
(*    `StoreHandoff.decide`: `held false` is `refuse`, `free` is           *)
(*    `acquire`). The read and the try are separate steps: the holder may  *)
(*    move between them.                                                    *)
(*  - Test: waits for its readiness event (`Ready`), then runs the         *)
(*    command and asserts the outcome it `Expects`.                        *)
(*                                                                          *)
(* `AssertionHolds`: the outcome the command reports is the one the test   *)
(* expects. It is a property of the test's wait, not of the product: the   *)
(* product is the same in every configuration, and both outcomes are its   *)
(* decided answers to different moments. Waiting only for the port         *)
(* (`Ready = "port"`, the test before this spec) runs the command in the   *)
(* start-up window between the bind and the publication, so the refusal   *)
(* the test expected and the server's own answer are both reachable.      *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Serves,   \* the holder is a server that listens and publishes
  Ready,    \* "claim" | "port" | "published": what the test waits for
  Expects   \* "answered" | "refused": the outcome the test asserts

VARIABLES
  claimed,    \* the holder has the store lock
  listening,  \* the holder's HTTP port accepts connections
  published,  \* `runtime.json` names the holder's endpoint
  command,    \* "idle" | "read" | "try" | "done"
  outcome     \* "none" | "answered" | "refused" | "opened"

vars == <<claimed, listening, published, command, outcome>>

TypeOK ==
  /\ claimed \in BOOLEAN /\ listening \in BOOLEAN /\ published \in BOOLEAN
  /\ command \in {"idle", "read", "try", "done"}
  /\ outcome \in {"none", "answered", "refused", "opened"}

Init ==
  /\ claimed = FALSE /\ listening = FALSE /\ published = FALSE
  /\ command = "idle" /\ outcome = "none"

Claim ==
  /\ ~claimed
  /\ claimed' = TRUE
  /\ UNCHANGED <<listening, published, command, outcome>>

Listen ==
  /\ Serves /\ claimed /\ ~listening
  /\ listening' = TRUE
  /\ UNCHANGED <<claimed, published, command, outcome>>

Publish ==
  /\ Serves /\ listening /\ ~published
  /\ published' = TRUE
  /\ UNCHANGED <<claimed, listening, command, outcome>>

TestReady ==
  CASE Ready = "claim" -> claimed
    [] Ready = "port" -> listening
    [] OTHER -> published

StartCommand ==
  /\ command = "idle" /\ TestReady
  /\ command' = "read"
  /\ UNCHANGED <<claimed, listening, published, outcome>>

(* `read_runtime_state` and `graphql_endpoint_available`.                  *)
ReadRuntimeState ==
  /\ command = "read"
  /\ IF published /\ listening
       THEN /\ command' = "done" /\ outcome' = "answered"
       ELSE /\ command' = "try" /\ UNCHANGED outcome
  /\ UNCHANGED <<claimed, listening, published>>

(* `acquire_store`: `StoreHandoff.decide` on a holder that never declares  *)
(* its release.                                                             *)
TryStore ==
  /\ command = "try"
  /\ command' = "done"
  /\ outcome' = IF claimed THEN "refused" ELSE "opened"
  /\ UNCHANGED <<claimed, listening, published>>

Next ==
  \/ Claim \/ Listen \/ Publish
  \/ StartCommand \/ ReadRuntimeState \/ TryStore

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

AssertionHolds == command = "done" => outcome = Expects

Answers == <>(command = "done")
====
