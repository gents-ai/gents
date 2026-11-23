import Proofs.Store.Identity.Household

/-!
Who sees the evidence that a provider refused work for usage.

Decision: the evidence is the failed `InferenceCall` documents themselves
(resource `children`), which already carry the recorded verdict, reset and
statement (`InferenceCall.ProviderLimit.Recorded`); no copy, summary or cache
of it is stored, so no new resource exists and nothing is readable that a call
is not. A call is created by the Agent whose request it serves, so that Agent
owns it, and its `parent` is that request's session. Every view reads the
evidence as its viewer: an Agent's views (`provider_limits`, the account
inventory) read as that Agent, and the operator's views read as the User. So
an Agent sees only its own calls, plus those in a session a User explicitly
shared with it, and the User, who administers every Agent of its Node, sees
the union. No view reads evidence as the runtime or with another Agent's
held signer: document ACP grants the runtime's node key none of it, and
its NAC bypass (`Household.bypass_reads_every_document`) is no grant to
pass on to an Agent.
-/
namespace Identity.ProviderUsageEvidence

open Identity.Household

/-- A Node owned by `user`, whose node key `runtime` is its runtime reader.
Agents `a` and `b` each made one call, `call-a` and `call-b`, in their own
sessions. `b` shared its session with the Agent `shared` as a reader. -/
def fixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩,
      ⟨"agents", "agent-a", "user"⟩, ⟨"agents", "agent-b", "user"⟩,
      ⟨"sessions", "session-a", "a"⟩, ⟨"sessions", "session-b", "b"⟩,
      ⟨"children", "call-a", "a"⟩, ⟨"children", "call-b", "b"⟩],
    relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩,
      ⟨"agents", "agent-a", "parent", .object "nodes" "node"⟩,
      ⟨"agents", "agent-a", "executor", .actor "a"⟩,
      ⟨"agents", "agent-b", "parent", .object "nodes" "node"⟩,
      ⟨"agents", "agent-b", "executor", .actor "b"⟩,
      ⟨"sessions", "session-a", "parent", .object "agents" "agent-a"⟩,
      ⟨"sessions", "session-a", "working_agent", .actor "a"⟩,
      ⟨"sessions", "session-b", "parent", .object "agents" "agent-b"⟩,
      ⟨"sessions", "session-b", "working_agent", .actor "b"⟩,
      ⟨"sessions", "session-b", "reader", .actor "shared"⟩,
      ⟨"children", "call-a", "parent", .object "sessions" "session-a"⟩,
      ⟨"children", "call-b", "parent", .object "sessions" "session-b"⟩],
    enrolled := [] }

def calls : List String := ["call-a", "call-b"]

/-- Whether `viewer`, reading as itself, sees `call`. -/
def sees (viewer call : String) : Bool :=
  allowed 6 fixture viewer "children" call "read"

/-- What `viewer` sees of the evidence. -/
def evidence (viewer : String) : List String :=
  calls.filter (sees viewer)

theorem each_agent_sees_only_its_own_calls :
    (evidence "a", evidence "b") = (["call-a"], ["call-b"]) := by
  native_decide

/-- The operator's view is the union of every Agent's. -/
theorem the_operator_sees_the_union :
    evidence "user" = calls ∧
      calls.all (fun call => sees "user" call == (sees "a" call || sees "b" call)) = true := by
  native_decide

/-- A session a User shared with another Agent shares its calls, and nothing
else. -/
theorem a_shared_session_shares_its_calls :
    evidence "shared" = ["call-b"] := by
  native_decide

/-- Document ACP grants the runtime's node key and an unrelated actor none of
the evidence. -/
theorem the_runtime_is_granted_no_evidence :
    (evidence "runtime", evidence "stranger") = ([], []) := by
  native_decide

/-- No Agent can widen what it sees: the calls' `parent` and its session's
`reader` are managed by their owners, never by another Agent. -/
theorem no_agent_grants_itself_another_agents_evidence :
    ["a", "shared"].map (fun actor =>
      mayManage fixture actor ⟨"sessions", "session-b", "reader", .actor actor⟩) =
      [false, false] := by
  native_decide

end Identity.ProviderUsageEvidence
