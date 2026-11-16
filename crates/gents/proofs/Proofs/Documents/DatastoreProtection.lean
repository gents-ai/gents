/-!
Collections no datastore tool may read, discover or write. Evaluation records,
optimization jobs and client-only pending submissions stay out of the
agent-visible datastore.
-/

namespace Documents.DatastoreProtection

def protectedCollections : List String :=
  [ "EvalDefinition", "EvalRun", "EvalTrial", "EvalVerdict", "OptimizationJob"
  , "PendingChatSubmission" ]

/-- The protection gate runs before the collection scope check, so no scope
grant widens it. -/
def refused (collection : String) : Bool := protectedCollections.contains collection

def readRefused (collection : String) : Bool := refused collection

/-- Discovery takes the same gate as reads: the field inventory is withheld for
exactly the collections a read refuses. -/
def discoveryRefused (collection : String) : Bool := refused collection

theorem refuses_a_protected_collection_even_when_unrestricted :
    ∀ collection ∈ protectedCollections, readRefused collection = true := by decide

theorem refuses_discovery_of_a_protected_collection :
    ∀ collection ∈ protectedCollections, discoveryRefused collection = true := by decide

theorem agent_request_is_not_protected : readRefused "AgentRequest" = false := by decide

end Documents.DatastoreProtection
