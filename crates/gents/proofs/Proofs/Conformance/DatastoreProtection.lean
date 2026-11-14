import Proofs.Documents.DatastoreProtection
import Proofs.ToolCalls.Identity.ClientAgentJournal
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

/-- A datastore tool declaration names its collection; validation refuses
exactly the protected ones. -/
@[conformance_family]
def declaration : Family RefusalInput Bool where
  inputs := (protectedCollections ++ ["Notes"]).map fun collection => { collection }
  model := fun i => refused i.collection

/-- A plugin granted a collection writes into it unless it is protected. -/
@[conformance_family]
def pluginWrite : Family RefusalInput Bool where
  inputs := (protectedCollections ++ ["Note"]).map fun collection => { collection }
  model := fun i => refused i.collection

example : refused Identity.ClientAgentJournal.protectedCollection = true := by decide

end Conformance.DatastoreProtection
