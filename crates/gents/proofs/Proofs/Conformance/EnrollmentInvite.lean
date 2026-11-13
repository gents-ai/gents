import Proofs.P2P.Enrollment.Invite
import Proofs.Conformance.Family

namespace Conformance.EnrollmentInvite

open Lean

@[conformance_family]
def linkable : Family Nat Bool where
  inputs := [0, 1, 1365, 1899, 1900, 1901, 32768]
  model := Enrollment.Invite.linkable

structure WindowInput where
  issued : Nat
  expires : Nat
  now : Nat
  deriving ToJson

@[conformance_family]
def admissible : Family WindowInput Bool where
  inputs := [
    { issued := 1000000, expires := 1003600, now := 1000000 },
    { issued := 1000030, expires := 1000100, now := 1000000 },
    { issued := 1000031, expires := 1000100, now := 1000000 },
    { issued := 999900, expires := 1000000, now := 1000000 },
    { issued := 1000000, expires := 1086400, now := 1000000 },
    { issued := 1000000, expires := 1086401, now := 1000000 },
    { issued := 1000001, expires := 1000000, now := 999999 }
  ]
  model := fun i => Enrollment.Invite.admissible 1 i.issued i.expires i.now

structure SchemaInput where
  signed : Option String
  observed : Option String
  deriving ToJson

@[conformance_family]
def schemaEvidence : Family SchemaInput Bool where
  inputs := [
    { signed := none, observed := none },
    { signed := none, observed := some "a" },
    { signed := some "a", observed := none },
    { signed := some "", observed := some "" },
    { signed := some "a", observed := some "a" },
    { signed := some "a", observed := some "b" }
  ]
  model := fun i => Enrollment.Invite.schemaEvidenceMatches i.signed i.observed

end Conformance.EnrollmentInvite
