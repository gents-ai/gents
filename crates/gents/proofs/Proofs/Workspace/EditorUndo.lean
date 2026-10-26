import Proofs.Workspace.Properties

namespace Workspace.EditorUndo

/-- These are editor observations, not source filesystem grants. The host resolves
retained actions through the existing authenticated native reader, resolves user
workspace mappings, and reads actual open documents. No webview-supplied bytes or
authorization flags may populate this boundary. Saving remains owned by VS Code. -/
structure Target where
  reference : String
  actions : List String
  uri : String
  before : String
  after : String
  version : Nat
  encoding : String := "utf8"
  eol : String := "lf"
  deriving DecidableEq, Repr

structure Document where
  uri : String
  content : String
  version : Nat
  encoding : String := "utf8"
  eol : String := "lf"
  dirty : Bool := false
  closed : Bool := false
  writable : Bool := false
  deriving DecidableEq, Repr

structure Observations where
  trusted : Bool
  selectionCurrent : Bool
  sourcesVerified : Bool
  mappingsCurrent : Bool
  preservesConflicts : Bool
  deriving DecidableEq, Repr

def textValid (text eol : String) : Bool :=
  !text.contains '\u0000' && !text.startsWith "\uFEFF" &&
  if eol == "lf" then !text.contains '\r'
  else if eol == "crlf" then
    let rest := text.replace "\r\n" ""
    !rest.contains '\r' && !rest.contains '\n'
  else false

/-- The native reader authenticates each physical action before the host groups
its exact root/path. Composition requires every intermediate postimage to equal
the next preimage; request text or a reported diff cannot fill a missing link. -/
def composeTextEdits : List (String × String) → Option (String × String)
  | [] => none
  | [edit] => if edit.1 == edit.2 then none else some edit
  | edit :: rest => do
      let tail ← composeTextEdits rest
      if edit.1 != edit.2 && edit.2 == tail.1 then some (edit.1, tail.2) else none

def targetValid (target : Target) : Bool :=
  !target.reference.isEmpty && !target.actions.isEmpty && target.actions.all (!·.isEmpty) &&
  !target.uri.isEmpty && target.version > 0 &&
  target.before != target.after && target.encoding == "utf8" &&
  textValid target.before target.eol && textValid target.after target.eol

def matchesDocument (target : Target) (document : Document) : Bool :=
  target.uri == document.uri && target.after == document.content &&
  target.version == document.version && target.encoding == document.encoding &&
  target.eol == document.eol && !document.dirty && !document.closed && document.writable

def eligible (targets : List Target) (documents : List Document)
    (observations : Observations) : Bool :=
  observations.trusted && observations.selectionCurrent && observations.sourcesVerified &&
  observations.mappingsCurrent && observations.preservesConflicts && !targets.isEmpty &&
  decide (targets.flatMap Target.actions).Nodup &&
  decide (targets.map Target.uri).Nodup && decide (documents.map Document.uri).Nodup &&
  targets.all fun target => targetValid target &&
    documents.any (matchesDocument target)

inductive State where
  | fresh | pending | applied | rejected | unknown
  deriving DecidableEq, Repr

inductive Completion where
  | applied | rejected | unknown
  deriving DecidableEq, Repr

/-- The workspace-local marker owns only this explicit editor action. Persisting
pending precedes application; an interrupted marker is never retried. The marker
contains exact references and URI identity, never retained bytes or source grants.
Each host serializes its admissions; this marker is not a cross-window lock.
Concurrent document changes remain fenced by the actual editor version. -/
def begin (state : State) (allowed persisted : Bool) : State × Bool :=
  match state with
  | .pending => (.unknown, false)
  | .applied => (.applied, false)
  | .unknown => (.unknown, false)
  | .fresh | .rejected =>
      if allowed && persisted then (.pending, true) else (state, false)

def complete (state : State) (completion : Completion) (observed persisted : Bool) : State :=
  if state != .pending || !persisted then state
  else match completion with
  | .rejected => .rejected
  | .applied => if observed then .applied else .unknown
  | .unknown => .unknown

theorem completed_replay_is_inert (allowed persisted : Bool) :
    begin .applied allowed persisted = (.applied, false) := by rfl

theorem unknown_never_retries (allowed persisted : Bool) :
    begin .unknown allowed persisted = (.unknown, false) := by rfl

theorem interrupted_never_retries (allowed persisted : Bool) :
    begin .pending allowed persisted = (.unknown, false) := by rfl

theorem failed_pending_write_has_no_effect (allowed : Bool) :
    (begin .fresh allowed false).2 = false := by simp [begin]

theorem failed_result_write_retains_pending (completion : Completion) (observed : Bool) :
    complete .pending completion observed false = .pending := by simp [complete]

inductive SaveState where
  | pending | saving | saved | conflict | failed | unknown
  deriving DecidableEq, Repr

structure SaveMarker where
  state : SaveState
  exact : Bool := false
  deriving DecidableEq, Repr

/-- Saving is a distinct per-document effect owned by VS Code, never an atomic
multi-file transaction. The host must refuse an effective overwrite-on-conflict
policy, preserve current buffers, and observe actual save events/results. A save
participant may change text: saved does not imply the recorded preimage was
persisted. False or unknown guard evidence cannot authorize a save. -/
structure SaveObservations where
  preservesConflicts : Bool
  documentCurrent : Bool
  diskPostimageCurrent : Bool
  bufferRecoverable : Bool
  deriving DecidableEq, Repr

def beginSave (marker : SaveMarker) (observations : SaveObservations)
    (persisted : Bool) : SaveMarker × Bool :=
  if marker.state == .saving then (⟨.unknown, false⟩, false)
  else if marker.state != .pending then (marker, false)
  else if !observations.preservesConflicts then (⟨.failed, false⟩, false)
  else if !observations.documentCurrent || !observations.diskPostimageCurrent then
    (⟨.conflict, false⟩, false)
  else if !observations.bufferRecoverable then (⟨.unknown, false⟩, false)
  else if persisted then (⟨.saving, false⟩, true)
  else (marker, false)

def finishSave (marker result : SaveMarker) (persisted : Bool) : SaveMarker :=
  if marker.state != .saving || !persisted then marker
  else if result.state == .saved then result
  else if result.state == .conflict || result.state == .failed then {result with exact := false}
  else ⟨.unknown, false⟩

def reopenSave (marker : SaveMarker) : SaveMarker :=
  if marker.state == .saving || marker.state == .pending then ⟨.unknown, false⟩ else marker

theorem saved_never_resaves (observations : SaveObservations) (persisted exact : Bool) :
    beginSave ⟨.saved, exact⟩ observations persisted = (⟨.saved, exact⟩, false) := by
  simp [beginSave]

theorem save_without_pending_write_is_inert (observations : SaveObservations) :
    (beginSave ⟨.pending, false⟩ observations false).2 = false := by
  cases observations with
  | mk policy current disk recoverable =>
    cases policy <;> cases current <;> cases disk <;> cases recoverable <;> rfl

theorem lost_save_receipt_preserves_saving (result : SaveMarker) :
    finishSave ⟨.saving, false⟩ result false = ⟨.saving, false⟩ := by
  simp [finishSave]

theorem interrupted_save_is_unknown : reopenSave ⟨.saving, false⟩ = ⟨.unknown, false⟩ := by
  rfl

structure Progress where
  reference : String
  actions : List String
  uri : String
  restore : State := .fresh
  save : SaveMarker := ⟨.pending, false⟩
  deriving DecidableEq, Repr

def Progress.done (progress : Progress) : Bool :=
  progress.restore == .applied && progress.save.state == .saved

structure Sequence where
  items : List Progress
  closed : Bool := false
  deriving DecidableEq, Repr

def admitSequence (targets : List Target) (documents : List Document)
    (observations : Observations) (persisted : Bool) : Option Sequence :=
  if eligible targets documents observations && persisted then
    some ⟨targets.map (fun target =>
      ⟨target.reference, target.actions, target.uri, .fresh, ⟨.pending, false⟩⟩), false⟩
  else none

inductive Effect where
  | none | restore | save
  deriving DecidableEq, Repr

inductive Action where
  | startRestore (index : Nat) (allowed persisted : Bool)
  | observeRestore (index : Nat) (completion : Completion) (observed persisted : Bool)
  | startSave (index : Nat) (observations : SaveObservations) (persisted : Bool)
  | observeSave (index : Nat) (result : SaveMarker) (persisted : Bool)
  | observeSavedByEditor (index : Nat) (result : SaveMarker) (persisted : Bool)
  deriving DecidableEq, Repr

def Action.index : Action → Nat
  | .startRestore index _ _ | .observeRestore index _ _ _ |
    .startSave index _ _ | .observeSave index _ _ | .observeSavedByEditor index _ _ => index

def replaceProgress (sequence : Sequence) (index : Nat) (progress : Progress)
    (close : Bool) : Sequence :=
  let items := sequence.items.set index progress
  ⟨items, close || items.all Progress.done⟩

/-- The initial all-target preflight precedes this ordered operation. Each target
uses the actual TextEditor.edit owner, with a synchronous final clean/text/version
check; its boolean alone is not an acknowledgement. UI locks are enforced by that
editor, never inferred from filesystem writability. Editor document observations
confirm application before a normal save is admitted. Restore and save are not
atomic across targets: the first changed, refused or unknown target closes the
sequence, preserving every prior outcome and all remaining targets. -/
def step (sequence : Sequence) (action : Action) : Sequence × Effect :=
  if sequence.closed then (sequence, .none)
  else if !(sequence.items.take action.index).all Progress.done then (sequence, .none)
  else match sequence.items[action.index]? with
  | none => (sequence, .none)
  | some progress =>
    if progress.done then (sequence, .none)
    else match action with
    | .startRestore index allowed persisted =>
      if progress.restore != .fresh then (sequence, .none)
      else
        let result := begin progress.restore allowed persisted
        (replaceProgress sequence index {progress with restore := result.1} (!result.2),
          if result.2 then .restore else .none)
    | .observeRestore index completion observed persisted =>
      if progress.restore != .pending then (sequence, .none)
      else
        let restore := complete progress.restore completion observed persisted
        (replaceProgress sequence index {progress with restore} (restore != .applied), .none)
    | .startSave index observations persisted =>
      if progress.restore != .applied || progress.save.state != .pending then (sequence, .none)
      else
        let result := beginSave progress.save observations persisted
        (replaceProgress sequence index {progress with save := result.1} (!result.2),
          if result.2 then .save else .none)
    | .observeSave index result persisted =>
      if progress.restore != .applied || progress.save.state != .saving then (sequence, .none)
      else
        let save := finishSave progress.save result persisted
        (replaceProgress sequence index {progress with save} (save.state != .saved), .none)
    | .observeSavedByEditor index result persisted =>
      if progress.restore != .applied || progress.save.state != .pending || result.state != .saved then
        (sequence, .none)
      else
        let save := if persisted then result else progress.save
        (replaceProgress sequence index {progress with save} (save.state != .saved), .none)

def reopen (sequence : Sequence) : Sequence :=
  { items := sequence.items.map fun progress =>
      { progress with
        restore := if progress.restore == .pending then .unknown else progress.restore
        save := if progress.restore == .applied then reopenSave progress.save else progress.save }
    closed := true }

theorem closed_sequence_has_no_effect (items : List Progress) (action : Action) :
    step ⟨items, true⟩ action = (⟨items, true⟩, .none) := by simp [step]

theorem reopened_sequence_has_no_effect (sequence : Sequence) (action : Action) :
    step (reopen sequence) action = (reopen sequence, .none) := by simp [step, reopen]

theorem failed_initial_marker_refuses_operation (targets : List Target) (documents : List Document)
    (observations : Observations) : admitSequence targets documents observations false = none := by
  simp [admitSequence]

structure Operation where
  id : String
  sequence : Sequence
  deriving DecidableEq, Repr

abbrev Ledger := List Operation

def referencesValid (sequence : Sequence) : Bool :=
  !sequence.items.isEmpty && decide (sequence.items.map Progress.uri).Nodup &&
  decide (sequence.items.flatMap Progress.actions).Nodup &&
  sequence.items.all fun progress => !progress.reference.isEmpty && !progress.uri.isEmpty &&
    !progress.actions.isEmpty && progress.actions.all (!·.isEmpty) &&
    (progress.save.state == .saved || !progress.save.exact) &&
    (progress.restore == .applied || (progress.save.state == .pending && !progress.save.exact))

def ledgerValid (ledger : Ledger) : Bool :=
  decide (ledger.map Operation.id).Nodup &&
  ledger.all fun operation => !operation.id.isEmpty && referencesValid operation.sequence

/-- Action identities come from the verified native physical request/tool and
recorded source root/path, never a view path, image hash, URI or selected group.
One host serializes this ref-only ledger and persists the entire admission before
any editor effect. This is not a cross-window storage transaction or a source
filesystem grant. Historical rows remain intact: only a closed, definitively
untouched or rejected target releases its actions for a new explicit attempt.
Pending, applied and unknown targets remain reserved after reload, even when a
later buffer happens to match an old postimage or the user selects another group. -/
def reserved (ledger : Ledger) (action : String) : Bool :=
  ledger.any fun operation => operation.sequence.items.any fun progress =>
    progress.actions.contains action &&
    (!operation.sequence.closed || (progress.restore != .fresh && progress.restore != .rejected))

def reserve (ledger : Ledger) (id : String) (targets : List Target)
    (documents : List Document) (observations : Observations) (persisted : Bool) : Option Ledger :=
  if !ledgerValid ledger || id.isEmpty || ledger.any (·.id == id) ||
    (targets.flatMap Target.actions).any (reserved ledger) then none
  else do
    let sequence ← admitSequence targets documents observations persisted
    some (ledger ++ [⟨id, sequence⟩])

theorem failed_reservation_write_has_no_admission (ledger : Ledger) (id : String)
    (targets : List Target) (documents : List Document) (observations : Observations) :
    reserve ledger id targets documents observations false = none := by
  simp [reserve, admitSequence]

theorem reserved_action_refuses_regrouping (ledger : Ledger) (id : String)
    (targets : List Target) (documents : List Document) (observations : Observations)
    (persisted : Bool) (overlap : (targets.flatMap Target.actions).any (reserved ledger) = true) :
    reserve ledger id targets documents observations persisted = none := by
  simp [reserve, overlap]

end Workspace.EditorUndo
