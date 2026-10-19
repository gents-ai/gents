import Batteries.Tactic.OpenPrivate
import Proofs.PluginJob.Admission
import Proofs.ToolExecution.Transition
import Proofs.ToolPolicy.PluginReceipt

open private toString_nat_injective prefix_cancel from Proofs.PluginJob.Keys
open private admitStep_pinned from Proofs.PluginJob.Admission

/-!
# Plugin-job adoption

Adoption is one step on the existing tool-call transition, and it adds no
`ToolCallContext.Action`. When step 0 returns a `pin`, the host flips the call
to the background (`Transition.background`) and writes `job_pin` and the
admitted effects in one transaction. It writes no execution receipt: the
terminal compare-and-set is the only receipt writer, so the job's single
receipt is written by its terminal.

A step-0 `result` or `suspend` that carries a `pin` is adopted and
terminalized in the same transaction. It is delivered inline as the tool
result, because the caller is still in the turn that made the call; no
Continuation is appended. Beside a `resume`, it adopts a new chain row that is
already terminal and is itself a resume target.
-/

namespace PluginJob

open ToolExecution

/-- The host's handle rule in the adoption transaction. `taken` holds the
handles of the caller session's other chains. A free `h` is kept; otherwise
the handle is `h-n` for the least `n ≥ 2` that is free. The candidates
`n = 2 … taken.length + 2` are distinct, so one of them is free. A resume row
does not come through here: it inherits the chain root's handle, so one handle
names one chain per caller session. -/
def uniqueHandle (taken : List String) (h : String) : String :=
  if h ∈ taken then
    (((List.range (taken.length + 1)).map fun i => h ++ "-" ++ toString (i + 2)).find?
      fun c => decide (c ∉ taken)).getD h
  else h

/-- How the caller sees a row's terminal: inline, as the tool result of the
turn that made the call, or as a Continuation that wakes the caller. -/
inductive Delivery | inline | continuation
  deriving DecidableEq, Repr

/-- The adoption write on step 0. When the effects carry a guest pin, the row
flips to the background, its operation becomes `pluginJob`, and `pin` is the
`job_pin` written beside it: `operation = pluginJob` exactly when `job_pin` is
set. The admitted effects commit in the same transaction (`admitStep`). A
pinless step 0 is a non-job call and is never adopted. -/
def adopt (row : ToolCallContext) (pin : Pin) (e : Effects) : ToolCallContext × Option Pin :=
  if e.pin.isSome then ({ row with awaitMode := .background, operation := .pluginJob }, some pin)
  else (row, none)

/-- A step-0 `result` or `suspend` carrying a pin (and optionally a `resume`
beside it): the flip and the terminal land in one transaction, and the same
admitted step writes the pin on the job row (`admitStep`). A suspended row is
`completed` with its note, so it stays resumable. Every step-0 terminal is
delivered inline, never as a Continuation. -/
def adoptTerminal (row : ToolCallContext) (_pin : Pin) (out : StepOutput) :
    ToolCallContext × Delivery :=
  match out with
  | .result _ _ (some _) _ | .suspend _ _ _ (some _) _ =>
    ({ row with awaitMode := .background, operation := .pluginJob, state := .completed }, .inline)
  | _ => (row, .inline)

/-- A job address is `session_id:tool_call_id`, unique across sessions where
handles are not. It splits at the last `:`, because a session id may contain
`:` (child sessions are `job-session:` plus components) and a tool-call id
never does. The `JobView` field that carries it is named `address`, never
`key`: `strip_secret_fields` drops every field whose name contains KEY. -/
def splitAddress (a : String) : Option (String × String) :=
  let r := a.data.reverse
  let t := r.takeWhile (· != ':')
  match r.drop t.length with
  | ':' :: s => some (String.mk s.reverse, String.mk t.reverse)
  | _ => none

private theorem step0_terminal_admitted {ctx : StepContext} {job job' : Job} {out : StepOutput}
    {g : GuestPin} (hpin : job.pin = none) (hg : out.pin = some g)
    (hne : ∀ e, out ≠ .effects e) (h : admitStep ctx job out = .ok job') :
    job' =
        { stepRow ctx job out with
          pin := some (adoptedPin ctx out g), consecutiveFailures := 0, state := .completed } ∧
      liveCount (stepRow ctx job out) = 0 := by
  have hp : stepPin ctx job out = some (adoptedPin ctx out g) := by simp [stepPin, hpin, hg]
  have h' := admitStep_pinned h hp
  cases out with
  | effects e => exact absurd rfl (hne e)
  | fail x => simp [StepOutput.pin] at hg
  | result x c p r =>
    simp only [admitJobOutput, endRow] at h'
    split_ifs at h' with hz
    cases h'
    simp only [Bool.and_eq_true, beq_iff_eq] at hz
    exact ⟨rfl, hz.1⟩
  | suspend k rs x p r =>
    simp only [admitJobOutput, endRow] at h'
    split_ifs at h' with hz
    cases h'
    simp only [Bool.and_eq_true, beq_iff_eq] at hz
    exact ⟨rfl, hz.1⟩

private theorem step0_effects_pin {ctx : StepContext} {job job' : Job} {e : Effects}
    {g : GuestPin} (hpin : job.pin = none) (hg : e.pin = some g)
    (h : admitStep ctx job (.effects e) = .ok job') : job'.pin = some (adoptedPin ctx (.effects e) g) := by
  have hp : stepPin ctx job (.effects e) = some (adoptedPin ctx (.effects e) g) := by
    simp [stepPin, hpin, StepOutput.pin, hg]
  have h' := admitStep_pinned h hp
  simp only [admitJobOutput, admitEffects] at h'
  split at h'
  · cases h'
  · split_ifs at h'
    cases h'
    rfl

/-- A terminal, pinned row is a resume target: `resumeValid` accepts it for any
caller with its process-control scope, the same plugin, a block naming it, and
the script, engine and budget rules. -/
private theorem pinned_terminal_resumable (j : Job) (p : Pin) (hs : j.state ≠ .running)
    (hp : j.pin = some p) (caller owner : Background.ProcessControl.Scope) (g : GuestPin)
    (facts : HostFacts) (r : Resume) (ha : Background.ProcessControl.authorized caller owner = true)
    (hr : r.rowDoc = j.rowDoc) (hf : facts.resumeOf = some j.rowDoc)
    (he : r.engineCurrent = true ∨ (g.scriptId = p.scriptId ∧ facts.engineDigest = p.engineDigest))
    (hb : p.agentBudget ≤ g.agentBudget) :
    resumeValid caller owner true j g facts r = true := by
  have he' : (r.engineCurrent || (g.scriptId == p.scriptId && facts.engineDigest == p.engineDigest))
      = true := by
    rcases he with he | ⟨h1, h2⟩ <;> simp [*]
  simp [resumeValid, resultReadAuthorized, hs, hp, ha, hr, hf, he', hb]

/-- An admitted step 0 carrying a pin is the existing background transition:
the running foreground call flips to the background, and the adopted row is
that flip with its operation set to `pluginJob`. The admitted step pins the
job row. -/
theorem adoption_is_background_transition (ctx : StepContext) (row : ToolCallContext)
    (job job' : Job) (e : Effects) (g : GuestPin) (h_state : row.state = .running)
    (h_mode : row.awaitMode = .foreground) (hpin : job.pin = none) (hg : e.pin = some g)
    (h : admitStep ctx job (.effects e) = .ok job') :
    ToolCallContext.Transition row { row with awaitMode := .background } ∧
      ToolCallContext.Transition { row with operation := .pluginJob }
        (adopt row (adoptedPin ctx (.effects e) g) e).1 ∧
      job'.pin = some (adoptedPin ctx (.effects e) g) := by
  refine ⟨.background h_state h_mode rfl, .background h_state h_mode ?_, step0_effects_pin hpin hg h⟩
  simp [adopt, hg]

/-- The flip, the pin and the admitted effects commit together or not at all.
An admitted step 0 flips the row, writes the pin, and charges and records
every fresh spawn. A refused step 0 is never adopted: the job row keeps no
pin, its children and its spend, and it fails closed (`settleStep`). -/
theorem adoption_atomic (ctx : StepContext) (row : ToolCallContext) (job : Job) (e : Effects)
    (g : GuestPin) (obs : SpawnKey → Recovery.KillObservation) (hpin : job.pin = none)
    (hg : e.pin = some g) :
    (∀ job', admitStep ctx job (.effects e) = .ok job' →
      adopt row (adoptedPin ctx (.effects e) g) e =
          ({ row with awaitMode := .background, operation := .pluginJob }, some (adoptedPin ctx (.effects e) g)) ∧
        job'.pin = some (adoptedPin ctx (.effects e) g) ∧
        job'.spent = (stepRow ctx job (.effects e)).spent +
            (freshSpawns (stepRow ctx job (.effects e)) e.spawns).length ∧
        ∀ s ∈ freshSpawns (stepRow ctx job (.effects e)) e.spawns,
          ∃ ch ∈ job'.children, ch.key = spawnKey (stepRow ctx job (.effects e)) s) ∧
      ∀ r, admitStep ctx job (.effects e) = .error r →
        (settleStep ctx job (.effects e) obs).1.pin = none ∧
          (settleStep ctx job (.effects e) obs).1.children = job.children ∧
          (settleStep ctx job (.effects e) obs).1.spent = job.spent := by
  have hp : stepPin ctx job (.effects e) = some (adoptedPin ctx (.effects e) g) := by
    simp [stepPin, hpin, StepOutput.pin, hg]
  refine ⟨fun job' h => ?_, fun r h => ?_⟩
  · exact ⟨by simp [adopt, hg], step0_effects_pin hpin hg h,
      (admit_all_or_nothing ctx job e obs).1 job' _ h hp⟩
  · refine ⟨?_, (admit_all_or_nothing ctx job e obs).2 r h⟩
    unfold settleStep
    rw [h]
    cases r <;> by_cases hs : job.state = .running <;> simp [terminalizeRow, hs, hpin]

/-- Adoption writes no receipt. The adopted row is still running, and a
well-formed receipt row of a running call carries none: the terminal
compare-and-set is the only receipt writer. A receipt written at adoption
would make that compare-and-set a conflict. -/
theorem adoption_writes_no_receipt (row : ToolCallContext) (pin : Pin) (e : Effects)
    (rr : ToolPolicy.PluginReceipt.Row) (h_state : row.state = .running) (hwf : rr.wellFormed)
    (h_running : rr.terminal = false) :
    (adopt row pin e).1.state = .running ∧ rr = ⟨false, none⟩ ∧
      ∀ x r, ToolPolicy.PluginReceipt.decide false (some x) r = .conflict := by
  refine ⟨?_, ?_, fun _ _ => rfl⟩
  · unfold adopt
    split <;> simp [h_state]
  · obtain ⟨t, receipt⟩ := rr
    simp only at h_running
    subst h_running
    cases receipt with
    | none => rfl
    | some x => exact absurd (hwf rfl) Bool.false_ne_true

/-- The adopted row's terminal compare-and-set meets `decide false none =
.commit`, so the job ends with exactly one receipt, and no later terminal
attempt changes it. -/
theorem adopted_job_single_receipt (t t' : ToolPolicy.PluginReceipt.Terminal)
    (r r' : ToolPolicy.PluginReceipt.Receipt) :
    ToolPolicy.PluginReceipt.decide false none (ToolPolicy.PluginReceipt.published t r) = .commit ∧
      ToolPolicy.PluginReceipt.step ⟨false, none⟩ t r =
        ⟨true, some (ToolPolicy.PluginReceipt.published t r)⟩ ∧
      ToolPolicy.PluginReceipt.step (ToolPolicy.PluginReceipt.step ⟨false, none⟩ t r) t' r' =
        ToolPolicy.PluginReceipt.step ⟨false, none⟩ t r :=
  ⟨rfl, rfl, ToolPolicy.PluginReceipt.terminal_row_is_final _ _ _ rfl⟩

/-- The adopted row is a detached `pluginJob` row carrying the pin, so it never
returns to the foreground. -/
theorem adopted_row_is_detached (row : ToolCallContext) (pin : Pin) (e : Effects)
    (hg : e.pin.isSome = true) :
    (adopt row pin e).1.operation = .pluginJob ∧ (adopt row pin e).1.operation.isDetached = true ∧
      (adopt row pin e).2 = some pin ∧
      ∀ post, ToolCallContext.Transition (adopt row pin e).1 post → post.awaitMode = .background := by
  have ha : adopt row pin e =
      ({ row with awaitMode := .background, operation := .pluginJob }, some pin) := by
    simp [adopt, hg]
  rw [ha]
  exact ⟨rfl, rfl, rfl, fun _ hstep =>
    ToolCallContext.plugin_job_row_never_foregrounds rfl rfl hstep⟩

/-- Every pin field outside `GuestPin` is the host's fact. -/
theorem pin_host_fields_overwritten (g : GuestPin) (f : HostFacts) :
    (hostPin g f).engineDigest = f.engineDigest ∧ (hostPin g f).pluginRef = f.pluginRef ∧
      (hostPin g f).cwd = f.cwd ∧ (hostPin g f).argsDigest = f.argsDigest ∧
      (hostPin g f).chainRoot = f.chainRoot ∧ (hostPin g f).origin = f.origin ∧
      (hostPin g f).adoptedAt = f.adoptedAt ∧ (hostPin g f).resumeOf = f.resumeOf ∧
      (hostPin g f).replayedFrom = f.replayedFrom ∧ (hostPin g f).ceilingAtPin = f.ceilingAtPin :=
  ⟨rfl, rfl, rfl, rfl, rfl, rfl, rfl, rfl, rfl, rfl⟩

/-- The handle the adoption transaction writes is not held by another chain
of the caller session. -/
theorem adopted_handle_unique_in_session (taken : List String) (h : String) :
    uniqueHandle taken h ∉ taken := by
  unfold uniqueHandle
  split_ifs with hh
  · generalize hf : ((List.range (taken.length + 1)).map
        fun i => h ++ "-" ++ toString (i + 2)).find? (fun c => decide (c ∉ taken)) = found
    cases found with
    | some c => simpa using List.find?_some hf
    | none =>
      exfalso
      have hall : ∀ c ∈ (List.range (taken.length + 1)).map
          (fun i => h ++ "-" ++ toString (i + 2)), c ∈ taken := by
        intro c hc
        simpa using List.find?_eq_none.mp hf c hc
      have hnd : ((List.range (taken.length + 1)).map
          fun i => h ++ "-" ++ toString (i + 2)).Nodup := by
        refine List.Nodup.map (fun a b hab => ?_) (List.nodup_range _)
        have := toString_nat_injective _ _ (prefix_cancel _ _ _ hab)
        omega
      have hlen := (hnd.subperm hall).length_le
      simp at hlen
  · exact hh

/-- One admitted step 0 writes the flip, the pin and the terminal, and the row
it ends (a `result`, or a `suspend` of rows 18 and 21) is a resume target. -/
theorem step0_pinned_terminal_adopts_and_terminalizes (ctx : StepContext)
    (row : ToolCallContext) (job job' : Job) (out : StepOutput) (g : GuestPin)
    (hpin : job.pin = none) (hg : out.pin = some g) (hne : ∀ e, out ≠ .effects e)
    (h : admitStep ctx job out = .ok job') :
    job' =
        { stepRow ctx job out with
          pin := some (adoptedPin ctx out g), consecutiveFailures := 0, state := .completed } ∧
      (adoptTerminal row (adoptedPin ctx out g) out).1 =
        { row with awaitMode := .background, operation := .pluginJob, state := .completed } ∧
      liveCount (stepRow ctx job out) = 0 ∧
      ∀ caller owner g' facts r, Background.ProcessControl.authorized caller owner = true →
        r.rowDoc = job'.rowDoc → facts.resumeOf = some job'.rowDoc →
        (r.engineCurrent = true ∨ (g'.scriptId = (adoptedPin ctx out g).scriptId ∧
          facts.engineDigest = (adoptedPin ctx out g).engineDigest)) →
        (adoptedPin ctx out g).agentBudget ≤ g'.agentBudget →
        resumeValid caller owner true job' g' facts r = true := by
  obtain ⟨hj, hlive⟩ := step0_terminal_admitted hpin hg hne h
  refine ⟨hj, ?_, hlive, fun caller owner g' facts r ha hr hf he hb =>
    pinned_terminal_resumable job' _ (by simp [hj]) (by simp [hj]) caller owner g' facts r
      ha hr hf he hb⟩
  cases out with
  | effects e => exact absurd rfl (hne e)
  | fail x => simp [StepOutput.pin] at hg
  | result x c p r =>
    cases p with
    | none => simp [StepOutput.pin] at hg
    | some _ => rfl
  | suspend k rs x p r =>
    cases p with
    | none => simp [StepOutput.pin] at hg
    | some _ => rfl

/-- A step-0 terminal is the tool result of the turn that made the call: it is
delivered inline and no Continuation is appended. -/
theorem step0_pinned_terminal_delivered_inline (row : ToolCallContext) (pin : Pin)
    (out : StepOutput) :
    (adoptTerminal row pin out).2 = .inline ∧ (adoptTerminal row pin out).2 ≠ .continuation := by
  unfold adoptTerminal
  split <;> simp

/-- Whatever the output, a step-0 pin beside a `resume` is written as the resume
pin built from the facts `resumeValid` checked: the handle, chain root and
`args_digest` come from the chain root, and `resumeOf` is the validated prior row. -/
theorem step0_resume_pin_inherits_chain_root (ctx : StepContext) (job rootJob priorJob : Job)
    (out : StepOutput) (g : GuestPin) (r : Resume) (owner : Background.ProcessControl.Scope)
    (samePlugin : Bool) (rootPin priorPin : Pin) (hpin : job.pin = none)
    (hg : out.pin = some g) (hr : out.resume = some r)
    (ht : ctx.resumeTarget r.rowDoc = some (owner, samePlugin, priorJob, rootJob))
    (hrp : rootJob.pin = some rootPin) (hpp : priorJob.pin = some priorPin) :
    ∃ p, stepPin ctx job out = some p ∧ p.handle = rootPin.handle ∧
      p.chainRoot = rootPin.chainRoot ∧ p.argsDigest = rootPin.argsDigest ∧
      p.resumeOf = ctx.resumeFacts.resumeOf := by
  refine ⟨resumePin rootPin priorPin g ctx.resumeFacts, ?_, rfl, rfl, rfl, rfl⟩
  simp [stepPin, adoptedPin, hpin, hg, hr, ht, hrp, hpp]

/-- A step-0 `result` or `suspend` carrying a pin and a `resume` adopts a new
chain row that is already terminal. Its pin is the resume pin: handle, chain
root and `args_digest` from the chain root, `resumeOf` the prior row. The row
carries the prior row's children and spend under the chain root's id, journals
the resume block with its answer, is delivered inline, and as the new chain
head is itself a resume target. -/
theorem step0_pinned_terminal_with_resume_is_terminal_chain_row (ctx : StepContext)
    (row : ToolCallContext) (job job' rootJob priorJob : Job) (out : StepOutput) (g : GuestPin)
    (r : Resume) (owner : Background.ProcessControl.Scope) (samePlugin : Bool)
    (rootPin priorPin : Pin) (hrp : rootJob.pin = some rootPin) (hpp : priorJob.pin = some priorPin)
    (hpin : job.pin = none)
    (hg : out.pin = some g) (hr : out.resume = some r) (hne : ∀ e, out ≠ .effects e)
    (ht : ctx.resumeTarget r.rowDoc = some (owner, samePlugin, priorJob, rootJob))
    (h : admitStep ctx job out = .ok job') :
    job'.state = .completed ∧
      job'.pin = some (resumePin rootPin priorPin g ctx.resumeFacts) ∧
      job'.root = rootJob.root ∧
      job'.children = priorJob.children ∧
      job'.spent = priorJob.spent ∧
      (resumePin rootPin priorPin g ctx.resumeFacts).handle = rootPin.handle ∧
      (resumePin rootPin priorPin g ctx.resumeFacts).chainRoot = rootPin.chainRoot ∧
      (resumePin rootPin priorPin g ctx.resumeFacts).argsDigest = rootPin.argsDigest ∧
      (resumePin rootPin priorPin g ctx.resumeFacts).resumeOf = some r.rowDoc ∧
      job'.resume = out.resume ∧
      (stepEcho job').control = some (echo r) ∧
      (adoptTerminal row (resumePin rootPin priorPin g ctx.resumeFacts) out).2 = .inline ∧
      ∀ caller owner g' facts' r', Background.ProcessControl.authorized caller owner = true →
        r'.rowDoc = job'.rowDoc →
        facts'.resumeOf = some job'.rowDoc →
        (r'.engineCurrent = true ∨ (g'.scriptId = (resumePin rootPin priorPin g ctx.resumeFacts).scriptId ∧
          facts'.engineDigest = (resumePin rootPin priorPin g ctx.resumeFacts).engineDigest)) →
        (resumePin rootPin priorPin g ctx.resumeFacts).agentBudget ≤ g'.agentBudget →
        resumeValid caller owner true job' g' facts' r' = true := by
  obtain ⟨hj, -⟩ := step0_terminal_admitted hpin hg hne h
  have hhost : adoptedPin ctx out g = resumePin rootPin priorPin g ctx.resumeFacts := by
    simp [adoptedPin, hr, ht, hrp, hpp]
  obtain ⟨g₂, owner₂, samePlugin₂, prior₂, root₂, hg₂, ht₂, hv⟩ :=
    admitted_resume_valid ctx job job' out r h hr
  rw [ht] at ht₂
  obtain ⟨-, -, rfl, rfl⟩ := Option.some.inj ht₂
  rw [hg] at hg₂
  obtain rfl := Option.some.inj hg₂
  obtain ⟨hrow, hres⟩ := resume_names_prior_row _ _ _ _ rootPin priorPin _ _ _ hv
  have hfacts : ctx.resumeFacts.resumeOf = some r.rowDoc := by
    rw [hrow]; exact hres
  rw [hhost, stepRow_resume hr ht] at hj
  subst hj
  have hs : ({ resumeRow rootJob priorJob job r with
      pin := some (resumePin rootPin priorPin g ctx.resumeFacts), consecutiveFailures := 0,
      state := .completed } : Job).state = .completed := rfl
  have hp : ({ resumeRow rootJob priorJob job r with
      pin := some (resumePin rootPin priorPin g ctx.resumeFacts), consecutiveFailures := 0,
      state := .completed } : Job).pin = some (resumePin rootPin priorPin g ctx.resumeFacts) := rfl
  refine ⟨hs, hp, rfl, rfl, rfl, rfl, rfl, rfl, hfacts, by simp [resumeRow, hr], rfl,
    (step0_pinned_terminal_delivered_inline row _ out).1,
    fun caller owner g' facts' r' ha hr' hf he hb =>
      pinned_terminal_resumable _ _ (by simp [hs]) hp caller owner g' facts' r' ha hr' hf he hb⟩

/-- An address whose tool-call id has no `:` splits back into its session id
and tool-call id, whatever `:` the session id holds. -/
theorem splitAddress_roundtrip (s t : String) (h : ':' ∉ t.data) :
    splitAddress (s ++ ":" ++ t) = some (s, t) := by
  have hd : (s ++ ":" ++ t).data.reverse = t.data.reverse ++ ':' :: s.data.reverse := by
    simp [String.data_append]
  have ht : (t.data.reverse ++ ':' :: s.data.reverse).takeWhile (· != ':') = t.data.reverse :=
    Triggers.Durable.before_colon _ _ (by simpa using h)
  have hr : (t.data.reverse ++ ':' :: s.data.reverse).drop t.data.reverse.length =
      ':' :: s.data.reverse := List.drop_left' rfl
  simp only [splitAddress, hd, ht, hr, List.reverse_reverse]

end PluginJob
