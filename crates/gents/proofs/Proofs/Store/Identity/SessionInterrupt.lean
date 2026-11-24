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

end Identity.SessionInterrupt
