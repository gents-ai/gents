namespace ToolPolicy.FileToolAdmission

/-- The file tool modes an agent can hold, least authority first. -/
inductive FileMode where
  | off | readOnly | readWrite
  deriving DecidableEq, Repr

/-- The pack authoring verbs that reach the tool root through the effective file
mode. The write flag of each verb is the one its production caller passes to the
admission check; a verb that only reads the tool root is not a writer. -/
inductive Operation where
  | scaffold | add | exportPack | build | test | check | verify
  deriving DecidableEq, Repr

def Operation.name : Operation → String
  | .scaffold => "scaffold"
  | .add => "add"
  | .exportPack => "export"
  | .build => "build"
  | .test => "test"
  | .check => "check"
  | .verify => "verify"

def Operation.writes : Operation → Bool
  | .scaffold | .add | .exportPack | .build | .test => true
  | .check | .verify => false

/-- A write inside the tool root needs readwrite; a read needs readonly or
better. Off admits neither. -/
def admits : FileMode → Bool → Bool
  | .off, _ => false
  | .readOnly, write => !write
  | .readWrite, _ => true

/-- The refusal names the missing permission: an Off agent lacks file tools
altogether, a readonly agent lacks the write. -/
def refusal (mode : FileMode) (write : Bool) : Option String :=
  if admits mode write then none
  else some (match mode with
    | .off => "file_permission"
    | _ => "readwrite")

theorem a_write_needs_readwrite (mode : FileMode) :
    admits mode true = true ↔ mode = .readWrite := by
  cases mode <;> decide

theorem a_read_needs_more_than_off (mode : FileMode) :
    admits mode false = true ↔ mode ≠ .off := by
  cases mode <;> decide

theorem write_authoring_requires_readwrite_and_verify_only_read (op : Operation) (mode : FileMode) :
    admits mode op.writes = true ↔ (if op.writes then mode = .readWrite else mode ≠ .off) := by
  cases op <;> cases mode <;> decide

end ToolPolicy.FileToolAdmission
