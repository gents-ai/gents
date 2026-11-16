namespace RuntimeReconcile.HttpAccess

/-- `admin` is a DID native NAC reports as the served node's owner or `admin`
relation: the client key (the one User DID shared by desktop, CLI and shims),
which `gents init` grants HouseholdOwner. The adapter learns it by asking
native NAC as the caller; holding a bearer alone never makes a DID an admin. -/
inductive Identity where
  | anonymous
  | other
  | operator
  | admin
  deriving DecidableEq, Repr

/-- `operator` routes act with the served node's own database principal and
stay with the node's DID. `snapshot` routes (`/fleet/slots`) and the private
part of `bootstrap` (`/status`) only read, and read as their caller. -/
inductive Surface where
  | bootstrap
  | operator
  | snapshot
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
private node reads require the DID that owns the served node or a native admin
of it. -/
def decide (authorityMatches : Bool) (surface : Surface) (identity : Identity) : Decision :=
  if !authorityMatches then .misdirected
  else match surface, identity with
    | .bootstrap, .operator => .operator
    | .bootstrap, .admin => .operator
    | .bootstrap, _ => .bootstrap
    | _, .anonymous => .unauthorized
    | _, .other => .forbidden
    | .operator, .admin => .forbidden
    | _, _ => .operator

theorem private_read_requires_operator (authorityMatches : Bool) (identity : Identity)
    (h : decide authorityMatches .operator identity = .operator) :
    authorityMatches = true ∧ identity = .operator := by
  cases authorityMatches <;> cases identity <;> simp_all [decide]

theorem caller_read_requires_operator_or_admin (authorityMatches : Bool) (surface : Surface)
    (identity : Identity) (h_surface : surface ≠ .operator)
    (h : decide authorityMatches surface identity = .operator) :
    authorityMatches = true ∧ (identity = .operator ∨ identity = .admin) := by
  cases authorityMatches <;> cases surface <;> cases identity <;> simp_all [decide]

/-- The principal a dispatched private read runs as. Private snapshots are
read with the caller's own forwarded bearer, so native ACP filters them for the
caller; an admin's read never borrows the served node's principal. -/
def readsAs (authorityMatches : Bool) (surface : Surface) (identity : Identity) :
    Option Identity :=
  if decide authorityMatches surface identity = .operator then some identity else none

theorem private_read_runs_as_its_caller (authorityMatches : Bool) (surface : Surface)
    (identity : Identity) (reader : Identity)
    (h : readsAs authorityMatches surface identity = some reader) : reader = identity := by
  unfold readsAs at h
  split at h <;> simp_all

theorem admin_read_never_runs_as_node (authorityMatches : Bool) (surface : Surface) :
    readsAs authorityMatches surface .admin ≠ some .operator := by
  unfold readsAs
  split <;> simp

theorem foreign_authority_never_dispatches (surface : Surface) (identity : Identity) :
    decide false surface identity = .misdirected := by
  simp [decide]

theorem anonymous_bootstrap_never_reads_as_node (authorityMatches : Bool) :
    decide authorityMatches .bootstrap .anonymous ≠ .operator := by
  cases authorityMatches <;> simp [decide]

end RuntimeReconcile.HttpAccess
