namespace ToolPolicy.PluginReceipt

/-- `gents_protocol::plugin::PluginExecutionVerdict`. `executionError` is a
host-side failure and may be retried; `failed` is the guest's own deterministic
failure (a non-zero exit or a trap). -/
inductive Verdict where
  | admissionRefused | executionError | success | refused | outOfFuel
  | outOfMemory | timeout | badOutput | failed
  deriving DecidableEq, Repr

def Verdict.spelling : Verdict → String
  | .admissionRefused => "admission_refused"
  | .executionError => "execution_error"
  | .success => "success"
  | .refused => "refused"
  | .outOfFuel => "out_of_fuel"
  | .outOfMemory => "out_of_memory"
  | .timeout => "timeout"
  | .badOutput => "bad_output"
  | .failed => "failed"

/-- `evidence` stands for the host-authored fields other than output and
verdict (coordinate, artifact and input digests, authority, limits); the native
receipt compares them as a whole. -/
structure Receipt where
  evidence : String
  output : Option String
  verdict : Verdict
  deriving DecidableEq, Repr

inductive Terminal where
  | completed | failed | timedOut | cancelled
  deriving DecidableEq, Repr

def Terminal.spelling : Terminal → String
  | .completed => "completed"
  | .failed => "failed"
  | .timedOut => "timedOut"
  | .cancelled => "cancelled"

/-- A receipt names an output only when its terminal published that output. A
guest that returned after the outer deadline reports `timeout`; one whose
result the host otherwise did not publish reports `executionError`. -/
def published (terminal : Terminal) (receipt : Receipt) : Receipt :=
  match terminal with
  | .completed => receipt
  | _ => { receipt with
      output := none
      verdict := if receipt.verdict = .success then
        (if terminal = .timedOut then .timeout else .executionError)
        else receipt.verdict }

theorem published_output_requires_completion (terminal : Terminal) (receipt : Receipt)
    (h : (published terminal receipt).output.isSome = true) : terminal = .completed := by
  cases terminal <;> simp_all [published]

theorem published_success_requires_completion (terminal : Terminal) (receipt : Receipt)
    (h : (published terminal receipt).verdict = .success) : terminal = .completed := by
  cases terminal <;> simp only [published] at h
  · rfl
  all_goals (by_cases hs : receipt.verdict = .success <;> simp_all)

inductive Decision where
  | commit | adopt | replay | conflict
  deriving DecidableEq, Repr

def Decision.spelling : Decision → String
  | .commit => "commit"
  | .adopt => "adopt"
  | .replay => "replay"
  | .conflict => "conflict"

/-- The terminal compare-and-set is the only receipt writer, so a durable
receipt implies a terminal row. A competing terminal that committed without a
receipt keeps the row as it is; a retry of the same terminal presents identical
evidence. Request generation is fenced at admission, not here: recovery closes
rows of a replaced generation through the same terminal path as any tool. -/
def decide (terminal : Bool) (existing : Option Receipt) (receipt : Receipt) : Decision :=
  match terminal, existing with
  | false, none => .commit
  | false, some _ => .conflict
  | true, none => .adopt
  | true, some prior => if prior = receipt then .replay else .conflict

structure Row where
  terminal : Bool
  receipt : Option Receipt
  deriving DecidableEq, Repr

def Row.wellFormed (row : Row) : Prop := row.receipt.isSome = true → row.terminal = true

/-- One terminal attempt carrying `receipt` against the durable row. -/
def step (row : Row) (terminal : Terminal) (receipt : Receipt) : Row :=
  match decide row.terminal row.receipt (published terminal receipt) with
  | .commit => ⟨true, some (published terminal receipt)⟩
  | _ => row

theorem step_preserves_wellFormed (row : Row) (terminal : Terminal) (receipt : Receipt)
    (h : row.wellFormed) : (step row terminal receipt).wellFormed := by
  unfold step
  split
  · intro _; rfl
  · exact h

theorem terminal_row_is_final (row : Row) (terminal : Terminal) (receipt : Receipt)
    (h : row.terminal = true) : step row terminal receipt = row := by
  obtain ⟨t, existing⟩ := row
  subst h
  cases existing with
  | none => rfl
  | some prior =>
    simp only [step, decide]
    by_cases hp : prior = published terminal receipt <;> simp [hp]

theorem running_row_commits_published (terminal : Terminal) (receipt : Receipt) :
    step ⟨false, none⟩ terminal receipt = ⟨true, some (published terminal receipt)⟩ := rfl

theorem terminal_absence_never_backfilled (receipt : Receipt) :
    decide true none receipt = .adopt := rfl

theorem replay_requires_identical_evidence (prior receipt : Receipt)
    (h : decide true (some prior) receipt = .replay) : prior = receipt := by
  simp only [decide] at h
  split at h <;> simp_all

end ToolPolicy.PluginReceipt
