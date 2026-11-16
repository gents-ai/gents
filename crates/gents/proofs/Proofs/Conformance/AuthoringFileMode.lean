import Proofs.Tools.ToolPolicy.FileToolAdmission
import Proofs.Conformance.Family

namespace Conformance.AuthoringFileMode

open Lean ToolPolicy.FileToolAdmission

def modeWire : FileMode → String
  | .off => "Off"
  | .readOnly => "ReadOnly"
  | .readWrite => "ReadWrite"

/-- Only the three wire names are emitted; any other name reads as Off, the least authority. -/
def modeOfWire : String → FileMode
  | "ReadOnly" => .readOnly
  | "ReadWrite" => .readWrite
  | _ => .off

structure AdmissionInput where
  mode : String
  operation : String
  write : Bool
  deriving ToJson

structure AdmissionOutput where
  admitted : Bool
  refusal : Option String
  deriving ToJson

@[conformance_family]
def admission : Family AdmissionInput AdmissionOutput where
  inputs := [FileMode.off, .readOnly, .readWrite].flatMap fun mode =>
    [Operation.scaffold, .add, .exportPack, .build, .test, .check, .verify].map fun op =>
      ({ mode := modeWire mode, operation := op.name, write := op.writes } : AdmissionInput)
  model := fun i =>
    { admitted := admits (modeOfWire i.mode) i.write
      refusal := refusal (modeOfWire i.mode) i.write }

end Conformance.AuthoringFileMode
