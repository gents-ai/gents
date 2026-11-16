import Proofs.Conformance.BackendHealthAdmission

def tlaBool (value : Bool) : String := if value then "TRUE" else "FALSE"

def main : IO Unit := do
  let family := Conformance.BackendHealthAdmission.probeRequest
  let rows := family.inputs.map fun input =>
    s!"<<{tlaBool input.awaitingProbe}, {tlaBool input.startupFailed}, {tlaBool (family.model input).waiting}>>"
  IO.println "---- MODULE BackendFirstProbeCases ----"
  IO.println ("AwaitProbeTable == {" ++ String.intercalate ", " rows ++ "}")
  IO.println "===="
