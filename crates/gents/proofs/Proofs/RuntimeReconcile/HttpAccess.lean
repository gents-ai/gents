namespace RuntimeReconcile.HttpAccess

inductive Identity where
  | anonymous
  | other
  | operator
  deriving DecidableEq, Repr

inductive Surface where
  | bootstrap
  | operator
  deriving DecidableEq, Repr

inductive Decision where
  | misdirected
  | unauthorized
  | forbidden
  | bootstrap
  | operator
  deriving DecidableEq, Repr

/-- The HTTP adapter supplies authority matching and the DID authenticated by
DefraDB's bearer extractor. Host matching is not authentication: an allowed
loopback authority never grants the node's principal to an anonymous caller.
Bootstrap contains only public identity, connection and contract observations;
private node reads require the same DID that owns the served node. -/
def decide (authorityMatches : Bool) (surface : Surface) (identity : Identity) : Decision :=
  if !authorityMatches then .misdirected
  else match surface, identity with
    | .operator, .anonymous => .unauthorized
    | .operator, .other => .forbidden
    | _, .operator => .operator
    | .bootstrap, _ => .bootstrap

theorem private_read_requires_operator (authorityMatches : Bool) (identity : Identity)
    (h : decide authorityMatches .operator identity = .operator) :
    authorityMatches = true ∧ identity = .operator := by
  cases authorityMatches <;> cases identity <;> simp_all [decide]

theorem foreign_authority_never_dispatches (surface : Surface) (identity : Identity) :
    decide false surface identity = .misdirected := by
  simp [decide]

theorem anonymous_bootstrap_never_reads_as_node (authorityMatches : Bool) :
    decide authorityMatches .bootstrap .anonymous ≠ .operator := by
  cases authorityMatches <;> simp [decide]

end RuntimeReconcile.HttpAccess
