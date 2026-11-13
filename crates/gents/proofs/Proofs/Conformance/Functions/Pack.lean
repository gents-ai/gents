import Proofs.Pack.Operations
import Proofs.Pack.CorePacks
import Proofs.Conformance.Pack
import Proofs.Conformance.Family

namespace Pack.Operations

deriving instance Lean.ToJson for
  Actor, Digest, DigestAdmission, Edited, EditedAdmission, VariableSource, RootReader, ReaderAction

attribute [conformance_function] digestAdmission editedAdmission variableSource readerAction

end Pack.Operations

namespace Pack.CorePacks

deriving instance Lean.ToJson for
  CorePack, Origin, Wiring, Release, InterfaceAdmission, UpdateDecision

attribute [conformance_function] approvalRequired initWiring enabledWiring updateDecision

structure InterfaceInput where
  runtime : Release
  declared : Option Release
  deriving Lean.ToJson

private def releases : List Release :=
  [⟨0, 21, 0, none⟩, ⟨1, 0, 0, some 1⟩, ⟨1, 0, 0, some 2⟩, ⟨1, 0, 0, some 10⟩, ⟨1, 0, 0, none⟩,
    ⟨1, 0, 1, none⟩, ⟨1, 1, 0, some 1⟩, ⟨1, 1, 0, none⟩, ⟨2, 0, 0, none⟩]

/-- Every pairing of runtime and declared release over candidates, releases,
patch, minor and major steps, and no declaration. -/
@[conformance_family]
def interfaceAdmissionCases : Conformance.Family InterfaceInput InterfaceAdmission where
  inputs := releases.flatMap fun runtime =>
    (none :: releases.map some).map fun declared => { runtime, declared }
  model := fun input => interfaceAdmission input.runtime input.declared

end Pack.CorePacks

namespace Pack.AllowedFolders

deriving instance Lean.ToJson for Target

attribute [conformance_function] fixtureGrant

end Pack.AllowedFolders
