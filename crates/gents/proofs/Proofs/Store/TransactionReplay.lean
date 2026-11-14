/-! What the transaction owner does with a failed attempt. An attempt fails at
begin, inside its callback, or at commit; the owner then reports the conflict,
runs the whole callback again on a fresh transaction, or fails. -/

namespace TransactionReplay

inductive Mode where
  | conflictRetry
  | signedConflictRebuild
  | singleAttempt
  | idempotent
  | idempotentFiveAttempts
  | observeConflict
  deriving DecidableEq, Repr

inductive Stage where
  | begin | callback | commit
  deriving DecidableEq, Repr

/-- `rejection` is every failure that is neither the store's conflict nor a
storage fault: a domain refusal, a denied write, an ownership violation. -/
inductive Failure where
  | conflict | storage | storageFull | rejection
  deriving DecidableEq, Repr

inductive Disposition where
  | observe | retry | fail
  deriving DecidableEq, Repr

def Mode.idempotent? : Mode → Bool
  | .idempotent | .idempotentFiveAttempts => true
  | _ => false

/-- The conflict bound is three repeats after the first attempt. A single
attempt replays a recorded pack transcript and an observing transaction
reports, so neither repeats. -/
def Mode.maxAttempts : Mode → Nat
  | .conflictRetry | .signedConflictRebuild | .idempotent => 4
  | .idempotentFiveAttempts => 5
  | .singleAttempt | .observeConflict => 1

/-- An idempotent transaction has stable identities and guards its updates on
durable state, so repeating it after a storage fault cannot apply it twice.
Once the callback has returned, whatever fails at commit is the store's, so a
commit failure is a storage fault whatever it carries. A full store is not
helped by another attempt. A conflict at begin is not an attempt's conflict:
nothing was read. -/
def repeatable (mode : Mode) (stage : Stage) (failure : Failure) : Bool :=
  match stage, failure with
  | _, .storageFull => false
  | .begin, .conflict => false
  | _, .conflict => true
  | .commit, _ => mode.idempotent?
  | _, .storage => mode.idempotent?
  | _, .rejection => false

/-- A write signed by a client and already sent may be signed again only when
the store confirmed that the attempt carrying it aborted on a conflict, and
only in the mode that authorizes rebuilding it. -/
def signedWriteMayRepeat (mode : Mode) (failure : Failure)
    (dispatched confirmedAbort : Bool) : Bool :=
  !dispatched || (confirmedAbort && failure == .conflict && mode == .signedConflictRebuild)

def disposition (mode : Mode) (stage : Stage) (failure : Failure)
    (dispatched confirmedAbort attemptsRemain : Bool) : Disposition :=
  if stage != .begin && failure == .conflict && mode == .observeConflict then .observe
  else if attemptsRemain && repeatable mode stage failure
      && signedWriteMayRepeat mode failure dispatched confirmedAbort then .retry
  else .fail

/-- A callback that asks to run again from the start is honoured only when its
attempt was discarded, no abandoned native operation still holds the
transaction, and a signed write it already sent may be signed again. -/
def rerunHonoured (mode : Mode) (discarded poisoned dispatched : Bool) : Bool :=
  (mode != .singleAttempt && mode != .observeConflict) && discarded && !poisoned
    && (!dispatched || mode == .signedConflictRebuild)

theorem repeat_needs_a_conflict_or_an_idempotent_transaction
    (mode : Mode) (stage : Stage) (failure : Failure) (d c r : Bool)
    (h : disposition mode stage failure d c r = .retry) :
    failure = .conflict ∨ mode.idempotent? = true := by
  cases mode <;> cases stage <;> cases failure <;> cases d <;> cases c <;> cases r <;>
    simp_all [disposition, repeatable, signedWriteMayRepeat, Mode.idempotent?]

theorem sent_signed_write_repeats_only_a_confirmed_conflict_rebuild
    (mode : Mode) (stage : Stage) (failure : Failure) (c r : Bool)
    (h : disposition mode stage failure true c r = .retry) :
    mode = .signedConflictRebuild ∧ failure = .conflict ∧ c = true := by
  cases mode <;> cases stage <;> cases failure <;> cases c <;> cases r <;>
    simp_all [disposition, repeatable, signedWriteMayRepeat, Mode.idempotent?]

theorem callback_rejection_is_terminal (mode : Mode) (d c r : Bool) :
    disposition mode .callback .rejection d c r = .fail := by
  cases mode <;> cases d <;> cases c <;> cases r <;> rfl

theorem full_store_is_terminal (mode : Mode) (stage : Stage) (d c r : Bool) :
    disposition mode stage .storageFull d c r = .fail := by
  cases mode <;> cases stage <;> cases d <;> cases c <;> cases r <;> rfl

theorem exhausted_attempts_never_repeat
    (mode : Mode) (stage : Stage) (failure : Failure) (d c : Bool) :
    disposition mode stage failure d c false ≠ .retry := by
  cases mode <;> cases stage <;> cases failure <;> cases d <;> cases c <;> decide

theorem observing_reports_and_never_repeats
    (stage : Stage) (failure : Failure) (d c r : Bool) :
    disposition .observeConflict stage failure d c r ≠ .retry := by
  cases stage <;> cases failure <;> cases d <;> cases c <;> cases r <;> decide

theorem only_an_observing_transaction_observes
    (mode : Mode) (stage : Stage) (failure : Failure) (d c r : Bool)
    (h : disposition mode stage failure d c r = .observe) :
    mode = .observeConflict ∧ failure = .conflict := by
  cases mode <;> cases stage <;> cases failure <;> cases d <;> cases c <;> cases r <;>
    simp_all [disposition, repeatable, signedWriteMayRepeat, Mode.idempotent?]

theorem begin_repeats_only_an_idempotent_storage_fault
    (mode : Mode) (failure : Failure) (d c r : Bool)
    (h : disposition mode .begin failure d c r = .retry) :
    failure = .storage ∧ mode.idempotent? = true := by
  cases mode <;> cases failure <;> cases d <;> cases c <;> cases r <;>
    simp_all [disposition, repeatable, signedWriteMayRepeat, Mode.idempotent?]

theorem rerun_needs_a_discarded_healthy_attempt
    (mode : Mode) (discarded poisoned dispatched : Bool)
    (h : rerunHonoured mode discarded poisoned dispatched = true) :
    discarded = true ∧ poisoned = false := by
  cases mode <;> cases discarded <;> cases poisoned <;> cases dispatched <;>
    simp_all [rerunHonoured]

/-- One attempt of a run: it commits, or it fails where and how the fault says.
`confirmedAbort` is the store's word that the attempt's writes did not land. -/
inductive Attempt where
  | commits
  | fails (stage : Stage) (failure : Failure) (confirmedAbort : Bool)
  deriving DecidableEq, Repr

inductive Result where
  | committed | conflictObserved | failed
  deriving DecidableEq, Repr

structure Run where
  result : Result
  begun : Nat
  callbacks : Nat
  /-- A commit failed without the store confirming that it did not land. -/
  unconfirmed : Bool
  /-- The failure a failed run reports: the one its last attempt met. -/
  failure : Option Failure
  deriving DecidableEq, Repr

/-- The owner's loop over a script of attempts; an attempt past the end of the
script commits. `dispatched` says the callback sends a client-signed write on
every attempt it runs. -/
def runFrom (mode : Mode) (dispatched : Bool) : Nat → List Attempt → Run → Run
  | 0, _, run => { run with result := .failed }
  | _ + 1, [], run =>
      { run with result := .committed, begun := run.begun + 1, callbacks := run.callbacks + 1 }
  | _ + 1, .commits :: _, run =>
      { run with result := .committed, begun := run.begun + 1, callbacks := run.callbacks + 1 }
  | left + 1, .fails stage failure confirmedAbort :: rest, run =>
      let ran := if stage == .begin then 0 else 1
      let run :=
        { run with
          begun := run.begun + ran
          callbacks := run.callbacks + ran
          unconfirmed := run.unconfirmed || (stage == .commit && !confirmedAbort) }
      match disposition mode stage failure (dispatched && stage != .begin) confirmedAbort
          (left != 0) with
      | .observe => { run with result := .conflictObserved }
      | .fail => { run with result := .failed, failure := some failure }
      | .retry => runFrom mode dispatched left rest run

def run (mode : Mode) (dispatched : Bool) (script : List Attempt) : Run :=
  runFrom mode dispatched mode.maxAttempts script
    { result := .failed, begun := 0, callbacks := 0, unconfirmed := false, failure := none }

theorem runFrom_bounds_callbacks (mode : Mode) (dispatched : Bool) :
    ∀ (left : Nat) (script : List Attempt) (start : Run),
      (runFrom mode dispatched left script start).callbacks ≤ start.callbacks + left := by
  intro left
  induction left with
  | zero => intro script start; simp [runFrom]
  | succ left ih =>
    intro script start
    cases script with
    | nil => simp [runFrom]
    | cons attempt rest =>
      cases attempt with
      | commits => simp [runFrom]
      | fails stage failure confirmedAbort =>
        simp only [runFrom]
        split
        · simp; split <;> omega
        · simp; split <;> omega
        · refine Nat.le_trans (ih rest _) ?_
          simp; split <;> omega

theorem callbacks_never_exceed_the_bound (mode : Mode) (dispatched : Bool)
    (script : List Attempt) :
    (run mode dispatched script).callbacks ≤ mode.maxAttempts := by
  simpa [run] using runFrom_bounds_callbacks mode dispatched mode.maxAttempts script
    { result := .failed, begun := 0, callbacks := 0, unconfirmed := false, failure := none }

theorem a_clean_attempt_commits_once (mode : Mode) (dispatched : Bool) :
    run mode dispatched [] =
      { result := .committed, begun := 1, callbacks := 1, unconfirmed := false, failure := none } := by
  cases mode <;> rfl

theorem a_sent_signed_write_runs_twice_only_after_a_confirmed_conflict
    (mode : Mode) (stage : Stage) (failure : Failure) (confirmedAbort : Bool)
    (rest : List Attempt) (ran : stage ≠ .begin)
    (h : 1 < (run mode true (.fails stage failure confirmedAbort :: rest)).callbacks) :
    mode = .signedConflictRebuild ∧ failure = .conflict ∧ confirmedAbort = true := by
  cases mode <;> cases stage <;> cases failure <;> cases confirmedAbort <;>
    simp_all [run, runFrom, Mode.maxAttempts, disposition, repeatable, signedWriteMayRepeat,
      Mode.idempotent?]

end TransactionReplay
