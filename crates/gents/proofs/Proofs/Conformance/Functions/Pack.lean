import Proofs.Pack.Operations
import Proofs.Pack.CorePacks
import Proofs.Conformance.Family

namespace Pack.Operations

deriving instance Lean.ToJson for
  Actor, Digest, DigestAdmission, Edited, EditedAdmission, VariableSource, RootReader, ReaderAction,
  HostRootSource

attribute [conformance_function] digestAdmission editedAdmission variableSource readerAction
  hostRootSource hostRootSupplied

end Pack.Operations

namespace Pack.CorePacks

deriving instance Lean.ToJson for CorePack, Origin, Wiring, InterfaceAdmission, UpdateDecision,
  ProfileState, SlotBinding, EnableRoute

attribute [conformance_function] approvalRequired initWiring enabledWiring updateDecision
  slotBinding enableRoute refuseInit

structure ObservedEnabledInput where
  pack : CorePack
  observed : Wiring
  deriving Lean.ToJson

/-- Every core pack and all combinations of observed wiring flags. -/
@[conformance_family]
def observedEnabledCases : Conformance.Family ObservedEnabledInput Bool where
  inputs := all.flatMap fun pack =>
    [false, true].flatMap fun wired =>
      [false, true].flatMap fun toolCalls =>
        [false, true].flatMap fun consentDynamic =>
          [false, true].flatMap fun rootReaders =>
            [false, true].map fun agentsActive =>
              { pack, observed := { wired, toolCalls, consentDynamic, rootReaders, agentsActive } }
  model := fun input => observedEnabled input.pack input.observed

structure InterfaceInput where
  declared : Option Nat
  deriving Lean.ToJson

/-- Undeclared, below the oldest version, the current one and the next. -/
@[conformance_family]
def interfaceAdmissionCases : Conformance.Family InterfaceInput InterfaceAdmission where
  inputs := [none, some (oldestInterface - 1), some currentInterface, some (currentInterface + 1)]
    |>.map fun declared => { declared }
  model := fun input => interfaceAdmission input.declared

end Pack.CorePacks
