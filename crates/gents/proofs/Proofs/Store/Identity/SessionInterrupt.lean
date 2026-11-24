import Proofs.Store.Identity.Household

namespace Identity.SessionInterrupt

open Identity.Household

/-- The interrupt marker's resource in `Household.policy`. -/
def resource : ResourceDefinition :=
  (policy.find? (·.name == "session_interrupts")).getD
    { name := "session_interrupts", relations := [], permissions := [] }

/-- A Node, its Agent and a session of it, with a request of the session and
the session's marker created with the session by the session's creator.
`sessionOwner` created the session and its marker; `requestOwner` sent the
request. `node-admin` administers the Node; `other` is an Agent with no
relation to the session. -/
def fixture (sessionOwner requestOwner : DID) : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"agents", "agent-doc", "user"⟩,
      ⟨"sessions", "session-doc", sessionOwner⟩,
      ⟨"session_interrupts", "marker", sessionOwner⟩,
      ⟨"delivered_requests", "request-doc", requestOwner⟩]
    relations := [⟨"nodes", "node", "administrator", .actor "node-admin"⟩,
      ⟨"agents", "agent-doc", "parent", .object "nodes" "node"⟩,
      ⟨"agents", "agent-doc", "executor", .actor "agent"⟩,
      ⟨"agents", "agent-doc", "administrator", .entitySet "nodes" "node" "administrator"⟩,
      ⟨"sessions", "session-doc", "parent", .object "agents" "agent-doc"⟩,
      ⟨"sessions", "session-doc", "working_agent", .actor "agent"⟩,
      ⟨"session_interrupts", "marker", "parent", .object "sessions" "session-doc"⟩,
      ⟨"delivered_requests", "request-doc", "parent", .object "agents" "agent-doc"⟩,
      ⟨"delivered_requests", "request-doc", "session", .object "sessions" "session-doc"⟩,
      ⟨"delivered_requests", "request-doc", "working_agent", .actor "agent"⟩]
    enrolled := [] }

def actors : List DID := ["user", "agent", "node-admin", "other"]

def mayInterrupt (state : State) (actor : DID) : Bool :=
  allowed 8 state actor "session_interrupts" "marker" "update"

def mayLatchRequest (state : State) (actor : DID) : Bool :=
  allowed 8 state actor "delivered_requests" "request-doc" "update"

/-- No new authority: on a User's session, who may bump the marker is who may
write the request's latch, and an Agent with no relation to the session may
do neither. -/
theorem a_user_session_is_interrupted_by_who_latches_its_request :
    actors.map (mayInterrupt (fixture "user" "user")) =
      actors.map (mayLatchRequest (fixture "user" "user")) ∧
    actors.map (mayInterrupt (fixture "user" "user")) = [true, true, true, false] := by
  native_decide

/-- The same on a session an Agent created and published into. -/
theorem an_agent_session_is_interrupted_by_who_latches_its_request :
    actors.map (mayInterrupt (fixture "agent" "agent")) =
      actors.map (mayLatchRequest (fixture "agent" "agent")) := by
  native_decide

/-- Whoever reads the session reads its marker, so every claim and wake
publication of the session can read the marker by id. -/
theorem the_marker_is_read_by_the_session_readers :
    actors.map (fun actor => allowed 8 (fixture "user" "user") actor
        "session_interrupts" "marker" "read") =
      actors.map (fun actor => allowed 8 (fixture "user" "user") actor
        "sessions" "session-doc" "read") := by
  native_decide

/-- A signed create as its id sees it: what it writes and who signs it. -/
structure SignedCreate where
  signer : DID
  content : String
  deriving DecidableEq, Repr

/-- The id a signed create takes. External premise
(`a_signed_create_takes_an_id_only_its_signer_can_derive`, gents-store): an id
is the CID of the genesis block, which links its signer's signature, so the
same content signed by another key takes another id. -/
def idOf (create : SignedCreate) : SignedCreate := create

/-- No one but a session's creator can name its id before the session
commits, so no one can squat its marker first: the marker's `session_doc_id`
is unique and the marker is created in the session's own transaction, so a
second marker for a session that exists is refused, and a marker another
actor creates first names an id no session of another signer takes.

The policy cannot refuse such a marker at its create. A create passes the
Node's create gate on the authenticated actor alone (`Household.directCreate`):
the policy language has no create permission per resource and no condition on
another document, so it cannot require that the named session exists or that
its creator signs the marker. A marker naming a session that does not exist
is therefore accepted and inert: readers find a session's marker by that
session's id, which only the session's own signer can produce. -/
theorem marker_squatting_needs_the_creators_key (session squat : SignedCreate)
    (h : squat.signer ≠ session.signer) : idOf squat ≠ idOf session := by
  intro h_eq
  exact h (congrArg SignedCreate.signer h_eq)

end Identity.SessionInterrupt
