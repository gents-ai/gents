import Proofs.Conformance.Contracts.Json.Core

namespace Conformance.PackSourceSelection

def select (sameDigest : Bool) (prior proposed : Option String) : Option String :=
  if sameDigest && (proposed.isNone || proposed == some "installed") then prior else proposed

theorem installed_keeps_prior (prior : Option String) :
    select true prior (some "installed") = prior := by simp [select]

private def jsonOption : Option String → String
  | none => "null"
  | some value => Conformance.Contracts.jsonString value

def casesJson : String :=
  Conformance.Contracts.jsonArray <|
    [false, true].flatMap fun sameDigest =>
      [none, some "registry"].flatMap fun prior =>
        [none, some "installed", some "store"].map fun proposed =>
          "{\"same_digest\":" ++ toString sameDigest ++
          ",\"prior\":" ++ jsonOption prior ++ ",\"proposed\":" ++ jsonOption proposed ++
          ",\"selected\":" ++ jsonOption (select sameDigest prior proposed) ++ "}"

end Conformance.PackSourceSelection

def main : IO Unit := IO.println Conformance.PackSourceSelection.casesJson
