import Proofs.Runtime.BackendHealth.Executable
import Proofs.Conformance.Family

namespace Conformance.BackendProbeSchedule

open Lean Proofs.BackendHealth

deriving instance ToJson for ProbeConfiguration, BackendField

structure SelectInput where
  known : List ProbeConfiguration
  current : List ProbeConfiguration
  periodic : Bool
  deriving ToJson

structure SelectOutput where
  due : List Nat
  observed : List ProbeConfiguration
  deriving ToJson

@[conformance_family]
def select : Family SelectInput SelectOutput where
  inputs := scheduleCases.map fun c => { known := c.known, current := c.current, periodic := c.periodic }
  model := fun i =>
    { due := scheduledProbes i.known i.current i.periodic
      observed := observedProbeConfigurations i.current }

structure FieldEditInput where
  field : BackendField
  repeated : Bool
  deriving ToJson

/-- A schedule that knows backend zero at revision zero sees one edit of
`field`; `repeated` sees the same edited configuration a second time. -/
@[conformance_family]
def fieldEdit : Family FieldEditInput (List Nat) where
  inputs := [BackendField.provider, .wireApi, .endpoint, .auth, .connectTimeout,
      .discoveryTimeout, .name, .tags, .capacity, .queueCapacity].flatMap fun field =>
    [false, true].map fun repeated => { field, repeated }
  model := fun i =>
    let edited := editedConfiguration i.field
    let known := if i.repeated then edited else { edited with revision := 0 }
    scheduledProbes [known] [edited] false

end Conformance.BackendProbeSchedule
