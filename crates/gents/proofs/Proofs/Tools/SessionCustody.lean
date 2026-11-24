import Proofs.Store.Identity.Operations

/-! What the tool surface offers a managed Agent on a session it published. -/

namespace Tools.SessionCustody

open Identity.Operations

/-- The policy resource of an `AgentSession` document. -/
def sessionResource : String := "sessions"

/-- Whether the policy tool performs a change of one document relationship on
a document of `collection`: a grant when `revoke` is false, a removal when it
is true.

Decision: a managed Agent never detaches a session it published from itself,
never withdraws a relation that carries a User's access to it, and never
deletes it; each would hide the Agent's activity from the User it works for.
The Agent is the native owner of a session it publishes
(`Identity.DelegatedPublication.commit`, `Identity.Household.commitAgentSession`),
and the native owner holds every permission on its document, so document ACP
admits those changes. The tool surface therefore offers no operation that
makes them: the policy tool refuses to remove any relation of a document whose
policy resource is `sessions`, and leaves every other change, grants on a
session included, to native authorization. A session document is deleted by no
Agent-reachable operation (the structure rule
`agent_reachable_surfaces_never_detach_or_delete_a_session`). -/
def relationChangeOffered (collection : String) (revoke : Bool) : Bool :=
  !(revoke && networkProtocolResource collection == some sessionResource)

theorem no_session_relation_is_removed (collection : String)
    (session : networkProtocolResource collection = some sessionResource) :
    relationChangeOffered collection true = false := by
  simp [relationChangeOffered, session]

theorem agent_session_relations_are_kept :
    relationChangeOffered "AgentSession" true = false := by native_decide

theorem grants_stay_with_native_authorization (collection : String) :
    relationChangeOffered collection false = true := by
  simp [relationChangeOffered]

theorem other_documents_stay_with_native_authorization :
    relationChangeOffered "Agent" true = true ∧
      relationChangeOffered "AgentRequest" true = true ∧
      relationChangeOffered "Shipment" true = true := by native_decide

end Tools.SessionCustody
