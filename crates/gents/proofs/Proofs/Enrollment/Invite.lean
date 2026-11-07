import Proofs.Enrollment.State

namespace Enrollment.Invite

/-- A compact invite has a bounded encoded size and a bounded validity
window. It is an invitation to request approval, not an authorization. -/
def maxTokenCharacters : Nat := 1900
def maxLifetimeSeconds : Nat := 86400
def defaultLifetimeSeconds : Nat := 3600

def linkable (tokenCharacters : Nat) : Bool :=
  decide (tokenCharacters > 0 ∧ tokenCharacters ≤ maxTokenCharacters)

def admissible (tokenCharacters issued expires now : Nat) : Bool :=
  linkable tokenCharacters &&
  decide (issued ≤ now + 30 ∧ now < expires ∧ issued ≤ expires ∧
    expires - issued ≤ maxLifetimeSeconds)

/-- The digest commits to the ordered collection names, version identifiers,
branchability and policy resources. Matching digests rely on SHA-256 collision
resistance; missing schema evidence is never treated as agreement. -/
def schemaEvidenceMatches (signedDigest localDigest : Option String) : Bool :=
  match signedDigest, localDigest with
  | some signed, some observed => signed != "" && signed == observed
  | _, _ => false

def schemaEvidenceFields (digest : String) : List String :=
  ["live-schema-v1", digest]

theorem schema_evidence_is_signed (digest : String) :
    schemaEvidenceFields digest = ["live-schema-v1", digest] := by rfl

theorem missing_schema_evidence_rejected (observed : Option String) :
    schemaEvidenceMatches none observed = false := by rfl

theorem different_schema_evidence_rejected (signed observed : String) (h : signed ≠ observed) :
    schemaEvidenceMatches (some signed) (some observed) = false := by
  simp [schemaEvidenceMatches, h]

theorem oversized_rejected (size issued expires now : Nat) (h : maxTokenCharacters < size) :
    admissible size issued expires now = false := by
  simp [admissible, linkable, Nat.not_le.mpr h]

theorem expired_rejected (size issued expires now : Nat) (h : expires ≤ now) :
    admissible size issued expires now = false := by
  simp [admissible, Nat.not_lt.mpr h]

theorem default_within_max : defaultLifetimeSeconds ≤ maxLifetimeSeconds := by decide

end Enrollment.Invite
