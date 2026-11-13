import Proofs.Conformance.ContractCases.RenderedCapture
import Proofs.Conformance.Family

namespace Conformance.CaptureScope

open Lean Conformance.ContractCases

structure LabelInput where
  label : String
  deriving ToJson

structure ScopeObservation where
  valid : Bool
  kind : String
  seq : Nat
  deriving ToJson

/-- A label parses to the scope it names; any other string is refused with no
kind and no sequence. -/
@[conformance_family]
def parse : Family LabelInput ScopeObservation where
  inputs := captureScopeCases.map fun c => { label := c.label }
  model := fun i =>
    match scopeOfLabel? i.label with
    | some (kind, seq) => { valid := true, kind := kind.label, seq }
    | none => { valid := false, kind := "", seq := 0 }

example : captureScopeCases.all fun c =>
    let o := parse.model { label := c.label }
    o.valid == c.valid && o.kind == c.kind && o.seq == c.seq := by
  native_decide

end Conformance.CaptureScope
