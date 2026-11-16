import Proofs.Clients.IdentityMetadata
import Proofs.Conformance.Family
import Lean

namespace Conformance.ClientIdentityMetadata
open Lean _root_.ClientIdentityMetadata

deriving instance ToJson for Stored, Read, Decision

structure ReadWire where
  read : Read
  stored : Stored
  deriving ToJson

@[conformance_family]
def decisions : Conformance.Family ReadWire Decision where
  inputs := [Read.first, .afterInstall].flatMap fun read =>
    [Stored.absent, .equal, .different].map fun stored => { read, stored }
  model := fun i => decide i.read i.stored

end Conformance.ClientIdentityMetadata
