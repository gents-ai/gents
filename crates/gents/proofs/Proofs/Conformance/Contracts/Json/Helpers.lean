import Proofs.Conformance.Contracts.Machines

namespace Conformance.Contracts

def jsonStringMatrix (values : List (List String)) : String :=
  jsonArray (values.map jsonStringArray)

def jsonOptionalStringArray : Option (List String) → String
  | none => "null"
  | some values => jsonStringArray values

def jsonOptionalNat : Option Nat → String
  | none => "null"
  | some value => toString value

def jsonOptionalBool : Option Bool → String
  | none => "null"
  | some true => "true"
  | some false => "false"

end Conformance.Contracts
