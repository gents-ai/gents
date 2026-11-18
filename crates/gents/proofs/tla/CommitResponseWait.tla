---- MODULE CommitResponseWait ----
\* lean: none
\* crates: gents-store, gents-pack
(***************************************************************************)
(* A client transaction's commit over the runtime's HTTP endpoint         *)
(* (`graphql::txn_commit`, reached from `transact_write_with_finality`,     *)
(* e.g. `pack.document_graph.install`), on a loaded host.                   *)
(*                                                                          *)
(* The client posts `/tx/{id}` and waits. The server works through the      *)
(* commit (`Work`, as many steps as the host's load makes it), then either *)
(* commits or aborts the transaction and answers. Under load nothing bounds *)
(* how long that takes relative to a client clock. The server may also die *)
(* (`Crashes`), which resets the connection.                                *)
(*                                                                          *)
(* `ClientWindow` is a total request timeout on the client (the shared     *)
(* reqwest client's 30 s `HTTP_TIMEOUT` before this change): the client    *)
(* may give up at any step before the answer arrives, and then classifies  *)
(* the outcome as unknown (`TxnCommitError::CleanupRequired`, "transaction *)
(* outcome is unconfirmed"), which the pack installer reports as "pack     *)
(* document outcome was not confirmed". An unknown outcome is correct only *)
(* when the server is gone: Lean `clientSignedWriteMayRetry` then forbids  *)
(* a rebuild, so a live server's commit must be answered, not abandoned.   *)
(*                                                                          *)
(* `ServerBounded` is the premise the waiting client rests on: the server  *)
(* answers every commit it is working on (DefraDB's commit completes or    *)
(* fails, and the runtime's own storage steps are bounded). `Wedge` lets a *)
(* server without it stop answering while alive.                            *)
(*                                                                          *)
(* `Agreement`: a confirmed outcome is the server's outcome.                *)
(* `ConfirmedWhileServerLives`: the client reports an unknown outcome only *)
(* once the server is dead. `ClientSettles`: the client reaches a verdict. *)
(* GREEN: no client window, a bounded server, crashes allowed. RED:        *)
(* `ClientWindow` (the failure on loaded CI runners) violates              *)
(* `ConfirmedWhileServerLives`; `Wedged` (the server premise dropped)      *)
(* violates `ClientSettles`.                                                *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  MaxWork,        \* bound on the model's work steps (any load fits under it)
  ClientWindow,   \* the client may time out before the answer
  Crashes,        \* the server may die mid-commit
  ServerBounded   \* the server answers every commit it works on

VARIABLES
  s,     \* server: working | committed | aborted | wedged | dead
  work,  \* work steps done
  sent,  \* the server's answer is on the wire
  c      \* client: waiting | committed | aborted | unknown

vars == <<s, work, sent, c>>

TypeOK ==
  /\ s \in {"working", "committed", "aborted", "wedged", "dead"}
  /\ work \in 0..MaxWork
  /\ sent \in BOOLEAN
  /\ c \in {"waiting", "committed", "aborted", "unknown"}

Init == s = "working" /\ work = 0 /\ sent = FALSE /\ c = "waiting"

Work ==
  /\ s = "working" /\ work < MaxWork
  /\ work' = work + 1
  /\ UNCHANGED <<s, sent, c>>

Decide ==
  /\ s = "working"
  /\ \E outcome \in {"committed", "aborted"} : s' = outcome
  /\ sent' = TRUE
  /\ UNCHANGED <<work, c>>

Wedge ==
  /\ ~ServerBounded /\ s = "working"
  /\ s' = "wedged"
  /\ UNCHANGED <<work, sent, c>>

Crash ==
  /\ Crashes /\ s \in {"working", "wedged"}
  /\ s' = "dead"
  /\ UNCHANGED <<work, sent, c>>

Receive ==
  /\ c = "waiting" /\ sent
  /\ c' = s
  /\ UNCHANGED <<s, work, sent>>

\* The connection resets once the server is gone without answering.
Reset ==
  /\ c = "waiting" /\ s = "dead"
  /\ c' = "unknown"
  /\ UNCHANGED <<s, work, sent>>

TimeOut ==
  /\ ClientWindow /\ c = "waiting" /\ ~sent
  /\ c' = "unknown"
  /\ UNCHANGED <<s, work, sent>>

Next == Work \/ Decide \/ Wedge \/ Crash \/ Receive \/ Reset \/ TimeOut

Spec ==
  /\ Init /\ [][Next]_vars
  /\ WF_vars(Decide) /\ WF_vars(Receive) /\ WF_vars(Reset)

Agreement == c \in {"committed", "aborted"} => c = s

ConfirmedWhileServerLives == c = "unknown" => s = "dead"

ClientSettles == <>(c # "waiting")

====
