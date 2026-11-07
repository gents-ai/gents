namespace SessionHydration.Discovery

inductive Field where
  | missing
  | null
  | text (value : String)
  deriving DecidableEq, Repr

inductive FieldState where
  | missing
  | null
  | empty
  | present
  deriving DecidableEq, Repr

def fieldState : Field → FieldState
  | .missing => .missing
  | .null => .null
  | .text value => if value.isEmpty then .empty else .present

structure Identity where
  key : Field
  requester : Field
  node : Field
  session : Field
  deriving DecidableEq, Repr

def hasText : Field → Bool
  | .text _ => true
  | _ => false

/-- A null or missing component cannot be used in the existing identity CAS.
Empty text remains available to the existing validator and rejection path. -/
def identifiable (identity : Identity) : Bool :=
  fieldState identity.key == .present && hasText identity.requester &&
    hasText identity.node && hasText identity.session

theorem identifiable_requires_all_fields (identity : Identity)
    (h : identifiable identity = true) :
    fieldState identity.key = .present ∧ hasText identity.requester = true ∧
      hasText identity.node = true ∧ hasText identity.session = true := by
  simpa [identifiable, and_assoc] using h

theorem null_requester_is_not_identifiable (identity : Identity)
    (h : identity.requester = .null) : identifiable identity = false := by
  simp [identifiable, h, hasText]

theorem missing_node_is_not_identifiable (identity : Identity)
    (h : identity.node = .missing) : identifiable identity = false := by
  simp [identifiable, h, hasText]

theorem empty_text_remains_identifiable :
    identifiable ⟨.text "key", .text "", .text "node", .text "session"⟩ = true := by
  rfl

def fieldCases : List Field := [.missing, .null, .text "", .text "value"]

def identityCases : List Identity :=
  fieldCases.flatMap fun key =>
    fieldCases.flatMap fun requester =>
      fieldCases.flatMap fun node =>
        fieldCases.map fun session => ⟨key, requester, node, session⟩

end SessionHydration.Discovery
