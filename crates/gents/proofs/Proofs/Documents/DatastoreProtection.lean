/-!
Collections no datastore tool may read, discover or write. Evaluation records,
optimization jobs and client-only pending submissions stay out of the
agent-visible datastore.
-/

namespace Documents.DatastoreProtection

def protectedCollections : List String :=
  [ "EvalDefinition", "EvalRun", "EvalTrial", "EvalVerdict", "OptimizationJob"
  , "PendingChatSubmission", "ClientAuthoredSubmission", "ClientOutboxSequence" ]

/-- The protection gate runs before the collection scope check, so no scope
grant widens it. -/
def refused (collection : String) : Bool := protectedCollections.contains collection

def readRefused (collection : String) : Bool := refused collection

/-- Discovery takes the same gate as reads: the field inventory is withheld for
exactly the collections a read refuses. -/
def discoveryRefused (collection : String) : Bool := refused collection

/-- Whether a declared write tool may not name its collection. Decision
(gents#3831): beyond the protected collections, a declaration never names a
collection Gents manages, as a page create, a plugin's data calls and the
schema tool already refuse one: a managed collection's documents are written
by the owners Gents gives them, and a write tool an Agent signs would make
the Agent their author. The mailbox is the one managed collection with a
declaration of its own, the canonical `file_mailbox_item`, which its own
validation fixes and the bounded write tool refuses separately. `managed` is
the collection being in the product's baseline schema catalog or replicated by
the protocol. -/
def writeDeclarationRefused («protected» managed mailbox : Bool) : Bool :=
  «protected» || (managed && !mailbox)

theorem a_managed_collection_other_than_the_mailbox_is_refused (protectedName : Bool) :
    writeDeclarationRefused protectedName true false = true := by
  cases protectedName <;> rfl

theorem an_application_collection_is_declared (mailbox : Bool) :
    writeDeclarationRefused false false mailbox = false := by
  cases mailbox <;> rfl

theorem refuses_a_protected_collection_even_when_unrestricted :
    ∀ collection ∈ protectedCollections, readRefused collection = true := by decide

theorem refuses_discovery_of_a_protected_collection :
    ∀ collection ∈ protectedCollections, discoveryRefused collection = true := by decide

theorem agent_request_is_not_protected : readRefused "AgentRequest" = false := by decide

end Documents.DatastoreProtection
