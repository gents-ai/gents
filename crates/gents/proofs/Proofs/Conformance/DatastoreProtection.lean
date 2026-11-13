import Proofs.Documents.DatastoreProtection
import Proofs.Conformance.Family

namespace Conformance.DatastoreProtection

open Lean Documents.DatastoreProtection

structure RefusalInput where
  collection : String
  deriving ToJson

structure RefusalOutput where
  readRefused : Bool
  discoveryRefused : Bool
  deriving ToJson

@[conformance_family]
def refusal : Family RefusalInput RefusalOutput where
  inputs := (protectedCollections ++ ["AgentRequest"]).map fun collection => { collection }
  model := fun i =>
    { readRefused := readRefused i.collection
      discoveryRefused := discoveryRefused i.collection }

end Conformance.DatastoreProtection
