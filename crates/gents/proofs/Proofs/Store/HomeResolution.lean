/-! Which home a command opens when `--home` is not passed: the home remembered in the config
directory, else `~/.gents`. A remembered file that cannot be read, or whose contents are not one
absolute path line, is an error rather than a fallback. The environment never chooses a home. -/

namespace HomeResolution

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
  | fromRemembered | dotGents | unreadable | invalid | unresolved
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

/-- Precedence: the remembered file wins over `~/.gents`. Without a config directory nothing is
remembered. A command given `--home` never calls this. -/
def resolve (remembered : Remembered) (configDir userHome : Bool) : Resolution :=
  let reading := if configDir then classify remembered else .missing
  match reading with
  | .failed => .unreadable
  | .malformed => .invalid
  | .found => .fromRemembered
  | .missing => if userHome then .dotGents else .unresolved

theorem a_remembered_home_wins_over_dot_gents (userHome : Bool) :
    resolve .absolute true userHome = .fromRemembered := rfl

theorem no_remembered_file_means_dot_gents :
    resolve .absent true true = .dotGents := rfl

theorem an_unreadable_remembered_file_is_never_bypassed (userHome : Bool) :
    resolve .unreadable true userHome = .unreadable := rfl

theorem a_remembered_file_that_is_not_one_absolute_line_is_invalid (userHome : Bool) :
    resolve .empty true userHome = .invalid
      ∧ resolve .newlineOnly true userHome = .invalid
      ∧ resolve .relative true userHome = .invalid
      ∧ resolve .multiLine true userHome = .invalid
      ∧ resolve .carriageReturn true userHome = .invalid := ⟨rfl, rfl, rfl, rfl, rfl⟩

end HomeResolution
