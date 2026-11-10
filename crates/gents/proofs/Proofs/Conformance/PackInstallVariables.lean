import Proofs.Configuration
import Proofs.Conformance.ContractTypes

open Configuration Conformance.Contracts

private def previousCases : List (Option (Option String)) :=
  [none, some none, some (some "previous")]

private def overrideCases : List (Option (Option String)) :=
  [none, some none, some (some "override")]

private def nestedValue (value : Option (Option String)) : String :=
  match value with
  | none => "{\"present\":false}"
  | some inner => "{\"present\":true,\"value\":" ++ jsonOptionalString inner ++ "}"

private def render (dependency installed : Bool) (previous override : Option (Option String))
    (ambient : Option String) : String :=
  "{\"dependency\":" ++ (if dependency then "true" else "false") ++
    ",\"installed\":" ++ (if installed then "true" else "false") ++
    ",\"previous\":" ++ nestedValue previous ++
    ",\"override\":" ++ nestedValue override ++
    ",\"ambient\":" ++ jsonOptionalString ambient ++
    ",\"result\":" ++ nestedValue (resolvePackInstallVariable dependency installed previous override ambient) ++ "}"

def main : IO Unit := do
  let cases := [false, true].flatMap fun dependency =>
    [false, true].flatMap fun installed =>
      previousCases.flatMap fun previous =>
        overrideCases.flatMap fun override =>
          [none, some "ambient"].map fun ambient => render dependency installed previous override ambient
  IO.println (jsonArray cases)
