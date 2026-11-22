import Proofs.Tools.HostPreset
import Proofs.Conformance.Functions.Session

namespace Tools.HostPreset

deriving instance Lean.ToJson for Package
deriving instance Lean.ToJson for Commands

attribute [conformance_function] bash

end Tools.HostPreset
