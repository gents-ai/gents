import Proofs.Shared.CompletionRetry.CountCarrier

/-! A configuration document can name a field of another collection and ask
something of it: a datastore surface's output obligation and an event source's
group both name the field that carries an expected count, and an event source
names the field that correlates its documents. Publication checks the named
field against the collection as the database declares it. -/

namespace SchemaObligations

open CompletionRetry.CountCarrier (Field canCarry)

inductive Scalar where
  | int
  | float32
  | float64
  | string
  | id
  | blob
  | json
  | dateTime
  | boolean
  | stringList
  deriving DecidableEq, Repr

/-- Premise (DefraDB schema introspection): a field reports its scalar by
name, a non-null field as that name with `!`, and every list as `LIST`. -/
structure FieldType where
  scalar : Scalar
  required : Bool
  deriving DecidableEq, Repr

structure FieldDecl where
  name : String
  type : FieldType
  deriving DecidableEq, Repr

def Scalar.carrier : Scalar → Field
  | .int => .integer
  | .float32 | .float64 => .number
  | .string | .blob => .text
  | .id => .docId
  | .json => .json
  | .dateTime => .dateTime
  | .boolean => .boolean
  | .stringList => .list

inductive Demand where
  | count
  | correlation
  deriving DecidableEq, Repr

/-- Nullability never decides: a required field carries what its scalar
carries. -/
def satisfies : Demand → FieldType → Bool
  | .count, type => canCarry type.scalar.carrier
  | .correlation, type => type.scalar == .string

/-- What a payload asks of the collection it names. `registered` is whether
the database has that collection at all. -/
structure Payload where
  registered : Bool
  count : Option String
  correlation : Option String
  deriving DecidableEq, Repr

def fieldAdmitted (schema : List FieldDecl) (demand : Demand) (name : String) : Bool :=
  match schema.find? (·.name == name) with
  | none => false
  | some declaration => satisfies demand declaration.type

/-- Decision: a payload naming a collection the database does not have yet is
admitted unjudged, so configuration can be published before the pack that
installs its collection. Once the collection exists, every named field must
exist on it and satisfy its demand. -/
def admitted (schema : List FieldDecl) (payload : Payload) : Bool :=
  !payload.registered ||
    (payload.correlation.all (fieldAdmitted schema .correlation) &&
      payload.count.all (fieldAdmitted schema .count))

/-- A plan carries a creation payload and a replacement payload for each
document. The one judged is the one publication would write: the creation
payload when no document is live, the replacement payload otherwise. -/
def judged (live : Option Payload) (create replacement : Payload) : Payload :=
  if live.isSome then replacement else create

def preflight (schema : List FieldDecl) (live : Option Payload)
    (create replacement : Payload) : Bool :=
  admitted schema (judged live create replacement)

structure Outcome where
  accepted : Bool
  live : Option Payload
  deriving DecidableEq, Repr

def publish (schema : List FieldDecl) (live : Option Payload)
    (create replacement : Payload) : Outcome :=
  if preflight schema live create replacement then
    { accepted := true, live := some (judged live create replacement) }
  else { accepted := false, live }

/-- Preflight and publication give the same verdict. -/
theorem preflight_is_publication_verdict (schema : List FieldDecl) (live : Option Payload)
    (create replacement : Payload) :
    (publish schema live create replacement).accepted =
      preflight schema live create replacement := by
  unfold publish
  split <;> simp_all

/-- A refused publication leaves what was live. -/
theorem refusal_publishes_nothing {schema : List FieldDecl} {live : Option Payload}
    {create replacement : Payload}
    (h : (publish schema live create replacement).accepted = false) :
    (publish schema live create replacement).live = live := by
  unfold publish at h ⊢
  split <;> simp_all

/-- Whatever is live after a publication that changed it was admitted. -/
theorem published_payload_is_admitted {schema : List FieldDecl} {live : Option Payload}
    {create replacement payload : Payload}
    (h_accepted : (publish schema live create replacement).accepted = true)
    (h_live : (publish schema live create replacement).live = some payload) :
    admitted schema payload = true := by
  unfold publish at h_accepted h_live
  split at h_live
  · rename_i h
    cases h_live
    exact h
  · simp_all

/-- A creation is judged on its creation payload alone. -/
theorem creation_ignores_replacement_payload (schema : List FieldDecl)
    (create first second : Payload) :
    publish schema none create first = publish schema none create second := by
  simp [publish, preflight, judged]

/-- A replacement is judged on its replacement payload alone. -/
theorem replacement_ignores_creation_payload (schema : List FieldDecl) (live : Payload)
    (first second replacement : Payload) :
    publish schema (some live) first replacement =
      publish schema (some live) second replacement := by
  simp [publish, preflight, judged]

/-- An admitted count field can store a count the runtime parses back. -/
theorem admitted_count_has_a_witness {schema : List FieldDecl} {name : String}
    (h : fieldAdmitted schema .count name = true) :
    ∃ declaration ∈ schema, declaration.name = name ∧
      ∃ encoding value, CompletionRetry.CountCarrier.carries
        declaration.type.scalar.carrier encoding value 10 := by
  unfold fieldAdmitted at h
  split at h
  · cases h
  · rename_i declaration h_found
    refine ⟨declaration, List.mem_of_find?_eq_some h_found, ?_, ?_⟩
    · simpa using List.find?_some h_found
    · exact (CompletionRetry.CountCarrier.admission_iff_witness _ 10 (by omega)).mp h

end SchemaObligations
