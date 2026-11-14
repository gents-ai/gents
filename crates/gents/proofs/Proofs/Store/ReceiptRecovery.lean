/-! A stable-id write over HTTP whose reply may be lost. The owner posts the
mutation, and after an ambiguous reply asks the caller's receipt whether the
row is durable before it may post again. -/

namespace ReceiptRecovery

/-- What one post of the mutation meets. `lostReply` lands and answers
ambiguously; `conflict` does not land and answers the same way. External
premise: the owner cannot tell the two apart from the reply. -/
inductive Post where
  | commits | lostReply | conflict | rejected
  deriving DecidableEq, Repr

/-- What one read of the receipt meets: an answer, a failure worth reading
again, or a failure that is not. -/
inductive Read where
  | answers | transient | fails
  deriving DecidableEq, Repr

inductive Outcome where
  | written | writeFailed | receiptFailed
  deriving DecidableEq, Repr

structure Run where
  outcome : Outcome
  posts : Nat
  reads : Nat
  durable : Bool
  /-- Receipt reads that answered that the row is absent. -/
  absences : Nat
  deriving DecidableEq, Repr

def maxPosts : Nat := 5

def maxReads : Nat := 5

/-- One receipt check: reads until an answer, a failure, or the bound. An
answer is the truth about the durable row. -/
def readReceipt (durable : Bool) : Nat → List Read → Nat → Option Bool × List Read × Nat
  | 0, script, reads => (none, script, reads)
  | _ + 1, [], reads => (some durable, [], reads + 1)
  | _ + 1, .answers :: rest, reads => (some durable, rest, reads + 1)
  | _ + 1, .fails :: rest, reads => (none, rest, reads + 1)
  | fuel + 1, .transient :: rest, reads => readReceipt durable fuel rest (reads + 1)

def runFrom : Nat → List Post → List Read → Run → Run
  | 0, _, _, run => { run with outcome := .writeFailed }
  | left + 1, posts, reads, run =>
      let post := posts.head?.getD .commits
      let run := { run with posts := run.posts + 1 }
      match post with
      | .commits => { run with outcome := .written, durable := true }
      | .rejected => { run with outcome := .writeFailed }
      | .lostReply | .conflict =>
          let durable := run.durable || post == .lostReply
          match readReceipt durable maxReads reads run.reads with
          | (none, _, read) => { run with durable, reads := read, outcome := .receiptFailed }
          | (some true, _, read) => { run with durable, reads := read, outcome := .written }
          | (some false, rest, read) =>
              let run := { run with durable, reads := read, absences := run.absences + 1 }
              if left = 0 then { run with outcome := .writeFailed }
              else runFrom left posts.tail rest run

def run (posts : List Post) (reads : List Read) : Run :=
  runFrom maxPosts posts reads
    { outcome := .writeFailed, posts := 0, reads := 0, durable := false, absences := 0 }

theorem readReceipt_answers_the_durable_row (durable : Bool) :
    ∀ (fuel : Nat) (script : List Read) (reads : Nat) (present : Bool) (rest : List Read)
      (read : Nat),
      readReceipt durable fuel script reads = (some present, rest, read) → present = durable := by
  intro fuel
  induction fuel with
  | zero => intro script reads present rest read h; simp [readReceipt] at h
  | succ fuel ih =>
    intro script reads present rest read h
    cases script with
    | nil => simp [readReceipt] at h; exact h.1.symm
    | cons head tail =>
      cases head with
      | answers => simp [readReceipt] at h; exact h.1.symm
      | fails => simp [readReceipt] at h
      | transient => exact ih tail (reads + 1) present rest read (by simpa [readReceipt] using h)

/-- A write reported as written is durable: it committed, or its receipt
answered that the row is there. -/
theorem runFrom_written_is_durable :
    ∀ (left : Nat) (posts : List Post) (reads : List Read) (start : Run),
      (runFrom left posts reads start).outcome = .written →
      (runFrom left posts reads start).durable = true := by
  intro left
  induction left with
  | zero => intro posts reads start h; simp [runFrom] at h
  | succ left ih =>
    intro posts reads start
    simp only [runFrom]
    split
    · simp
    · simp
    all_goals
      split
      · simp
      · next answered =>
          intro _
          exact (readReceipt_answers_the_durable_row _ _ _ _ _ _ _ answered).symm
      · split
        · simp
        · exact ih _ _ _

theorem written_is_durable (posts : List Post) (reads : List Read)
    (h : (run posts reads).outcome = .written) : (run posts reads).durable = true :=
  runFrom_written_is_durable _ _ _ _ h

/-- The mutation is posted again only after a receipt read answered that the
row is absent. -/
theorem runFrom_reposts_follow_absences :
    ∀ (left : Nat) (posts : List Post) (reads : List Read) (start : Run),
      start.posts ≤ start.absences →
      (runFrom left posts reads start).posts ≤ (runFrom left posts reads start).absences + 1 := by
  intro left
  induction left with
  | zero => intro posts reads start h; simp [runFrom]; omega
  | succ left ih =>
    intro posts reads start h
    simp only [runFrom]
    split
    · simp; omega
    · simp; omega
    all_goals
      split
      · simp; omega
      · simp; omega
      · split
        · simp; omega
        · exact ih _ _ _ (by simp; omega)

theorem reposts_follow_absences (posts : List Post) (reads : List Read) :
    (run posts reads).posts ≤ (run posts reads).absences + 1 :=
  runFrom_reposts_follow_absences _ _ _ _ (Nat.le_refl 0)

theorem runFrom_bounds_posts :
    ∀ (left : Nat) (posts : List Post) (reads : List Read) (start : Run),
      (runFrom left posts reads start).posts ≤ start.posts + left := by
  intro left
  induction left with
  | zero => intro posts reads start; simp [runFrom]
  | succ left ih =>
    intro posts reads start
    simp only [runFrom]
    split
    · simp <;> omega
    · simp <;> omega
    all_goals
      split
      · simp <;> omega
      · simp <;> omega
      · split
        · simp <;> omega
        · refine Nat.le_trans (ih _ _ _) ?_
          simp; omega

theorem posts_never_exceed_the_bound (posts : List Post) (reads : List Read) :
    (run posts reads).posts ≤ maxPosts := by
  simpa [run] using runFrom_bounds_posts maxPosts posts reads
    { outcome := .writeFailed, posts := 0, reads := 0, durable := false, absences := 0 }

end ReceiptRecovery
