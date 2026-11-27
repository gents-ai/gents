import Proofs.Conformance.Contracts.Machines

namespace Conformance.Contracts

def jsonOptionalNat : Option Nat → String
  | none => "null"
  | some value => toString value

def jsonOptionalBool : Option Bool → String
  | none => "null"
  | some true => "true"
  | some false => "false"

end Conformance.Contracts
