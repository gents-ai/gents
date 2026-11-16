/-! Which home a command opens when none is passed on the command line: the
`GENTS_HOME` value, else the home remembered in the config directory, else
`~/.gents`. A remembered file that cannot be read, or whose contents are not
one absolute path line, is an error rather than a fallback. -/

namespace HomeResolution

/-- The `GENTS_HOME` variable: unset, set to nothing, or set to a value. -/
inductive Env where
  | absent | empty | set
  deriving DecidableEq, Repr

/-- The remembered-home file: missing, a directory at its path, or contents
of each shape the reader can meet. -/
inductive Remembered where
  | absent
  | unreadable
  | newlineOnly
  | empty
  | relative
  | multiLine
  | carriageReturn
  | absolute
  deriving DecidableEq, Repr

inductive Resolution where
  | fromEnv | fromRemembered | dotGents | unreadable | invalid | unresolved
  deriving DecidableEq, Repr

inductive Reading where
  | missing | failed | malformed | found
  deriving DecidableEq, Repr

/-- Only one absolute line is a remembered home. Any other contents, including
none at all, is malformed. -/
def classify : Remembered → Reading
  | .absent => .missing
  | .unreadable => .failed
  | .absolute => .found
  | .newlineOnly | .empty | .relative | .multiLine | .carriageReturn => .malformed

/-- Precedence: a set `GENTS_HOME` wins outright and the remembered file is not
read. An empty `GENTS_HOME` is treated as unset. Without a config directory
nothing is remembered. -/
def resolve (env : Env) (remembered : Remembered) (configDir userHome : Bool) : Resolution :=
  match env with
  | .set => .fromEnv
  | .absent | .empty =>
    let reading := if configDir then classify remembered else .missing
    match reading with
    | .failed => .unreadable
    | .malformed => .invalid
    | .found => .fromRemembered
    | .missing => if userHome then .dotGents else .unresolved

theorem a_set_environment_wins (remembered : Remembered) (configDir userHome : Bool) :
    resolve .set remembered configDir userHome = .fromEnv := rfl

theorem a_remembered_home_wins_over_dot_gents (env : Env) (userHome : Bool) (h : env ≠ .set) :
    resolve env .absolute true userHome = .fromRemembered := by
  cases env <;> first | exact absurd rfl h | rfl

theorem no_remembered_file_means_dot_gents (env : Env) (h : env ≠ .set) :
    resolve env .absent true true = .dotGents := by
  cases env <;> first | exact absurd rfl h | rfl

theorem an_unreadable_remembered_file_is_never_bypassed (env : Env) (userHome : Bool)
    (h : env ≠ .set) :
    resolve env .unreadable true userHome = .unreadable := by
  cases env <;> first | exact absurd rfl h | rfl

theorem a_remembered_file_that_is_not_one_absolute_line_is_invalid (env : Env) (userHome : Bool)
    (h : env ≠ .set) :
    resolve env .empty true userHome = .invalid
      ∧ resolve env .newlineOnly true userHome = .invalid
      ∧ resolve env .relative true userHome = .invalid
      ∧ resolve env .multiLine true userHome = .invalid
      ∧ resolve env .carriageReturn true userHome = .invalid := by
  cases env <;> first | exact absurd rfl h | exact ⟨rfl, rfl, rfl, rfl, rfl⟩

end HomeResolution
