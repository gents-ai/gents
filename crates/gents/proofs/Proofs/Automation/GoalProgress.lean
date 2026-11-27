import Proofs.Automation.Goals
import Lean.Data.Json

namespace GoalProgress

/-- One tool call of a goal turn as the harness observed it: the tool's name,
its arguments in canonical JSON (keys sorted, no insignificant whitespace) and
a digest of its result. The Agent's text is not part of it, so rewording what
the Agent says never changes whether a turn made progress. -/
structure CallPrint where
  tool : String
  arguments : String
  result : String
  deriving DecidableEq, Repr

/-- What the harness observed of one finished goal turn: the prints of its
tool calls, and whether the turn changed the Goal's state (an `update_goal`
the Goal owner applied). -/
structure Turn where
  calls : List CallPrint
  goalStateChanged : Bool
  deriving DecidableEq, Repr

/-- Consecutive goal turns without progress after which the harness pauses a
Goal. Premise: three empty automatic turns is the shipped threshold of the
nearest comparable runtime, and repeated identical actions predict failure
rather than eventual success, so a fourth identical turn mostly buys cost. -/
def noProgressLimit : Nat := 3

/-- A turn makes progress iff it changed the Goal's state or made a tool call
whose print was not among the previous goal turn's prints. A turn with no
tool calls, or only calls identical in name, arguments and result to the
previous turn's, makes none. -/
def madeProgress (previous : List CallPrint) (turn : Turn) : Bool :=
  turn.goalStateChanged || turn.calls.any (fun call => !previous.contains call)

/-- How the harness counts a finished goal turn. A Goal waiting on its own
live background work (a running tool call whose deadline has not passed) is
in a verified wait; once that deadline passes, the turn counts as any other. -/
inductive Observation where
  | progress
  | verifiedWait
  | noProgress
  deriving DecidableEq, Repr

def observe (previous : List CallPrint) (turn : Turn) (liveDeadline : Option Nat)
    (now : Nat) : Observation :=
  if madeProgress previous turn then .progress
  else if liveDeadline.any (now < ·) then .verifiedWait
  else .noProgress

/-- The no-progress record the Goal owner keeps on the Goal when it decides a
continuation: the streak of turns without progress and the prints of the
latest turn. -/
structure Record where
  streak : Nat
  previous : List CallPrint
  deriving DecidableEq, Repr

def Record.fresh : Record := ⟨0, []⟩

/-- The owner's step over one finished goal turn: the record it writes and
whether the Goal pauses. A verified wait neither extends nor clears the
streak. -/
def step (record : Record) (turn : Turn) (liveDeadline : Option Nat) (now : Nat) :
    Record × Bool :=
  match observe record.previous turn liveDeadline now with
  | .progress => (⟨0, turn.calls⟩, false)
  | .verifiedWait => (⟨record.streak, turn.calls⟩, false)
  | .noProgress =>
      let streak := record.streak + 1
      (⟨streak, turn.calls⟩, noProgressLimit ≤ streak)

theorem a_repeated_turn_makes_no_progress (calls : List CallPrint) :
    madeProgress calls ⟨calls, false⟩ = false := by
  simp only [madeProgress, Bool.false_or, List.any_eq_false, Bool.not_eq_eq_eq_not,
    Bool.not_true]
  intro call member
  simpa using member

theorem a_turn_without_calls_makes_no_progress (previous : List CallPrint) :
    madeProgress previous ⟨[], false⟩ = false := rfl

theorem a_new_call_is_progress (previous : List CallPrint) (call : CallPrint)
    (rest : List CallPrint) (changed : Bool) (fresh : previous.contains call = false) :
    madeProgress previous ⟨call :: rest, changed⟩ = true := by
  have absent : call ∉ previous := by simpa using fresh
  simp [madeProgress, absent]

theorem a_goal_state_change_is_progress (previous calls : List CallPrint) :
    madeProgress previous ⟨calls, true⟩ = true := rfl

/-- A wait is verified only before its job's deadline: from the deadline on,
the turn is observed exactly as if no job were live. -/
theorem an_expired_wait_is_not_a_wait (previous : List CallPrint) (turn : Turn)
    (deadline now : Nat) (expired : deadline ≤ now) :
    observe previous turn (some deadline) now = observe previous turn none now := by
  have : ¬ now < deadline := Nat.not_lt.mpr expired
  simp [observe, this]

/-- Turns without progress after every live deadline has passed pause the
Goal on the `noProgressLimit`-th of them, from a fresh record. -/
def run (record : Record) : List (Turn × Option Nat × Nat) → Record × Bool
  | [] => (record, false)
  | (turn, deadline, now) :: rest =>
      let (next, pause) := step record turn deadline now
      if pause then (next, true) else run next rest

theorem the_same_turn_repeated_pauses (call : CallPrint) (now : Nat) :
    (run Record.fresh
      [(⟨[call], false⟩, none, now), (⟨[call], false⟩, none, now),
       (⟨[call], false⟩, none, now), (⟨[call], false⟩, none, now)]).2 = true := by
  simp [run, step, observe, madeProgress, Record.fresh, noProgressLimit]

/-- An Agent cannot idle forever behind a wait: once now is past its job's
deadline, `noProgressLimit` turns that repeat the previous turn pause the
Goal, whatever streak the record holds. -/
theorem waiting_past_the_deadline_pauses (record : Record) (deadline now : Nat)
    (expired : deadline ≤ now) :
    let turn : Turn := ⟨record.previous, false⟩
    (run record (List.replicate noProgressLimit (turn, some deadline, now))).2 = true := by
  have hn : ¬ now < deadline := Nat.not_lt.mpr expired
  have hp : ∀ calls : List CallPrint, madeProgress calls ⟨calls, false⟩ = false :=
    a_repeated_turn_makes_no_progress
  rcases record with ⟨streak, previous⟩
  simp only [noProgressLimit, List.replicate, run, step, observe, hp, hn, Option.any_some,
    decide_false, Bool.false_eq_true, if_false]
  by_cases h1 : 3 ≤ streak + 1
  · simp [h1]
  · by_cases h2 : 3 ≤ streak + 1 + 1
    · simp [h1, h2]
    · have h3 : 3 ≤ streak + 1 + 1 + 1 := by omega
      simp [h1, h2, h3]

/-- JSON object keys whose values vary only with the clock at read time
(process/wait polls, live goal timers, P2P retry countdowns, command wall
time, filesystem walk time). Stripping them before hashing a tool call's
result lets no-progress detection recognize repeated identical polls without
masking real state changes. -/
def volatileResultFields : List String :=
  [ "read_at"
  , "elapsed_seconds"
  , "active_time_seconds"
  , "next_pending_retry_in_ms"
  , "lease_remaining_days"
  , "duration_ms"
  , "elapsed_ms"
  ]

/-- Command envelope (`gents_exec:`) metadata keys whose values vary with
command wall time (`hint` formats `duration_ms` on timeout). Outside a
command envelope, `hint` remains a progress-bearing field. -/
def commandEnvelopeVolatileFields : List String :=
  [ "hint" ]

def isVolatileResultField (key : String) : Bool :=
  volatileResultFields.contains key

/-- Whether `key` in a JSON object varies only by read-time clock;
`commandEnvelope` enables command-envelope volatile metadata such as `hint`. -/
def isVolatileFieldFor (commandEnvelope : Bool) (key : String) : Bool :=
  isVolatileResultField key || (commandEnvelope && commandEnvelopeVolatileFields.contains key)

theorem read_at_is_volatile : isVolatileResultField "read_at" = true := rfl
theorem elapsed_seconds_is_volatile : isVolatileResultField "elapsed_seconds" = true := rfl
theorem active_time_seconds_is_volatile : isVolatileResultField "active_time_seconds" = true := rfl
theorem next_pending_retry_in_ms_is_volatile : isVolatileResultField "next_pending_retry_in_ms" = true := rfl
theorem lease_remaining_days_is_volatile : isVolatileResultField "lease_remaining_days" = true := rfl
theorem duration_ms_is_volatile : isVolatileResultField "duration_ms" = true := rfl
theorem elapsed_ms_is_volatile : isVolatileResultField "elapsed_ms" = true := rfl
theorem status_is_not_volatile : isVolatileResultField "status" = false := rfl
theorem result_is_not_volatile : isVolatileResultField "result" = false := rfl
theorem hint_is_not_globally_volatile : isVolatileResultField "hint" = false := rfl
theorem hint_is_volatile_in_command_envelope : isVolatileFieldFor true "hint" = true := rfl
theorem hint_is_not_volatile_outside_command_envelope : isVolatileFieldFor false "hint" = false := rfl

/-- Total structured value model for proving canonicalization and no-progress
invariance over arbitrarily nested tool outputs. -/
inductive ResultValue where
  | atom (value : String)
  | array (items : List ResultValue)
  | object (fields : List (String × ResultValue))
  deriving Repr

inductive ResultFormat where
  | json
  | execEnvelope
  | fsEnvelope
  deriving DecidableEq, Repr

def ResultFormat.isCommandEnvelope : ResultFormat → Bool
  | .execEnvelope => true
  | .json | .fsEnvelope => false

/-- Structured observation of a tool result: either raw non-JSON text or a
parsed JSON / metadata-envelope result. -/
inductive ResultObservation where
  | rawText (text : String)
  | structured (format : ResultFormat) (meta : ResultValue) (body : String)
  deriving Repr

def insertField (entry : String × ResultValue) :
    List (String × ResultValue) → List (String × ResultValue)
  | [] => [entry]
  | head :: tail =>
      if entry.1 ≤ head.1 then entry :: head :: tail
      else head :: insertField entry tail

def sortFields : List (String × ResultValue) → List (String × ResultValue)
  | [] => []
  | entry :: rest => insertField entry (sortFields rest)

mutual
  def stripValue (commandEnvelope : Bool) : ResultValue → ResultValue
    | .atom v => .atom v
    | .array items => .array (stripArray commandEnvelope items)
    | .object fields => .object (sortFields (stripFields commandEnvelope fields))

  def stripArray (commandEnvelope : Bool) : List ResultValue → List ResultValue
    | [] => []
    | item :: rest => stripValue commandEnvelope item :: stripArray commandEnvelope rest

  def stripFields (commandEnvelope : Bool) :
      List (String × ResultValue) → List (String × ResultValue)
    | [] => []
    | (k, v) :: rest =>
        if isVolatileFieldFor commandEnvelope k then
          stripFields commandEnvelope rest
        else
          (k, stripValue commandEnvelope v) :: stripFields commandEnvelope rest
end

def canonicalObservation : ResultObservation → ResultObservation
  | .rawText text => .rawText text
  | .structured format meta body =>
      .structured format (stripValue format.isCommandEnvelope meta) body

def observationCallPrintWith (digest : ResultObservation → String)
    (tool arguments : String) (obs : ResultObservation) : CallPrint :=
  ⟨tool, arguments, digest (canonicalObservation obs)⟩

mutual
  inductive OnlyVolatileValueDiff (cmd : Bool) : ResultValue → ResultValue → Prop where
    | atom (s : String) : OnlyVolatileValueDiff cmd (.atom s) (.atom s)
    | array {xs ys : List ResultValue} :
        OnlyVolatileArrayDiff cmd xs ys →
        OnlyVolatileValueDiff cmd (.array xs) (.array ys)
    | object {xs ys : List (String × ResultValue)} :
        OnlyVolatileFieldsDiff cmd xs ys →
        OnlyVolatileValueDiff cmd (.object xs) (.object ys)

  inductive OnlyVolatileArrayDiff (cmd : Bool) :
      List ResultValue → List ResultValue → Prop where
    | nil : OnlyVolatileArrayDiff cmd [] []
    | cons {x y : ResultValue} {xs ys : List ResultValue} :
        OnlyVolatileValueDiff cmd x y →
        OnlyVolatileArrayDiff cmd xs ys →
        OnlyVolatileArrayDiff cmd (x :: xs) (y :: ys)

  inductive OnlyVolatileFieldsDiff (cmd : Bool) :
      List (String × ResultValue) → List (String × ResultValue) → Prop where
    | nil : OnlyVolatileFieldsDiff cmd [] []
    | skipLeft {k : String} {v : ResultValue} {xs ys : List (String × ResultValue)} :
        isVolatileFieldFor cmd k = true →
        OnlyVolatileFieldsDiff cmd xs ys →
        OnlyVolatileFieldsDiff cmd ((k, v) :: xs) ys
    | skipRight {k : String} {v : ResultValue} {xs ys : List (String × ResultValue)} :
        isVolatileFieldFor cmd k = true →
        OnlyVolatileFieldsDiff cmd xs ys →
        OnlyVolatileFieldsDiff cmd xs ((k, v) :: ys)
    | stepVolatile {k : String} {v₁ v₂ : ResultValue} {xs ys : List (String × ResultValue)} :
        isVolatileFieldFor cmd k = true →
        OnlyVolatileFieldsDiff cmd xs ys →
        OnlyVolatileFieldsDiff cmd ((k, v₁) :: xs) ((k, v₂) :: ys)
    | step (k : String) {v₁ v₂ : ResultValue} {xs ys : List (String × ResultValue)} :
        OnlyVolatileValueDiff cmd v₁ v₂ →
        OnlyVolatileFieldsDiff cmd xs ys →
        OnlyVolatileFieldsDiff cmd ((k, v₁) :: xs) ((k, v₂) :: ys)
end

inductive OnlyVolatileObservationDiff : ResultObservation → ResultObservation → Prop where
  | rawText (text : String) :
      OnlyVolatileObservationDiff (.rawText text) (.rawText text)
  | structured (format : ResultFormat) {meta₁ meta₂ : ResultValue} (body : String) :
      OnlyVolatileValueDiff format.isCommandEnvelope meta₁ meta₂ →
      OnlyVolatileObservationDiff
        (.structured format meta₁ body) (.structured format meta₂ body)

mutual
  theorem volatile_only_diff_stripValue {cmd : Bool} {v₁ v₂ : ResultValue}
      (h : OnlyVolatileValueDiff cmd v₁ v₂) :
      stripValue cmd v₁ = stripValue cmd v₂ := by
    cases h with
    | atom _ => rfl
    | array hItems =>
        simp [stripValue, volatile_only_diff_stripArray hItems]
    | object hFields =>
        simp [stripValue, volatile_only_diff_stripFields hFields]
  termination_by sizeOf v₁ + sizeOf v₂

  theorem volatile_only_diff_stripArray {cmd : Bool} {xs ys : List ResultValue}
      (h : OnlyVolatileArrayDiff cmd xs ys) :
      stripArray cmd xs = stripArray cmd ys := by
    cases h with
    | nil => rfl
    | cons hx hxs =>
        simp [stripArray, volatile_only_diff_stripValue hx, volatile_only_diff_stripArray hxs]
  termination_by sizeOf xs + sizeOf ys

  theorem volatile_only_diff_stripFields {cmd : Bool} {xs ys : List (String × ResultValue)}
      (h : OnlyVolatileFieldsDiff cmd xs ys) :
      stripFields cmd xs = stripFields cmd ys := by
    cases h with
    | nil => rfl
    | skipLeft hk hrest =>
        simp [stripFields, hk, volatile_only_diff_stripFields hrest]
    | skipRight hk hrest =>
        simp [stripFields, hk, volatile_only_diff_stripFields hrest]
    | stepVolatile hk hrest =>
        simp [stripFields, hk, volatile_only_diff_stripFields hrest]
    | step k hv hrest =>
        by_cases hk : isVolatileFieldFor cmd k = true
        · simp [stripFields, hk, volatile_only_diff_stripFields hrest]
        · have hkFalse : isVolatileFieldFor cmd k = false := Bool.eq_false_iff.mpr hk
          simp [stripFields, hkFalse, volatile_only_diff_stripValue hv,
            volatile_only_diff_stripFields hrest]
  termination_by sizeOf xs + sizeOf ys
end

theorem volatile_only_diff_canonical_observation {obs₁ obs₂ : ResultObservation}
    (h : OnlyVolatileObservationDiff obs₁ obs₂) :
    canonicalObservation obs₁ = canonicalObservation obs₂ := by
  cases h with
  | rawText _ => rfl
  | structured format _ hMeta =>
      simp [canonicalObservation, volatile_only_diff_stripValue hMeta]

theorem volatile_only_diff_makes_no_progress (digest : ResultObservation → String)
    (tool arguments : String) {obs₁ obs₂ : ResultObservation}
    (h : OnlyVolatileObservationDiff obs₁ obs₂) :
    madeProgress [observationCallPrintWith digest tool arguments obs₁]
      ⟨[observationCallPrintWith digest tool arguments obs₂], false⟩ = false := by
  have heq :
      observationCallPrintWith digest tool arguments obs₁ =
        observationCallPrintWith digest tool arguments obs₂ := by
    simp [observationCallPrintWith, volatile_only_diff_canonical_observation h]
  rw [← heq]
  exact a_repeated_turn_makes_no_progress _

theorem volatile_only_diff_advances_streak (digest : ResultObservation → String)
    (tool arguments : String) (streak : Nat) {obs₁ obs₂ : ResultObservation}
    (h : OnlyVolatileObservationDiff obs₁ obs₂)
    (deadline : Option Nat) (now : Nat)
    (notWaiting : deadline.any (now < ·) = false) :
    let prev := observationCallPrintWith digest tool arguments obs₁
    let curr := observationCallPrintWith digest tool arguments obs₂
    step (⟨streak, [prev]⟩ : Record) (⟨[curr], false⟩ : Turn) deadline now =
      ((⟨streak + 1, [curr]⟩ : Record), decide (noProgressLimit ≤ streak + 1)) := by
  have hp : madeProgress [observationCallPrintWith digest tool arguments obs₁]
      ⟨[observationCallPrintWith digest tool arguments obs₂], false⟩ = false :=
    volatile_only_diff_makes_no_progress digest tool arguments h
  simp [step, observe, hp, notWaiting]

theorem volatile_only_polls_pause (digest : ResultObservation → String)
    (tool arguments : String) {obs₀ obs₁ obs₂ obs₃ : ResultObservation}
    (h₁ : OnlyVolatileObservationDiff obs₀ obs₁)
    (h₂ : OnlyVolatileObservationDiff obs₀ obs₂)
    (h₃ : OnlyVolatileObservationDiff obs₀ obs₃)
    (now : Nat) :
    let c (obs : ResultObservation) : Turn × Option Nat × Nat :=
      (⟨[observationCallPrintWith digest tool arguments obs], false⟩, none, now)
    (run Record.fresh [c obs₀, c obs₁, c obs₂, c obs₃]).2 = true := by
  have h1 := volatile_only_diff_canonical_observation h₁
  have h2 := volatile_only_diff_canonical_observation h₂
  have h3 := volatile_only_diff_canonical_observation h₃
  dsimp only
  simp only [observationCallPrintWith, ← h1, ← h2, ← h3]
  simp [run, step, observe, madeProgress, Record.fresh, noProgressLimit]

theorem insertField_val_inj (ys : List (String × ResultValue)) (k : String) (a b : ResultValue)
    (h : insertField (k, a) ys = insertField (k, b) ys) : a = b := by
  induction ys with
  | nil =>
      simp [insertField] at h
      exact h
  | cons head tail ih =>
      simp only [insertField] at h
      split at h
      · injection h with hHead _
        exact congrArg Prod.snd hHead
      · injection h with _ hTail
        exact ih hTail

theorem insertField_length (entry : String × ResultValue) (ys : List (String × ResultValue)) :
    (insertField entry ys).length = ys.length + 1 := by
  induction ys with
  | nil => simp [insertField]
  | cons head tail ih =>
      simp only [insertField]
      split
      · simp
      · simp [ih]

theorem non_volatile_field_update_changes_canonical (cmd : Bool) (k : String)
    (hk : isVolatileFieldFor cmd k = false) (v₁ v₂ : ResultValue)
    (hv : stripValue cmd v₁ ≠ stripValue cmd v₂)
    (rest : List (String × ResultValue)) :
    stripValue cmd (.object ((k, v₁) :: rest)) ≠
      stripValue cmd (.object ((k, v₂) :: rest)) := by
  intro heq
  simp only [stripValue, stripFields, hk, Bool.false_eq_true, ↓reduceIte, sortFields] at heq
  injection heq with hFields
  exact hv (insertField_val_inj (sortFields (stripFields cmd rest)) k
    (stripValue cmd v₁) (stripValue cmd v₂) hFields)

theorem non_volatile_field_addition_changes_canonical (cmd : Bool) (k : String)
    (hk : isVolatileFieldFor cmd k = false) (v : ResultValue)
    (rest : List (String × ResultValue)) :
    stripValue cmd (.object ((k, v) :: rest)) ≠ stripValue cmd (.object rest) := by
  intro heq
  simp only [stripValue, stripFields, hk, Bool.false_eq_true, ↓reduceIte, sortFields] at heq
  injection heq with hFields
  have hLen := congrArg List.length hFields
  rw [insertField_length] at hLen
  omega

theorem non_volatile_body_change_changes_canonical (format : ResultFormat)
    (meta₁ meta₂ : ResultValue) {body₁ body₂ : String} (hb : body₁ ≠ body₂) :
    canonicalObservation (.structured format meta₁ body₁) ≠
      canonicalObservation (.structured format meta₂ body₂) := by
  intro heq
  simp [canonicalObservation] at heq
  exact hb heq.2

theorem distinct_canonical_observation_resets_streak (record : Record)
    (digest : ResultObservation → String)
    (tool arguments : String) {obs : ResultObservation}
    (fresh : ∀ prev ∈ record.previous,
      prev ≠ observationCallPrintWith digest tool arguments obs)
    (changed : Bool) (deadline : Option Nat) (now : Nat) :
    let call := observationCallPrintWith digest tool arguments obs
    step record (⟨[call], changed⟩ : Turn) deadline now = ((⟨0, [call]⟩ : Record), false) := by
  have absent : observationCallPrintWith digest tool arguments obs ∉ record.previous :=
    fun hmem => fresh _ hmem rfl
  have hp : madeProgress record.previous
      ⟨[observationCallPrintWith digest tool arguments obs], changed⟩ = true := by
    simp [madeProgress, absent]
  simp [step, observe, hp]

theorem non_volatile_field_change_resets_streak (streak : Nat)
    (digest : ResultObservation → String)
    (inj : ∀ a b, digest a = digest b → a = b)
    (tool arguments : String) (format : ResultFormat) (k : String)
    (hk : isVolatileFieldFor format.isCommandEnvelope k = false)
    (v₁ v₂ : ResultValue)
    (hv : stripValue format.isCommandEnvelope v₁ ≠ stripValue format.isCommandEnvelope v₂)
    (rest : List (String × ResultValue)) (body : String)
    (deadline : Option Nat) (now : Nat) :
    let obs₁ : ResultObservation := .structured format (.object ((k, v₁) :: rest)) body
    let obs₂ : ResultObservation := .structured format (.object ((k, v₂) :: rest)) body
    let call₁ := observationCallPrintWith digest tool arguments obs₁
    let call₂ := observationCallPrintWith digest tool arguments obs₂
    step (⟨streak, [call₁]⟩ : Record) (⟨[call₂], false⟩ : Turn) deadline now =
      ((⟨0, [call₂]⟩ : Record), false) := by
  apply distinct_canonical_observation_resets_streak ⟨streak, _⟩ digest
  intro prev hmem heq
  simp only [List.mem_singleton] at hmem
  rw [hmem] at heq
  simp only [observationCallPrintWith, CallPrint.mk.injEq] at heq
  have hCanon := inj _ _ heq.2.2
  simp only [canonicalObservation, ResultObservation.structured.injEq] at hCanon
  exact non_volatile_field_update_changes_canonical format.isCommandEnvelope k hk v₁ v₂ hv rest
    (congrArg (ResultValue.object) (by
      have := hCanon.2.1
      simpa [stripValue] using this))

theorem non_volatile_body_change_resets_streak (streak : Nat)
    (digest : ResultObservation → String)
    (inj : ∀ a b, digest a = digest b → a = b)
    (tool arguments : String) (format : ResultFormat)
    (meta₁ meta₂ : ResultValue) {body₁ body₂ : String} (hb : body₁ ≠ body₂)
    (deadline : Option Nat) (now : Nat) :
    let obs₁ : ResultObservation := .structured format meta₁ body₁
    let obs₂ : ResultObservation := .structured format meta₂ body₂
    let call₁ := observationCallPrintWith digest tool arguments obs₁
    let call₂ := observationCallPrintWith digest tool arguments obs₂
    step (⟨streak, [call₁]⟩ : Record) (⟨[call₂], false⟩ : Turn) deadline now =
      ((⟨0, [call₂]⟩ : Record), false) := by
  apply distinct_canonical_observation_resets_streak ⟨streak, _⟩ digest
  intro prev hmem heq
  simp only [List.mem_singleton] at hmem
  rw [hmem] at heq
  simp only [observationCallPrintWith, CallPrint.mk.injEq] at heq
  exact non_volatile_body_change_changes_canonical format meta₁ meta₂ hb (inj _ _ heq.2.2)

/-- Prefix emitted by foreground/background command tools (`gents-loop`
`COMMAND_OUTPUT_META_PREFIX`). -/
def commandOutputMetaPrefix : String := "gents_exec: "

/-- Prefix emitted by filesystem tools (`gents-fs-runner`, `file_tools`). -/
def fsOutputMetaPrefix : String := "gents_fs: "

private def insertJsonField (entry : String × Lean.Json) :
    List (String × Lean.Json) → List (String × Lean.Json)
  | [] => [entry]
  | head :: tail =>
      if entry.1 ≤ head.1 then entry :: head :: tail
      else head :: insertJsonField entry tail

private def sortJsonFields : List (String × Lean.Json) → List (String × Lean.Json)
  | [] => []
  | entry :: rest => insertJsonField entry (sortJsonFields rest)

/-- Executable canonicalization of raw tool output strings: parses JSON
documents and `gents_exec:` / `gents_fs:` metadata envelopes, strips volatile
read-time fields recursively across nested objects and arrays, sorts object
keys in ascending order, and leaves plain non-JSON text and malformed headers
verbatim. -/
partial def canonicalizeAndRenderJson (commandEnvelope : Bool) : Lean.Json → String
  | .obj fields =>
      let kept := fields.toArray.toList.filterMap fun entry =>
        if isVolatileFieldFor commandEnvelope entry.1 then none
        else some (entry.1, entry.2)
      let rendered := (sortJsonFields kept).map fun (k, v) =>
        Lean.Json.renderString k ++ ":" ++ canonicalizeAndRenderJson commandEnvelope v
      "{" ++ ",".intercalate rendered ++ "}"
  | .arr items =>
      let rendered := items.toList.map (canonicalizeAndRenderJson commandEnvelope)
      "[" ++ ",".intercalate rendered ++ "]"
  | other => other.compress

private def splitFirstLine (text : String) : String × Option String :=
  match text.splitOn "\n" with
  | [] => (text, none)
  | [head] => (head, none)
  | head :: rest => (head, some ("\n".intercalate rest))

private def canonicalizePrefixedEnvelope (outPrefix : String) (commandEnvelope : Bool)
    (result : String) : Option String :=
  if result.startsWith outPrefix then
    let rest := result.drop outPrefix.length
    let (header, body) := splitFirstLine rest
    match Lean.Json.parse header with
    | .ok json =>
        let canonicalHeader := canonicalizeAndRenderJson commandEnvelope json
        let suffix := match body with
          | some text => "\n" ++ text
          | none => ""
        some (outPrefix ++ canonicalHeader ++ suffix)
    | .error _ => none
  else
    none

def canonicalizeResultText (result : String) : String :=
  match canonicalizePrefixedEnvelope commandOutputMetaPrefix true result with
  | some canonical => canonical
  | none =>
      match canonicalizePrefixedEnvelope fsOutputMetaPrefix false result with
      | some canonical => canonical
      | none =>
          match Lean.Json.parse result with
          | .ok json => canonicalizeAndRenderJson false json
          | .error _ => result

end GoalProgress
