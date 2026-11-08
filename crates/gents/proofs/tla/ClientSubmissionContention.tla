---- MODULE ClientSubmissionContention ----
\* A User's signed client submission racing its session's active turn.
\*
\* A submission is two DefraDB transactions run by the User through the
\* signed client endpoint. The reservation (`request.client.reserve_session`,
\* `reserve_client_targets`) reads the target `AgentSession` document and the
\* selected `Agent` to bind the physical targets the User then signs. The
\* submission (`request.client.submit`, `submit_client_request`) reads the
\* same documents (requester, selected Agent, `relationships_pending`), then
\* creates the User-signed `AgentRequest` and stages its relationships. While the
\* session's turn runs, its working Agent publishes each message
\* (`tool_call.complete_delivery`, assistant publication) in a transaction
\* that also refreshes the `AgentSession` observation (`patch_session_in_txn`,
\* `last_activity_at`). That refresh changes the document the submission
\* read, so the submission's commit aborts with a DefraDB transaction
\* conflict (traced in the `mailbox_tool_turn` busy-session test). The
\* reservation's read of that document aborts the same way ("a partial read
\* lost to a newer merge" in `live_standard_backgrounding_uses_real_inference`
\* while the runtime wrote the session).
\*
\* Keys: "sess" is the `AgentSession` document; the submission's other reads
\* (the Agent document) have no concurrent writer during a turn. DefraDB is
\* optimistic: a commit aborts when a key it read changed after its snapshot.
\*
\* A client-signed write may be rebuilt only after the native owner confirms
\* the attempt did not commit and the caller authorizes replay of a
\* transaction-only operation (Lean `Identity.Configuration
\* .clientSignedWriteMayRetry`). The rebuild re-reads the session, re-plans
\* and signs a fresh graph; the User's request admission signature covers the
\* request content, not the graph, so it is reused. `Redrive` selects that
\* opt-in mode for the submission and `ReserveRedrive` for the reservation:
\* FALSE is the code before each fix, which surfaced the first confirmed
\* conflict as a rejected submission. The reservation is idempotent on an
\* existing session, so its rebuild binds the same targets. A lost commit
\* response is unconfirmed and never rebuilt (`LostResponse`).
\*
\* Recovery after an unconfirmed submission (`Recovery`): the caller's
\* journal retains the signed bytes, and the reconciler reads the owner's
\* request (`reconcile_client_request`) and replays the identical bytes when
\* it sees none (`resolve_unresolved_submission`). DefraDB derives the
\* document identity from its content, so the identical replay and a late
\* landing of the original commit are one document ("orig"). A lost response
\* may leave the original commit in flight; it later lands or aborts. The
\* bytes may be lost first (a restart of an older uncorrelated send, a reset
\* journal); the caller can then only read the owner's visible request
\* (`reconcile_unretained_submission`), whose absence proves nothing.
\* `UnretainedPolicy` is the pending decision for that absence: "wait" keeps
\* the send unconfirmed (the native `DeliveryUnconfirmed` today), "refuse"
\* reports it not delivered (the `.refused` branch of Lean
\* `ClientSubmissionReceipt.result`), and the person may then resend a new
\* identity ("resend"). Lean `journal_records_never_show_unconfirmed` shows a
\* journal-written record never reaches that view; this models the rows the
\* journal did not write.
\*
\* `Publications` bounds the Agent's session refreshes that land inside one
\* submission's attempt series: the owned loop refreshes the observation once
\* per published message, and the series spans the bounded
\* `transaction_backoff` (100 + 200 + 400 ms). An abort consumes one such
\* refresh, so either phase's series outlasts them only when
\* `Publications < Attempts`.
EXTENDS Naturals, FiniteSets

CONSTANTS
  Publications,  \* Agent session refreshes landing in one attempt series
  Attempts,      \* bounded attempts (TRANSACT_CONFLICT_MAX_RETRIES + 1)
  Redrive,       \* signed-conflict rebuild of the submission
  ReserveRedrive,\* signed-conflict rebuild of the target reservation
  LostResponse,  \* a commit response may be lost (unconfirmed)
  Recovery,      \* model recovery and an in-flight original commit
  BytesLost,     \* the caller may lose the signed bytes first
  ReplayIdentical, \* recovery replays the retained bytes, not a new identity
  UnretainedPolicy \* "wait" | "refuse": an unproven send without bytes

ASSUME /\ Publications \in Nat
       /\ Attempts \in Nat \ {0}
       /\ Redrive \in BOOLEAN
       /\ ReserveRedrive \in BOOLEAN
       /\ LostResponse \in BOOLEAN
       /\ Recovery \in BOOLEAN
       /\ BytesLost \in BOOLEAN
       /\ ReplayIdentical \in BOOLEAN
       /\ UnretainedPolicy \in {"wait", "refuse"}

VARIABLES
  sessVer,   \* committed version of the AgentSession document
  agentLeft, \* Agent session refreshes still to land
  client,    \* "idle" | "reserving" | "reserveUnconfirmed" | "reserved"
             \* | "open" | "admitted" | "unconfirmed" | "lost" | "unretained"
             \* | "notDelivered"
  snap,      \* the session version the open attempt read
  tries,     \* attempts begun in the current phase
  committed, \* committed AgentRequest identities: "orig" and a "resend"
  inflight,  \* the original commit is still undecided
  retained   \* the caller still holds the original signed bytes

vars == <<sessVer, agentLeft, client, snap, tries, committed, inflight, retained>>

\* Committed AgentRequest documents for this authored message.
created == Cardinality(committed)

Init ==
  /\ sessVer = 0
  /\ agentLeft = Publications
  /\ client = "idle"
  /\ snap = 0
  /\ tries = 0
  /\ committed = {}
  /\ inflight = FALSE
  /\ retained = TRUE

\* The working Agent publishes a message and refreshes the observation.
AgentPublish ==
  /\ agentLeft > 0
  /\ sessVer' = sessVer + 1
  /\ agentLeft' = agentLeft - 1
  /\ UNCHANGED <<client, snap, tries, committed, inflight, retained>>

\* The reservation opens its first attempt and reads the session.
ReserveBegin ==
  /\ client = "idle"
  /\ client' = "reserving"
  /\ snap' = sessVer
  /\ tries' = 1
  /\ UNCHANGED <<sessVer, agentLeft, committed, inflight, retained>>

\* Reservation commit: the targets are bound when the read session is
\* unchanged; a confirmed abort is rebuilt by re-reading while attempts remain.
ReserveCommit ==
  /\ client = "reserving"
  /\ IF snap = sessVer
       THEN /\ client' = "reserved"
            /\ tries' = 0
            /\ UNCHANGED snap
       ELSE IF ReserveRedrive /\ tries < Attempts
              THEN /\ snap' = sessVer
                   /\ tries' = tries + 1
                   /\ UNCHANGED client
              ELSE /\ client' = "lost"
                   /\ UNCHANGED <<snap, tries>>
  /\ UNCHANGED <<sessVer, agentLeft, committed, inflight, retained>>

\* The reservation commits but its response is lost: it is reported, and the
\* caller repeats it against the same existing session. No request was sent.
ReserveCommitUnconfirmed ==
  /\ LostResponse
  /\ client = "reserving"
  /\ snap = sessVer
  /\ client' = "reserveUnconfirmed"
  /\ UNCHANGED <<sessVer, agentLeft, snap, tries, committed, inflight, retained>>

\* The submission opens its first attempt and reads the session.
ClientBegin ==
  /\ client = "reserved"
  /\ client' = "open"
  /\ snap' = sessVer
  /\ tries' = 1
  /\ UNCHANGED <<sessVer, agentLeft, committed, inflight, retained>>

\* Commit: success when the read session is unchanged; otherwise a confirmed
\* abort, which the rebuild re-drives by re-reading while attempts remain.
ClientCommit ==
  /\ client = "open"
  /\ IF snap = sessVer
       THEN /\ client' = "admitted"
            /\ committed' = committed \cup {"orig"}
            /\ UNCHANGED <<snap, tries>>
       ELSE IF Redrive /\ tries < Attempts
              THEN /\ snap' = sessVer
                   /\ tries' = tries + 1
                   /\ UNCHANGED <<client, committed>>
              ELSE /\ client' = "lost"
                   /\ UNCHANGED <<snap, tries, committed>>
  /\ UNCHANGED <<sessVer, agentLeft, inflight, retained>>

\* The commit lands but its response is lost: finality is unknown, so the
\* submission reports it and never rebuilds a second graph.
ClientCommitUnconfirmed ==
  /\ LostResponse
  /\ client = "open"
  /\ snap = sessVer
  /\ client' = "unconfirmed"
  /\ committed' = committed \cup {"orig"}
  /\ UNCHANGED <<sessVer, agentLeft, snap, tries, inflight, retained>>

\* The response is lost before the commit is decided: the submission is
\* reported unconfirmed while the original may still land.
ClientCommitInFlight ==
  /\ LostResponse
  /\ Recovery
  /\ client = "open"
  /\ snap = sessVer
  /\ client' = "unconfirmed"
  /\ inflight' = TRUE
  /\ UNCHANGED <<sessVer, agentLeft, snap, tries, committed, retained>>

\* The undecided original commit lands, or aborts.
OriginalLands ==
  /\ inflight
  /\ inflight' = FALSE
  /\ committed' = committed \cup {"orig"}
  /\ UNCHANGED <<sessVer, agentLeft, client, snap, tries, retained>>

OriginalAborts ==
  /\ inflight
  /\ inflight' = FALSE
  /\ UNCHANGED <<sessVer, agentLeft, client, snap, tries, committed, retained>>

\* The caller loses the signed bytes of an unconfirmed send.
LoseBytes ==
  /\ BytesLost
  /\ client = "unconfirmed"
  /\ retained
  /\ retained' = FALSE
  /\ client' = "unretained"
  /\ UNCHANGED <<sessVer, agentLeft, snap, tries, committed, inflight>>

\* With retained bytes: a visible request settles it; absence replays the
\* identical bytes, which is the same document as the original. Without
\* `ReplayIdentical` the absence is answered with a freshly signed request.
ReconcileRetained ==
  /\ Recovery
  /\ client = "unconfirmed"
  /\ retained
  /\ client' = "admitted"
  /\ committed' = IF "orig" \in committed \/ ReplayIdentical
                    THEN committed \cup {"orig"}
                    ELSE committed \cup {"resend"}
  /\ UNCHANGED <<sessVer, agentLeft, snap, tries, inflight, retained>>

\* Without bytes only the owner's visible request is evidence.
CheckUnretained ==
  /\ client = "unretained"
  /\ IF "orig" \in committed
       THEN client' = "admitted"
       ELSE IF UnretainedPolicy = "refuse"
              THEN client' = "notDelivered"
              ELSE UNCHANGED client
  /\ UNCHANGED <<sessVer, agentLeft, snap, tries, committed, inflight, retained>>

\* Told it was not delivered, the person sends it again as a new identity.
Resend ==
  /\ client = "notDelivered"
  /\ client' = "admitted"
  /\ committed' = committed \cup {"resend"}
  /\ UNCHANGED <<sessVer, agentLeft, snap, tries, inflight, retained>>

Next ==
  \/ AgentPublish
  \/ ReserveBegin
  \/ ReserveCommit
  \/ ReserveCommitUnconfirmed
  \/ ClientBegin
  \/ ClientCommit
  \/ ClientCommitUnconfirmed
  \/ ClientCommitInFlight
  \/ OriginalLands
  \/ OriginalAborts
  \/ LoseBytes
  \/ ReconcileRetained
  \/ CheckUnretained
  \/ Resend

Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ WF_vars(ReserveBegin)
  /\ WF_vars(ReserveCommit)
  /\ WF_vars(ClientBegin)
  /\ WF_vars(ClientCommit)
  /\ WF_vars(OriginalLands \/ OriginalAborts)
  /\ WF_vars(ReconcileRetained)
  /\ WF_vars(CheckUnretained)
  /\ WF_vars(Resend)

TypeOK ==
  /\ sessVer \in 0..Publications
  /\ agentLeft \in 0..Publications
  /\ client \in {"idle", "reserving", "reserveUnconfirmed", "reserved", "open", "admitted",
                 "unconfirmed", "lost", "unretained", "notDelivered"}
  /\ tries \in 0..Attempts
  /\ committed \subseteq {"orig", "resend"}
  /\ inflight \in BOOLEAN
  /\ retained \in BOOLEAN

\* A valid submission during a turn is never lost to the turn's own writes.
NoConflictLoss == client # "lost"

\* Neither rebuilding nor recovery creates a second request for one
\* authored message, even after a lost response.
CreatedOnce == created <= 1

\* Every submission settles: admitted, or reported unconfirmed.
EverySubmissionSettles ==
  <>(client \in {"admitted", "unconfirmed", "reserveUnconfirmed"})

\* Recovery: an unconfirmed send is eventually shown delivered, or not
\* delivered with a way forward; it is never left checking forever.
UnconfirmedResolves ==
  [](client \in {"unconfirmed", "unretained"} => <>(client \in {"admitted", "notDelivered"}))

====
