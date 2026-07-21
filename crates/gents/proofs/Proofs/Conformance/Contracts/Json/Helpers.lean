import Proofs.Conformance.Contracts.Machines

/-!
# Shared JSON Helpers

Helpers used by the conformance JSON serializer shards.
-/

namespace Conformance.Contracts

def jsonStringMatrix (values : List (List String)) : String :=
  jsonArray (values.map jsonStringArray)

def jsonOptionalStringArray : Option (List String) → String
  | none => "null"
  | some values => jsonStringArray values

def jsonOptionalNat : Option Nat → String
  | none => "null"
  | some value => toString value

end Conformance.Contracts
