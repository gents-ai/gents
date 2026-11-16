import Proofs.Tools.SelfConfig.ConfigurationDiscovery
import Proofs.Conformance.Family

namespace Conformance.ConfigurationDiscovery

open Lean SelfConfig.ConfigurationDiscovery

deriving instance ToJson for Root
deriving instance ToJson for FileCondition
deriving instance ToJson for Notice
deriving instance ToJson for Outcome
deriving instance ToJson for Source

structure SourceReport where
  outcome : Outcome
  warnings : List Notice
  truncations : List Notice
  deriving ToJson

structure Request where
  sources : List Source
  deriving ToJson

structure Inventory where
  sources : List SourceReport
  deriving ToJson

@[conformance_family]
def scan : Family Request Inventory where
  inputs := List.map Request.mk
    [ [⟨.present, some .malformed, some true⟩, ⟨.missing, none, none⟩]
    , [⟨.present, some .wellFormed, some false⟩]
    , [⟨.present, none, none⟩]
    , [⟨.present, some .oversized, none⟩]
    , [⟨.present, none, some true⟩]
    , [⟨.relative, none, none⟩]
    , [⟨.notDirectory, none, none⟩]
    , [⟨.missing, none, none⟩, ⟨.present, some .malformed, none⟩] ]
  model := fun i =>
    ⟨i.sources.map fun s => { outcome := outcome s, warnings := warnings s, truncations := truncations s }⟩

end Conformance.ConfigurationDiscovery
