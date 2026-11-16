import Proofs.Store.Identity.EvalRecords
import Proofs.Conformance.Family

namespace Conformance.EvalRecords

open Lean Identity.Household Identity.EvalRecords

structure PublishInput where
  name : String
  creator : String
  resource : String
  nodeVisible : Bool
  relationshipSucceeded : Bool
  commitSucceeded : Bool
  deriving ToJson

structure PublishOutput where
  created : Bool
  userRead : Bool
  strangerRead : Bool
  deriving ToJson

def PublishInput.ofCase (c : Case) : PublishInput :=
  { name := c.name, creator := c.creator, resource := c.resource, nodeVisible := c.nodeVisible
    relationshipSucceeded := c.related, commitSucceeded := c.committed }

def publishOutput (i : PublishInput) : PublishOutput :=
  let s := publish initial i.creator i.name "home" i.resource i.nodeVisible true
    i.relationshipSucceeded i.commitSucceeded
  { created := s.objects.any fun x => x.resource == i.resource && x.id == i.name
    userRead := allowed 5 s "user" i.resource i.name "read"
    strangerRead := allowed 5 s "stranger" i.resource i.name "read" }

/-- A served node publishes each record's Node parent in the creating
transaction, so a failed parent relationship or a failed commit cannot be
staged from outside the eval document owners. -/
def failedPublicationNames : List String := ["relationship-failed", "commit-failed"]

@[conformance_family]
def published : Family PublishInput PublishOutput where
  inputs := (cases.filter fun c => c.name ∉ failedPublicationNames).map PublishInput.ofCase
  model := publishOutput

/-- model-production-gap: no production path lets the Node parent relationship
fail or the commit fail after an eval record create is staged. -/
@[conformance_family]
def publishedUnreached : Family PublishInput PublishOutput where
  inputs := (cases.filter fun c => c.name ∈ failedPublicationNames).map PublishInput.ofCase
  model := publishOutput

end Conformance.EvalRecords
