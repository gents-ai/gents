import Proofs.PluginJob.Control
import Proofs.ToolPolicy.PluginCalls
import Proofs.Request.CausalHop
import Proofs.Background.Budget
import Proofs.Enrollment.RequestInput

/-!
# Plugin-job step admission

`admitStep` is the host's check of one step output against the facts it
re-reads in the step transaction. Every refusal except `.terminal` fails the
row closed and kills its own children (`settleStep`). The engine pre-checks
budget, liveness and targets, so a host refusal means guest and host diverged.
-/

namespace PluginJob

open ToolPolicy

/-- Facts re-read in the step transaction. -/
structure StepContext where
  live : Ceiling
  available : Bool
  /-- The caller's AgentTools target ceiling (the `agent_new` filter). -/
  callerTargets : EndpointScope Target Unit
  /-- `job.agent_targets`, resolved. -/
  jobTargets : EndpointScope Target Unit
  policy : Policy
  engineDigest : String
  engineProvenanceOperator : Bool
  /-- Whether a source digest is still installed-origin under the mounts,
  install records and `policy.trustedPacks` re-read in this step's transaction
  (`originOf`): a file of a pack-tier mount whose pack has `operator` install
  provenance and is trusted. Read on every step. -/
  installedTrusted : String → Bool
  maxHop : Option Nat
  /-- The target session's current hop. -/
  targetHop : Target → Nat
  cwdAllowed : String → Bool
  within : String → String → Bool
  /-- The caller request's live background rows; read on step 0. -/
  liveBackground : Nat
  /-- The host's cross-session ancestry walk; read on step 0. -/
  startedByJob : Bool
  /-- The origin the host resolves for the script a call names without a
  `script` or a `path` (`originOf` over its source digest): a kept or
  project-tier script is `dynamic`. `none` when the call names no script.
  Read on step 0. -/
  namedOrigin : Option Origin
  /-- The facts the host writes into the pin it adopts from a step-0 guest pin
  with no `resume`. The guest supplies only the `GuestPin` fields; the origin
  and every other field are host facts, so step-0 effects are admitted under
  the origin the host computes at adoption. Read on step 0. -/
  adoptionFacts : HostFacts
  /-- The calling row's process-control scope; read for non-job `control`. -/
  callerScope : Background.ProcessControl.Scope
  /-- The owner scope of a row a non-job `control` names, and whether it
  belongs to the same plugin; `none` when no such row exists. -/
  controlTarget : DocId → Option (Background.ProcessControl.Scope × Bool)
  /-- The prior row a step-0 `resume` names, with its owner scope, whether it
  belongs to the same plugin, and the chain root row; `none` when no such row
  exists. Read on step 0. -/
  resumeTarget : DocId → Option (Background.ProcessControl.Scope × Bool × Job × Job)
  /-- Whether no row of the chain has `resume_of` naming that prior row, so it
  is the chain head and its children and spend are the chain's. Read on step 0. -/
  resumeIsHead : DocId → Bool
  /-- The facts the host writes into a resume pin, `resumeOf` included. Read on
  step 0. -/
  resumeFacts : HostFacts

/-- Live ∩ pinned ceiling ∩ job targets ∩ the caller's ceiling, and the dynamic
policy's targets for a dynamic origin. -/
def effectiveTargets (ctx : StepContext) (pin : Pin) : EndpointScope Target Unit :=
  ((ctx.live.meet pin.ceilingAtPin).targets.meet unitVM ctx.jobTargets).meet unitVM ctx.callerTargets
  |> (if pin.origin = .dynamic then (·.meet unitVM ctx.policy.dynamicTargets) else id)

inductive Refusal
  | terminal | notAvailable | engineChanged | policyDenied | stalled | depthExceeded | timedOut
  | stepFailed | backgroundBudgetExceeded
  deriving DecidableEq, Repr

/-- The host terminal codes; `.terminal` is a no-op and has none. -/
def Refusal.code : Refusal → Option String
  | .terminal => none
  | .notAvailable => some "plugin_job_not_available"
  | .engineChanged => some "plugin_job_engine_changed"
  | .policyDenied => some "plugin_job_policy_denied"
  | .stalled => some "plugin_job_stalled"
  | .depthExceeded => some "plugin_job_depth_exceeded"
  | .timedOut => some "plugin_job_timed_out"
  | .stepFailed => some "plugin_job_step_failed"
  | .backgroundBudgetExceeded => some "background_tool_budget_exceeded"

def StepOutput.pin : StepOutput → Option GuestPin
  | .result _ _ p _ => p
  | .fail _ => none
  | .suspend _ _ _ p _ => p
  | .effects e => e.pin

def StepOutput.resume : StepOutput → Option Resume
  | .result _ _ _ r => r
  | .fail _ => none
  | .suspend _ _ _ _ r => r
  | .effects e => e.resume

/-- The pin a step-0 guest pin is adopted as. A `resume` naming a target whose
chain root and prior row are pinned adopts the resume pin, built from the same
`resumeFacts` that `resumeAdmitted` validates; every other pin is a `hostPin`
over `adoptionFacts`. -/
def adoptedPin (ctx : StepContext) (out : StepOutput) (g : GuestPin) : Pin :=
  match out.resume with
  | some r =>
    match ctx.resumeTarget r.rowDoc with
    | some (_, _, prior, root) =>
      match root.pin, prior.pin with
      | some rootPin, some priorPin => resumePin rootPin priorPin g ctx.resumeFacts
      | _, _ => hostPin g ctx.adoptionFacts
    | none => hostPin g ctx.adoptionFacts
  | none => hostPin g ctx.adoptionFacts

/-- An unpinned row is on step 0. A pin on a later step is ignored: the first
pin wins. A pinless step 0 is a non-job call (`none`). -/
def stepPin (ctx : StepContext) (job : Job) (out : StepOutput) : Option Pin :=
  match job.pin with
  | some p => some p
  | none => out.pin.map (adoptedPin ctx out)

/-- Rule 3. -/
def engineChanged (ctx : StepContext) (job : Job) (out : StepOutput) : Bool :=
  match job.pin with
  | some p => ctx.engineDigest != p.engineDigest && !(out.resume.any (·.engineCurrent))
  | none => false

/-- Rule 4: `resume` only on step 0, on `effects` or beside a `pin`. -/
def resumePlaced (job : Job) : StepOutput → Bool
  | .effects _ => job.pin.isNone
  | out => job.pin.isNone && out.pin.isSome

/-- Rule 4: a step-0 `resume` is admitted only beside the guest pin it
adopts, naming a row `resumeValid` accepts under the caller's scope whose
chain root is pinned, so the adopted pin is always the resume pin. The named
row must be the chain head: a resume of a row an earlier resume continued
would replay that row's stale children and spend. -/
def resumeAdmitted (ctx : StepContext) (out : StepOutput) : Bool :=
  match out.resume with
  | none => true
  | some r =>
    match out.pin, ctx.resumeTarget r.rowDoc with
    | some g, some (owner, samePlugin, prior, root) =>
      ctx.resumeIsHead r.rowDoc && root.pin.isSome &&
        resumeValid ctx.callerScope owner samePlugin prior g ctx.resumeFacts r
    | _, _ => false

/-- The row a step is admitted against: a validated step-0 `resume` continues
the chain, so its freshness, spend and live bounds are the chain head's
children and spend (`resumeRow`), not the new row's empty ones. -/
def stepRow (ctx : StepContext) (job : Job) (out : StepOutput) : Job :=
  match out.resume with
  | some r =>
    match ctx.resumeTarget r.rowDoc with
    | some (_, _, prior, root) => resumeRow root prior job r
    | none => job
  | none => job

theorem stepRow_state (ctx : StepContext) (job : Job) (out : StepOutput) :
    (stepRow ctx job out).state = job.state := by
  unfold stepRow; split <;> (try split) <;> rfl

theorem stepRow_pin (ctx : StepContext) (job : Job) (out : StepOutput) :
    (stepRow ctx job out).pin = job.pin := by
  unfold stepRow; split <;> (try split) <;> rfl

theorem stepRow_callerHop (ctx : StepContext) (job : Job) (out : StepOutput) :
    (stepRow ctx job out).callerHop = job.callerHop := by
  unfold stepRow; split <;> (try split) <;> rfl

theorem stepRow_resume {ctx : StepContext} {job : Job} {out : StepOutput} {r : Resume}
    {owner : Background.ProcessControl.Scope} {samePlugin : Bool} {prior root : Job}
    (hr : out.resume = some r) (ht : ctx.resumeTarget r.rowDoc = some (owner, samePlugin, prior, root)) :
    stepRow ctx job out = resumeRow root prior job r := by
  simp [stepRow, hr, ht]

theorem stepRow_children {ctx : StepContext} {job : Job} {out : StepOutput} {c : Child}
    (hc : c ∈ (stepRow ctx job out).children) :
    c ∈ job.children ∨ ∃ r owner samePlugin prior root, out.resume = some r ∧
      ctx.resumeTarget r.rowDoc = some (owner, samePlugin, prior, root) ∧ c ∈ prior.children := by
  cases hr : out.resume with
  | none => simp only [stepRow, hr] at hc; exact Or.inl hc
  | some r =>
    cases ht : ctx.resumeTarget r.rowDoc with
    | none => simp only [stepRow, hr, ht] at hc; exact Or.inl hc
    | some t =>
      obtain ⟨owner, samePlugin, prior, root⟩ := t
      rw [stepRow_resume hr ht] at hc
      exact Or.inr ⟨r, owner, samePlugin, prior, root, rfl, ht, hc⟩

def spawnKey (job : Job) (s : Spawn) : SpawnKey :=
  { root := job.root, slot := s.slot, attempt := s.attempt }

/-- Slots are attributed by `retry_key` lookup, never by parsing. -/
def known (job : Job) (k : SpawnKey) : Bool :=
  job.children.any fun ch => ch.key.retryKey == k.retryKey

/-- The first spawn of each retry key; a later spawn of a kept key is a no-op. -/
def firstOfKey (key : Spawn → String) : List String → List Spawn → List Spawn
  | _, [] => []
  | seen, s :: rest =>
    if key s ∈ seen then firstOfKey key seen rest
    else s :: firstOfKey key (key s :: seen) rest

/-- Re-served keys, and repeats of a key inside one panel, are no-ops. -/
def freshSpawns (job : Job) (spawns : List Spawn) : List Spawn :=
  firstOfKey (fun s => (spawnKey job s).retryKey) [] (spawns.filter fun s => !known job (spawnKey job s))

def maxSlotBytes : Nat := 128

def liveCount (job : Job) : Nat := (liveOwnChildren job).length

def terminalCount (job : Job) : Nat :=
  (job.children.filter fun ch => ch.key.root == job.root && ch.state != .live).length

/-- `none` is the pinned caller cwd. -/
def cwdAdmitted (ctx : StepContext) (pin : Pin) : Option String → Bool
  | none => true
  | some c => ctx.within pin.cwd c && ctx.cwdAllowed c

/-- A slot opens at attempt 0; a later attempt is admitted only as the next
attempt after a latest attempt that ended interrupted, as `reattempt` does. -/
def attemptAdmitted (job : Job) (s : Spawn) : Bool :=
  match latestAttempt job s.slot with
  | none => s.attempt == 0
  | some ch => ch.state == .interrupted && s.attempt == ch.key.attempt + 1

/-- Rule 5, for one fresh spawn. -/
def spawnRefusal (ctx : StepContext) (pin : Pin) (job : Job) (s : Spawn) : Option Refusal :=
  if ¬ (effectiveTargets ctx pin).permits s.target then some .policyDenied
  else if !CausalHop.admitHop ctx.maxHop
      (CausalHop.nextHop (.crossSession job.callerHop) (ctx.targetHop s.target)) then
    some .depthExceeded
  else if !cwdAdmitted ctx pin s.cwd then some .policyDenied
  else if maxSlotBytes < s.slot.utf8ByteSize then some .policyDenied
  else if !attemptAdmitted job s then some .policyDenied
  else none

/-- Rule 6: the whole panel of `n` fresh spawns fits the budget and the live
bound of the live ∩ pinned ceiling. -/
def panelAdmitted (ctx : StepContext) (pin : Pin) (job : Job) (n : Nat) : Bool :=
  decide (job.spent + n ≤ Nat.min pin.agentBudget (ctx.live.meet pin.ceilingAtPin).maxSpawns) &&
    decide (liveCount job + n ≤ (ctx.live.meet pin.ceilingAtPin).maxLive)

def addSpawns (job : Job) (fresh : List Spawn) : Job :=
  { job with
    children := job.children ++ fresh.map fun s =>
      { key := spawnKey job s, target := s.target, state := .live, messages := 0 }
    spent := job.spent + fresh.length }

/-- Rule 7: a message reaches its slot's latest attempt in this chain as the
step began, which must exist, so a session's opening request is never a message
of the step that spawned it, whose target the effective target ceiling still
permits, and keeps the slot within its message cap. A message that re-lives a
terminal attempt counts against the live bound like a spawn (`admitEffects`). -/
def messagesAdmitted (cap : Nat) (targets : EndpointScope Target Unit) (job : Job)
    (msgs : List String) : Bool :=
  msgs.all fun slot =>
    match latestAttempt job slot with
    | some ch => decide (targets.permits ch.target ∧ ch.messages + msgs.count slot ≤ cap)
    | none => false

/-- Messages are uncharged: the slot's message count grows and its latest
attempt is live again, as the message lands a new request in its session. -/
def addMessages (job : Job) (msgs : List String) : Job :=
  { job with
    children := job.children.map fun ch =>
      let n := (msgs.filter fun slot => (latestAttempt job slot).map (·.key) == some ch.key).length
      { ch with messages := ch.messages + n, state := if n = 0 then ch.state else .live } }

/-- A message re-lives its slot's latest attempt, terminal or not, so the
reply reaches a step: the row keeps a live child instead of waiting out
`max_wall_secs`: a slot's view is its latest request. -/
theorem message_relives_latest_attempt (job : Job) (msgs : List String) (slot : String)
    (ch : Child) (hlat : latestAttempt job slot = some ch) (hmem : ch ∈ job.children)
    (hroot : ch.key.root = job.root) (hs : slot ∈ msgs) :
    0 < liveCount (addMessages job msgs) := by
  have hn : 0 < (msgs.filter fun sl => (latestAttempt job sl).map (·.key) == some ch.key).length :=
    List.length_pos_of_mem (List.mem_filter.mpr ⟨hs, by simp [hlat]⟩)
  refine List.length_pos_of_mem (List.mem_filter.mpr ⟨List.mem_map_of_mem _ hmem, ?_⟩)
  simp [addMessages, hroot, Nat.pos_iff_ne_zero.mp hn]

/-- Rule 8. -/
def interruptsAdmitted (job : Job) (slots : List String) : Bool :=
  slots.all fun slot => (latestAttempt job slot).isSome

/-- Rule 9: no live child, no unobserved terminal, no fresh spawn and no
message. An interrupt is not new: it ends only a live child, which already
keeps the job, and an interrupt of a terminal slot changes nothing. -/
def stalled (job : Job) (fresh : List Spawn) (msgs : List String) : Bool :=
  liveCount job == 0 && !decide (job.observedTerminal < terminalCount job) && fresh.isEmpty &&
    msgs.isEmpty

/-- Rules 5-9. An admitted step has observed every terminal child. -/
def admitEffects (ctx : StepContext) (pin : Pin) (job : Job) (e : Effects) : Except Refusal Job :=
  let fresh := freshSpawns job e.spawns
  match (fresh.filterMap (spawnRefusal ctx pin job)).head? with
  | some r => .error r
  | none =>
    if !fresh.isEmpty && !panelAdmitted ctx pin job fresh.length then .error .policyDenied
    else
      let spawned := addSpawns job fresh
      if !messagesAdmitted (ctx.live.meet pin.ceilingAtPin).maxMessagesPerSlot
          (effectiveTargets ctx pin) job e.messages then
        .error .policyDenied
      else if !interruptsAdmitted spawned e.interrupts then .error .policyDenied
      else if !e.messages.isEmpty &&
          !decide (liveCount (addSpawns (addMessages job e.messages) fresh) ≤
            (ctx.live.meet pin.ceilingAtPin).maxLive) then
        .error .policyDenied
      else if stalled job fresh e.messages then .error .stalled
      else
        let admitted := addSpawns (addMessages job e.messages) fresh
        .ok { admitted with observedTerminal := terminalCount admitted }

/-- Rules 10 and 11 for a job's terminal: admitted only with no live child,
so the guest cannot leave children running behind it, and `control` belongs
to non-job calls only. -/
def endRow (job : Job) (s : RowState) (control : List (DocId × ControlKind)) :
    Except Refusal Job :=
  if liveCount job == 0 && control.isEmpty then .ok { job with state := s } else .error .policyDenied

def admitJobOutput (ctx : StepContext) (pin : Pin) (job : Job) : StepOutput → Except Refusal Job
  | .effects e => admitEffects ctx pin job e
  | .result _ control _ _ => endRow job .completed control
  | .fail _ => endRow job .failed []
  | .suspend .. => endRow job .completed []

def controlAuthorized (ctx : StepContext) (row : DocId) : Bool :=
  match ctx.controlTarget row with
  | some (owner, samePlugin) => Background.ProcessControl.authorized ctx.callerScope owner && samePlugin
  | none => false

/-- A pinless step 0 is never adopted and admits nothing; rule 11 checks the
rows its `control` names. `effects` without a pin is refused: only a pin adopts
the row, so nothing would run its spawns, messages or interrupts. -/
def admitNonJob (ctx : StepContext) (job : Job) : StepOutput → Except Refusal Job
  | .result _ control _ _ =>
    if control.all fun c => controlAuthorized ctx c.1 then .ok job else .error .policyDenied
  | .fail _ => .ok job
  | .suspend .. => .ok job
  | .effects _ => .error .policyDenied

/-- The workflow verbs that run script code: `run`, the `check` smoke run and
`import`, which checks the file it binds. -/
def scriptVerbs : List String := ["run", "check", "import"]

/-- A call runs dynamic script code when a script verb carries its source with
the call: an inline `script` or a `path`. A pinless call presents no source
digest, so the host treats a carried source as dynamic. A named run of an
installed script and the workflow control, status, result and help verbs
carry neither. -/
def CallArgs.runsDynamicScript (c : CallArgs) : Bool :=
  c.argv.head?.any scriptVerbs.contains && (c.script.isSome || c.path.isSome)

/-- A pinless call runs dynamic code when it carries a dynamic source
(`CallArgs.runsDynamicScript`) or when a script verb names a script whose
resolved origin is dynamic (`StepContext.namedOrigin`). -/
def runsDynamicCode (ctx : StepContext) (c : CallArgs) : Bool :=
  c.runsDynamicScript ||
    (c.argv.head?.any scriptVerbs.contains && decide (ctx.namedOrigin = some .dynamic))

/-- `policy.dynamic` for a dynamic-origin script: `off` admits no step of it
(`admitStep` also refuses under `off` a pinless call that runs dynamic code, `runsDynamicCode`),
`check` admits it but no fresh spawn (a re-served key is a no-op) and no message (it would start an agent request in a child session), and `run` admits spawns within
`dynamicTargets`, which `effectiveTargets` applies. An installed-origin script
is not gated by the mode. `admitStep` applies it to the pin's origin, so the host refuses with
`plugin_job_policy_denied`; the engine's pre-check only gives a friendlier
message. -/
def dynamicModeRefusal (p : Policy) (origin : Origin) (job : Job) (out : StepOutput) :
    Option Refusal :=
  match origin, p.mode, out with
  | .installed, _, _ => none
  | .dynamic, .off, _ => some .policyDenied
  | .dynamic, .check, .effects e =>
    if (freshSpawns job e.spawns).isEmpty && e.messages.isEmpty then none else some .policyDenied
  | .dynamic, _, _ => none

/-- Rules 1-11 in order. A step-0 output with a pin is adopted under the
host's pin; a step-0 pinned `result` or `suspend` is adopted and ends the row
in the same transaction. Under the empty ceiling (an absent job block) a new
pin is refused with `notAvailable` and a pinless call still runs. A step-0 `resume` is admitted only if `resumeValid`
accepts it (`resumeAdmitted`), and its effects are admitted against the resumed
chain row (`stepRow`). Under `policy.mode = off` a dynamic-origin pin is
refused with `policyDenied` by `dynamicModeRefusal`, and a pinless step 0
here, before `admitNonJob`, when its call runs dynamic code
(`runsDynamicCode`): it carries a `script` or a `path`, or a script verb names
a script whose resolved origin is dynamic. Any other pinless call is not
refused by the mode. A step whose pin has installed origin is refused with
`policyDenied` once the step-0 rules pass when `installedTrusted` no longer
holds for its source digest. -/
def admitStep (ctx : StepContext) (job : Job) (out : StepOutput) : Except Refusal Job :=
  if job.state ≠ .running then .error .terminal
  else if !ctx.available || !ctx.engineProvenanceOperator then .error .notAvailable
  else if engineChanged ctx job out then .error .engineChanged
  else if out.resume.isSome && !resumePlaced job out then .error .policyDenied
  else if !resumeAdmitted ctx out then .error .policyDenied
  else
    match stepPin ctx job out with
    | none =>
      if ctx.policy.mode = .off ∧ runsDynamicCode ctx job.call = true then .error .policyDenied
      else admitNonJob ctx job out
    | some pin =>
      if (dynamicModeRefusal ctx.policy pin.origin (stepRow ctx job out) out).isSome then
        .error .policyDenied
      else if job.pin.isNone && ctx.live = Ceiling.empty then .error .notAvailable
      else if job.pin.isNone && (Background.admitBackground ctx.liveBackground).isNone then
        .error .backgroundBudgetExceeded
      else if job.pin.isNone && ctx.startedByJob then .error .depthExceeded
      else if pin.origin = .installed ∧ ctx.installedTrusted pin.sourceDigest = false then
        .error .policyDenied
      else admitJobOutput ctx pin { stepRow ctx job out with pin := some pin, consecutiveFailures := 0 } out

/-- An admitted step replaces the row. `.terminal` changes nothing. Every
other refusal ends the row `failed` and runs `Control.kill` over its own
children; the kill's actions do not depend on which control ends the row. -/
def settleStep (ctx : StepContext) (job : Job) (out : StepOutput)
    (obs : SpawnKey → Recovery.KillObservation) : Job × List (SpawnKey × Recovery.KillAction) :=
  match admitStep ctx job out with
  | .ok j => (j, [])
  | .error .terminal => (job, [])
  | .error _ => ((terminalizeRow job .failed).1, (Control.kill job .stop obs).2)

theorem refusal_code_spellings :
    Refusal.code .terminal = none ∧
      [Refusal.stalled, .policyDenied, .notAvailable, .engineChanged, .timedOut, .depthExceeded,
          .stepFailed, .backgroundBudgetExceeded].map Refusal.code =
        [some "plugin_job_stalled", some "plugin_job_policy_denied", some "plugin_job_not_available",
          some "plugin_job_engine_changed", some "plugin_job_timed_out",
          some "plugin_job_depth_exceeded", some "plugin_job_step_failed",
          some "background_tool_budget_exceeded"] ∧
      ∀ r : Refusal, r.code = none ↔ r = .terminal := by
  refine ⟨rfl, rfl, ?_⟩
  intro r
  cases r <;> simp [Refusal.code]

theorem scope_meet_permits_iff {K V : Type} [DecidableEq K] (vm : ValueMeet V)
    (a b : EndpointScope K V) (k : K) :
    (a.meet vm b).permits k ↔ a.permits k ∧ b.permits k := by
  cases a <;> cases b <;> simp [EndpointScope.meet, EndpointScope.permits, Finset.mem_inter]

theorem scopeLe_permits (a b : EndpointScope Target Unit) (t : Target) (h : scopeLe a b = true)
    (ha : a.permits t) : b.permits t := by
  cases a <;> cases b <;> simp_all [scopeLe, EndpointScope.permits]
  exact h ha

theorem effectiveTargets_permits_iff (ctx : StepContext) (p : Pin) (t : Target) :
    (effectiveTargets ctx p).permits t ↔
      ctx.live.targets.permits t ∧ p.ceilingAtPin.targets.permits t ∧ ctx.jobTargets.permits t ∧
        ctx.callerTargets.permits t ∧ (p.origin = .dynamic → ctx.policy.dynamicTargets.permits t) := by
  unfold effectiveTargets Ceiling.meet
  by_cases ho : p.origin = .dynamic <;> simp [ho, scope_meet_permits_iff, and_assoc]

/-- Retry-key lookup finds exactly the children with that key. -/
theorem known_iff (job : Job) (k : SpawnKey) :
    known job k = true ↔ ∃ ch ∈ job.children, ch.key = k := by
  simp only [known, List.any_eq_true, beq_iff_eq]
  exact ⟨fun ⟨ch, hm, he⟩ => ⟨ch, hm, retryKey_injective he⟩,
    fun ⟨ch, hm, he⟩ => ⟨ch, hm, he ▸ rfl⟩⟩

private theorem firstOfKey_append_repeat (key : Spawn → String) (d : Spawn) :
    ∀ (seen : List String) (l : List Spawn),
      (key d ∈ seen ∨ ∃ s ∈ l, key s = key d) →
      firstOfKey key seen (l ++ [d]) = firstOfKey key seen l := by
  intro seen l
  induction l generalizing seen with
  | nil =>
    rintro (h | ⟨s, hs, -⟩)
    · simp [firstOfKey, h]
    · simp at hs
  | cons s rest ih =>
    rintro (h | ⟨s0, hs0, hk⟩)
    · by_cases hs : key s ∈ seen
      · simp only [List.cons_append, firstOfKey, hs, if_true]
        exact ih seen (Or.inl h)
      · simp only [List.cons_append, firstOfKey, hs, if_false]
        rw [ih _ (Or.inl (List.mem_cons_of_mem _ h))]
    · by_cases hs : key s ∈ seen
      · simp only [List.cons_append, firstOfKey, hs, if_true]
        rcases List.mem_cons.mp hs0 with rfl | hr
        · exact ih seen (Or.inl (hk ▸ hs))
        · exact ih seen (Or.inr ⟨s0, hr, hk⟩)
      · simp only [List.cons_append, firstOfKey, hs, if_false]
        rcases List.mem_cons.mp hs0 with rfl | hr
        · rw [ih _ (Or.inl (hk ▸ List.mem_cons_self _ _))]
        · rw [ih _ (Or.inr ⟨s0, hr, hk⟩)]

private theorem firstOfKey_spec (key : Spawn → String) :
    ∀ (seen : List String) (l : List Spawn),
      ((firstOfKey key seen l).map key).Nodup ∧ ∀ s ∈ firstOfKey key seen l, key s ∉ seen := by
  intro seen l
  induction l generalizing seen with
  | nil => simp [firstOfKey]
  | cons s rest ih =>
    by_cases hs : key s ∈ seen
    · simp only [firstOfKey, hs, if_true]
      exact ih seen
    · simp only [firstOfKey, hs, if_false]
      obtain ⟨hnd, hnot⟩ := ih (key s :: seen)
      refine ⟨List.nodup_cons.mpr ⟨?_, hnd⟩, ?_⟩
      · simp only [List.mem_map]
        rintro ⟨t, ht, he⟩
        exact hnot t ht (by rw [he]; exact List.mem_cons_self _ _)
      · intro t ht
        rcases List.mem_cons.mp ht with rfl | ht
        · exact hs
        · exact fun hm => hnot t ht (List.mem_cons_of_mem _ hm)

/-- The spawns of a panel that are admitted carry pairwise distinct retry keys,
so each becomes one child and is charged once. -/
theorem fresh_spawns_keys_distinct (job : Job) (spawns : List Spawn) :
    ((freshSpawns job spawns).map fun s => (spawnKey job s).retryKey).Nodup :=
  (firstOfKey_spec _ [] _).1

set_option maxHeartbeats 400000 in
private theorem admitStep_pinned {ctx : StepContext} {job job' : Job} {out : StepOutput} {p : Pin}
    (h : admitStep ctx job out = .ok job') (hp : stepPin ctx job out = some p) :
    admitJobOutput ctx p { stepRow ctx job out with pin := some p, consecutiveFailures := 0 } out =
      .ok job' := by
  unfold admitStep at h
  rw [hp] at h
  dsimp only at h
  split_ifs at h
  exact h

private theorem addMessages_nil (job : Job) : addMessages job [] = job := by
  simp [addMessages]

private theorem liveCount_addSpawns (job : Job) (fresh : List Spawn) :
    liveCount (addSpawns job fresh) = liveCount job + fresh.length := by
  simp [liveCount, liveOwnChildren, addSpawns, List.filter_append, List.filter_map, spawnKey,
    Function.comp_def]

private theorem liveCount_observe (job : Job) (n : Nat) :
    liveCount { job with observedTerminal := n } = liveCount job := rfl

private theorem admitEffects_ok {ctx : StepContext} {p : Pin} {job job' : Job} {e : Effects}
    (h : admitEffects ctx p job e = .ok job') :
    (∀ s ∈ freshSpawns job e.spawns, spawnRefusal ctx p job s = none) ∧
      ((freshSpawns job e.spawns).isEmpty = false →
        panelAdmitted ctx p job (freshSpawns job e.spawns).length = true) ∧
      liveCount job' =
        liveCount (addSpawns (addMessages job e.messages) (freshSpawns job e.spawns)) ∧
      job'.spent = job.spent + (freshSpawns job e.spawns).length ∧
      (∀ s ∈ freshSpawns job e.spawns, ∃ ch ∈ job'.children, ch.key = spawnKey job s) ∧
      (e.messages.isEmpty = false → liveCount job' ≤ (ctx.live.meet p.ceilingAtPin).maxLive) := by
  unfold admitEffects at h
  dsimp only at h
  split at h
  · cases h
  · rename_i hnone
    have hall : ∀ s ∈ freshSpawns job e.spawns, spawnRefusal ctx p job s = none :=
      List.filterMap_eq_nil_iff.mp (List.head?_eq_none_iff.mp hnone)
    split_ifs at h with h1 h2 h3 h4 h5
    cases h
    refine ⟨hall, fun hne => by simpa [hne] using h1, rfl, rfl, fun s hs =>
      ⟨_, List.mem_append_right _ (List.mem_map_of_mem _ hs), rfl⟩,
      fun hne => ?_⟩
    simp only [hne, Bool.not_false, Bool.true_and, Bool.not_eq_true', decide_eq_false_iff_not,
      Bool.not_not, decide_eq_true_eq] at h4
    exact not_not.mp h4

private theorem endRow_ok {j j' : Job} {s : RowState} {c : List (DocId × ControlKind)}
    (h : endRow j s c = .ok j') : liveCount j' = 0 ∧ j'.spent = j.spent := by
  unfold endRow at h
  by_cases hz : (liveCount j == 0 && c.isEmpty) = true
  · rw [if_pos hz] at h
    cases h
    simp only [Bool.and_eq_true, beq_iff_eq] at hz
    exact ⟨hz.1, rfl⟩
  · rw [if_neg hz] at h
    cases h

private theorem freshSpawns_mem_permitted {ctx : StepContext} {job job' : Job} {e : Effects}
    {p : Pin} (h : admitStep ctx job (.effects e) = .ok job')
    (hp : stepPin ctx job (.effects e) = some p) {s : Spawn}
    (hs : s ∈ freshSpawns (stepRow ctx job (.effects e)) e.spawns) :
    (effectiveTargets ctx p).permits s.target := by
  have h1 := (admitEffects_ok (admitStep_pinned h hp)).1 s hs
  unfold spawnRefusal at h1
  by_contra hn
  simp [hn] at h1

/-- All fresh spawns are admitted together and charged once each against the
row the step continues (`stepRow`: the chain's spend and children on a resume),
or the step is refused and the row keeps its children and spend. -/
theorem admit_all_or_nothing (ctx : StepContext) (job : Job) (e : Effects)
    (obs : SpawnKey → Recovery.KillObservation) :
    (∀ job' p, admitStep ctx job (.effects e) = .ok job' → stepPin ctx job (.effects e) = some p →
      job'.spent = (stepRow ctx job (.effects e)).spent +
          (freshSpawns (stepRow ctx job (.effects e)) e.spawns).length ∧
        ∀ s ∈ freshSpawns (stepRow ctx job (.effects e)) e.spawns,
          ∃ ch ∈ job'.children, ch.key = spawnKey (stepRow ctx job (.effects e)) s) ∧
      ∀ r, admitStep ctx job (.effects e) = .error r →
        (settleStep ctx job (.effects e) obs).1.children = job.children ∧
          (settleStep ctx job (.effects e) obs).1.spent = job.spent := by
  refine ⟨fun job' p h hp => ?_, fun r h => ?_⟩
  · obtain ⟨-, -, -, hspent, hmem, -⟩ := admitEffects_ok (admitStep_pinned h hp)
    exact ⟨hspent, hmem⟩
  · unfold settleStep
    rw [h]
    cases r <;> by_cases hs : job.state = .running <;> simp [terminalizeRow, hs]

private theorem stepRow_spawns (ctx : StepContext) (job : Job) (e : Effects) (l : List Spawn) :
    stepRow ctx job (.effects { e with spawns := l }) = stepRow ctx job (.effects e) := rfl

private theorem adoptedPin_spawns (ctx : StepContext) (e : Effects) (l : List Spawn) :
    adoptedPin ctx (.effects { e with spawns := l }) = adoptedPin ctx (.effects e) := rfl

/-- The mode refusal reads only the fresh spawns and the messages. -/
private theorem dynamicModeRefusal_fresh (p : Policy) (o : Origin) (job : Job) (e e' : Effects)
    (h : freshSpawns job e.spawns = freshSpawns job e'.spawns) (hm : e.messages = e'.messages) :
    dynamicModeRefusal p o job (.effects e) = dynamicModeRefusal p o job (.effects e') := by
  unfold dynamicModeRefusal
  generalize p.mode = m
  cases o <;> cases m <;> simp [h, hm]

/-- A spawn whose key a child already holds changes neither the spend nor the
refusal. -/
theorem budget_counts_only_new_keys (ctx : StepContext) (job : Job) (e : Effects) (s : Spawn)
    (h : ∃ ch ∈ (stepRow ctx job (.effects e)).children,
      ch.key = spawnKey (stepRow ctx job (.effects e)) s) :
    admitStep ctx job (.effects { e with spawns := s :: e.spawns }) = admitStep ctx job (.effects e) := by
  have hk := (known_iff (stepRow ctx job (.effects e)) (spawnKey (stepRow ctx job (.effects e)) s)).mpr h
  have hf : ∀ (q : Option Pin) (n : Nat),
      freshSpawns { stepRow ctx job (.effects e) with pin := q, consecutiveFailures := n } (s :: e.spawns) =
        freshSpawns { stepRow ctx job (.effects e) with pin := q, consecutiveFailures := n } e.spawns := by
    intro q n
    simp only [freshSpawns, List.filter_cons]
    rw [if_neg]
    simpa [known, spawnKey] using hk
  have hmr : ∀ o, dynamicModeRefusal ctx.policy o (stepRow ctx job (.effects e))
      (.effects { e with spawns := s :: e.spawns }) =
      dynamicModeRefusal ctx.policy o (stepRow ctx job (.effects e)) (.effects e) := fun o =>
    dynamicModeRefusal_fresh _ o _ _ _ (hf _ _) rfl
  simp only [admitStep, admitJobOutput, admitEffects, admitNonJob, stepPin, StepOutput.pin,
    StepOutput.resume, engineChanged, resumePlaced, resumeAdmitted, stepRow_spawns, adoptedPin_spawns, hf, hmr]

private theorem freshSpawns_append_repeat (J : Job) (spawns : List Spawn) (d : Spawn)
    (h : ∃ s ∈ spawns, spawnKey J s = spawnKey J d) :
    freshSpawns J (spawns ++ [d]) = freshSpawns J spawns := by
  obtain ⟨s0, hs0, hk⟩ := h
  simp only [freshSpawns, List.filter_append]
  by_cases hkn : known J (spawnKey J d) = true
  · simp [hkn]
  · have hd : List.filter (fun s => !known J (spawnKey J s)) [d] = [d] := by simp [hkn]
    rw [hd]
    apply firstOfKey_append_repeat
    refine Or.inr ⟨s0, List.mem_filter.mpr ⟨hs0, ?_⟩, by rw [hk]⟩
    have : known J (spawnKey J s0) = false := by
      rw [hk]; simpa using hkn
    simp [this]

/-- A spawn whose key an earlier spawn of the same panel already holds changes
neither the spend nor the refusal. -/
theorem budget_ignores_repeated_key (ctx : StepContext) (job : Job) (e : Effects) (d : Spawn)
    (h : ∃ s ∈ e.spawns, spawnKey (stepRow ctx job (.effects e)) s =
      spawnKey (stepRow ctx job (.effects e)) d) :
    admitStep ctx job (.effects { e with spawns := e.spawns ++ [d] }) = admitStep ctx job (.effects e) := by
  have hf : ∀ (q : Option Pin) (n : Nat),
      freshSpawns { stepRow ctx job (.effects e) with pin := q, consecutiveFailures := n } (e.spawns ++ [d]) =
        freshSpawns { stepRow ctx job (.effects e) with pin := q, consecutiveFailures := n } e.spawns :=
    fun q n => freshSpawns_append_repeat _ _ _ h
  have hmr : ∀ o, dynamicModeRefusal ctx.policy o (stepRow ctx job (.effects e))
      (.effects { e with spawns := e.spawns ++ [d] }) =
      dynamicModeRefusal ctx.policy o (stepRow ctx job (.effects e)) (.effects e) := fun o =>
    dynamicModeRefusal_fresh _ o _ _ _ (freshSpawns_append_repeat _ _ _ h) rfl
  simp only [admitStep, admitJobOutput, admitEffects, admitNonJob, stepPin, StepOutput.pin,
    StepOutput.resume, engineChanged, resumePlaced, resumeAdmitted, stepRow_spawns, adoptedPin_spawns, hf, hmr]

/-- An admitted spawn is permitted by the live, pinned, job and caller
ceilings. -/
theorem admitted_targets_within_meet {ctx : StepContext} {job job' : Job} {e : Effects} {p : Pin}
    (h : admitStep ctx job (.effects e) = .ok job') (hp : stepPin ctx job (.effects e) = some p)
    {s : Spawn} (hs : s ∈ freshSpawns (stepRow ctx job (.effects e)) e.spawns) :
    ctx.live.targets.permits s.target ∧ p.ceilingAtPin.targets.permits s.target ∧
      ctx.jobTargets.permits s.target ∧ ctx.callerTargets.permits s.target := by
  obtain ⟨h1, h2, h3, h4, -⟩ :=
    (effectiveTargets_permits_iff ctx p s.target).mp (freshSpawns_mem_permitted h hp hs)
  exact ⟨h1, h2, h3, h4⟩

/-- Wiring: once the job targets are within the caller's ceiling, an
installed origin spawns exactly the live ∩ pinned ∩ job targets. -/
theorem wiring_makes_job_targets_spawnable (ctx : StepContext) (p : Pin)
    (hw : scopeLe ctx.jobTargets ctx.callerTargets = true) (ho : p.origin = .installed) (t : Target) :
    (effectiveTargets ctx p).permits t ↔
      ((ctx.live.meet p.ceilingAtPin).targets.meet unitVM ctx.jobTargets).permits t := by
  rw [effectiveTargets_permits_iff]
  simp only [Ceiling.meet, scope_meet_permits_iff, ho, reduceCtorEq, false_implies, and_true]
  constructor
  · exact fun ⟨h1, h2, h3, _⟩ => ⟨⟨h1, h2⟩, h3⟩
  · exact fun ⟨⟨h1, h2⟩, h3⟩ => ⟨h1, h2, h3, scopeLe_permits _ _ _ hw h3⟩

theorem dynamic_origin_targets_within_dynamic_ceiling {ctx : StepContext} {job job' : Job}
    {e : Effects} {p : Pin} (h : admitStep ctx job (.effects e) = .ok job')
    (hp : stepPin ctx job (.effects e) = some p) (ho : p.origin = .dynamic) {s : Spawn}
    (hs : s ∈ freshSpawns (stepRow ctx job (.effects e)) e.spawns) :
    ctx.policy.dynamicTargets.permits s.target :=
  ((effectiveTargets_permits_iff ctx p s.target).mp (freshSpawns_mem_permitted h hp hs)).2.2.2.2 ho

/-- An absent job block is `Ceiling.empty`, which admits no spawn. -/
theorem absent_job_block_is_empty_ceiling {ctx : StepContext} {job job' : Job} {e : Effects}
    {p : Pin} (h_live : ctx.live = Ceiling.empty) (h : admitStep ctx job (.effects e) = .ok job')
    (hp : stepPin ctx job (.effects e) = some p) :
    freshSpawns (stepRow ctx job (.effects e)) e.spawns = [] := by
  refine List.eq_nil_iff_forall_not_mem.mpr fun s hs => ?_
  have := (admitted_targets_within_meet h hp hs).1
  rw [h_live] at this
  exact this

set_option maxHeartbeats 400000 in
/-- Under the empty ceiling no step-0 pin is adopted, so a pinned step-0
`result`, `suspend` or `effects` is refused, while a pinless call is admitted
exactly as under any other ceiling. -/
theorem absent_job_block_adopts_no_pin {ctx : StepContext} {job : Job} {out : StepOutput}
    (h_live : ctx.live = Ceiling.empty) (hj : job.pin = none) {p : Pin}
    (hp : stepPin ctx job out = some p) {j : Job} : admitStep ctx job out ≠ .ok j := by
  intro h
  unfold admitStep at h
  rw [hp] at h
  dsimp only at h
  split_ifs at h
  simp [hj, h_live] at *

theorem absent_job_block_keeps_pinless_admission {ctx : StepContext} {job : Job} {out : StepOutput}
    (hj : job.pin = none) (ho : out.pin = none) :
    admitStep { ctx with live := Ceiling.empty } job out = admitStep ctx job out := by
  have hn : ∀ o, admitNonJob { ctx with live := Ceiling.empty } job o = admitNonJob ctx job o :=
    fun o => by cases o <;> rfl
  have he : engineChanged { ctx with live := Ceiling.empty } job out = engineChanged ctx job out := rfl
  have hr : resumeAdmitted { ctx with live := Ceiling.empty } out = resumeAdmitted ctx out := rfl
  unfold admitStep
  simp only [stepPin, hj, ho, Option.map, hn, he, hr]
  rfl

/-- A narrower live ceiling permits a subset of the targets, and a removed
tool, plugin, job block or install record fails the running job closed and
kills its own children. -/
theorem narrowing_applies_to_running_jobs (ctx : StepContext) (l : Ceiling) (hl : l ≤ ctx.live)
    (p : Pin) (t : Target) (job : Job) (out : StepOutput)
    (obs : SpawnKey → Recovery.KillObservation) :
    ((effectiveTargets { ctx with live := l } p).permits t → (effectiveTargets ctx p).permits t) ∧
      (job.state = .running → ctx.available = false →
        admitStep ctx job out = .error .notAvailable ∧
          settleStep ctx job out obs = ((terminalizeRow job .failed).1, (Control.kill job .stop obs).2)) := by
  refine ⟨fun ht => ?_, fun hr ha => ?_⟩
  · change l.leB ctx.live = true at hl
    simp only [Ceiling.leB, Bool.and_eq_true] at hl
    rw [effectiveTargets_permits_iff] at ht ⊢
    obtain ⟨h1, h2⟩ := ht
    exact ⟨scopeLe_permits _ _ _ hl.1.1.1.1.1 h1, h2⟩
  · have hn : admitStep ctx job out = .error .notAvailable := by
      simp [admitStep, hr, ha]
    exact ⟨hn, by simp [settleStep, hn]⟩

theorem live_le_maxLive {ctx : StepContext} {job job' : Job} {out : StepOutput} {p : Pin}
    (h : admitStep ctx job out = .ok job') (hp : stepPin ctx job out = some p)
    (hinv : liveCount (stepRow ctx job out) ≤ (ctx.live.meet p.ceilingAtPin).maxLive) :
    liveCount job' ≤ (ctx.live.meet p.ceilingAtPin).maxLive := by
  have h := admitStep_pinned h hp
  cases out with
  | effects e =>
    obtain ⟨-, hpanel, hlive, -, -, hmsg⟩ := admitEffects_ok h
    by_cases hm : e.messages.isEmpty = false
    · exact hmsg hm
    have hnil : e.messages = [] := List.isEmpty_iff.mp (by simpa using hm)
    rw [hlive, hnil, addMessages_nil, liveCount_addSpawns]
    cases hfr : (freshSpawns { stepRow ctx job (.effects e) with pin := some p, consecutiveFailures := 0 } e.spawns).isEmpty
    · have := hpanel hfr
      simp only [panelAdmitted, Bool.and_eq_true, decide_eq_true_eq] at this
      exact this.2
    · rw [List.isEmpty_iff.mp hfr, List.length_nil, Nat.add_zero]
      exact hinv
  | result x c g r =>
    simp only [admitJobOutput] at h
    have := (endRow_ok h).1
    omega
  | fail x =>
    simp only [admitJobOutput] at h
    have := (endRow_ok h).1
    omega
  | suspend k r x g rs =>
    simp only [admitJobOutput] at h
    have := (endRow_ok h).1
    omega

theorem spawned_le_budget {ctx : StepContext} {job job' : Job} {out : StepOutput} {p : Pin}
    (h : admitStep ctx job out = .ok job') (hp : stepPin ctx job out = some p)
    (hinv : (stepRow ctx job out).spent ≤ Nat.min p.agentBudget (ctx.live.meet p.ceilingAtPin).maxSpawns) :
    job'.spent ≤ Nat.min p.agentBudget (ctx.live.meet p.ceilingAtPin).maxSpawns := by
  have h := admitStep_pinned h hp
  cases out with
  | effects e =>
    obtain ⟨-, hpanel, -, hspent, -, -⟩ := admitEffects_ok h
    rw [hspent]
    cases hfr : (freshSpawns { stepRow ctx job (.effects e) with pin := some p, consecutiveFailures := 0 } e.spawns).isEmpty
    · have := hpanel hfr
      simp only [panelAdmitted, Bool.and_eq_true, decide_eq_true_eq] at this
      exact this.1
    · rw [List.isEmpty_iff.mp hfr, List.length_nil, Nat.add_zero]
      exact hinv
  | result x c g r =>
    simp only [admitJobOutput] at h
    rw [(endRow_ok h).2]
    exact hinv
  | fail x =>
    simp only [admitJobOutput] at h
    rw [(endRow_ok h).2]
    exact hinv
  | suspend k r x g rs =>
    simp only [admitJobOutput] at h
    rw [(endRow_ok h).2]
    exact hinv

private theorem liveCount_adopt (job : Job) (q : Option Pin) (n : Nat) (s : RowState) :
    liveCount { job with pin := q, consecutiveFailures := n, state := s } = liveCount job := rfl

private theorem spawnRefusal_eq_none {ctx : StepContext} {p : Pin} {job : Job} {s : Spawn}
    (h : spawnRefusal ctx p job s = none) :
    (effectiveTargets ctx p).permits s.target ∧
      CausalHop.admitHop ctx.maxHop
        (CausalHop.nextHop (.crossSession job.callerHop) (ctx.targetHop s.target)) = true ∧
      cwdAdmitted ctx p s.cwd = true ∧ s.slot.utf8ByteSize ≤ maxSlotBytes ∧
      attemptAdmitted job s = true := by
  unfold spawnRefusal at h
  split_ifs at h with h1 h2 h3 h4 h5
  exact ⟨h1, by simpa using h2, by simpa using h3, Nat.le_of_not_lt h4, by simpa using h5⟩

private theorem admitted_spawn_checks {ctx : StepContext} {job job' : Job} {e : Effects} {p : Pin}
    (h : admitStep ctx job (.effects e) = .ok job') (hp : stepPin ctx job (.effects e) = some p)
    {s : Spawn} (hs : s ∈ freshSpawns (stepRow ctx job (.effects e)) e.spawns) :
    (effectiveTargets ctx p).permits s.target ∧
      CausalHop.admitHop ctx.maxHop
        (CausalHop.nextHop (.crossSession job.callerHop) (ctx.targetHop s.target)) = true ∧
      cwdAdmitted ctx p s.cwd = true ∧ s.slot.utf8ByteSize ≤ maxSlotBytes ∧
      attemptAdmitted (stepRow ctx job (.effects e)) s = true := by
  have := spawnRefusal_eq_none ((admitEffects_ok (admitStep_pinned h hp)).1 s hs)
  rwa [stepRow_callerHop] at this

/-- A slot opens at attempt 0 and is re-attempted only as the next attempt after
a latest attempt that ended interrupted: `reattempt`'s rule, on the admission path. -/
theorem admitted_respawn_only_interrupted_slot {ctx : StepContext} {job job' : Job} {e : Effects}
    {p : Pin} (h : admitStep ctx job (.effects e) = .ok job')
    (hp : stepPin ctx job (.effects e) = some p) {s : Spawn}
    (hs : s ∈ freshSpawns (stepRow ctx job (.effects e)) e.spawns) :
    (latestAttempt (stepRow ctx job (.effects e)) s.slot = none ∧ s.attempt = 0) ∨
      ∃ ch, latestAttempt (stepRow ctx job (.effects e)) s.slot = some ch ∧
        ch.state = .interrupted ∧ s.attempt = ch.key.attempt + 1 := by
  have h5 := (admitted_spawn_checks h hp hs).2.2.2.2
  cases hl : latestAttempt (stepRow ctx job (.effects e)) s.slot with
  | none => simp_all [attemptAdmitted]
  | some ch => simp_all [attemptAdmitted]

private theorem admitEffects_ok_rules {ctx : StepContext} {p : Pin} {job job' : Job} {e : Effects}
    (h : admitEffects ctx p job e = .ok job') :
    messagesAdmitted (ctx.live.meet p.ceilingAtPin).maxMessagesPerSlot (effectiveTargets ctx p)
        job e.messages = true ∧
      stalled job (freshSpawns job e.spawns) e.messages = false := by
  unfold admitEffects at h
  dsimp only at h
  split at h
  · cases h
  · split_ifs at h with h1 h2 h3 h4 h5
    exact ⟨by simpa using h2, by simpa using h5⟩

private theorem endRow_ok_live {j j' : Job} {s : RowState} {c : List (DocId × ControlKind)}
    (h : endRow j s c = .ok j') : liveCount j = 0 ∧ c = [] := by
  unfold endRow at h
  by_cases hz : (liveCount j == 0 && c.isEmpty) = true
  · simpa only [Bool.and_eq_true, beq_iff_eq, List.isEmpty_iff] using hz
  · rw [if_neg hz] at h
    cases h

/-- A child's hop is strictly further than the job call's, and it is the hop
its session-message write carries. -/
theorem child_hop_outward (job : Job) (own : Nat) :
    job.callerHop + 1 ≤ CausalHop.nextHop (.crossSession job.callerHop) own ∧
      (DurableLineage.jobSpawnWrite job.callerHop own).lineage.requestHop =
        CausalHop.nextHop (.crossSession job.callerHop) own :=
  ⟨CausalHop.cross_session_exceeds_cause _ _, rfl⟩

/-- An admitted child's hop is within the target node's bound. -/
theorem child_admissible_hop_within_configured_bound {ctx : StepContext} {job job' : Job}
    {e : Effects} {p : Pin} {bound : Nat}
    (hbound : ctx.maxHop = some bound)
    (h : admitStep ctx job (.effects e) = .ok job') (hp : stepPin ctx job (.effects e) = some p)
    {s : Spawn} (hs : s ∈ freshSpawns (stepRow ctx job (.effects e)) e.spawns) :
    CausalHop.nextHop (.crossSession job.callerHop) (ctx.targetHop s.target) ≤ bound := by
  simpa [CausalHop.admitHop, hbound] using (admitted_spawn_checks h hp hs).2.1

/-- The child write is the `agent_new` session-message request naming the job
row's tool call, so the child's completion settles that call.
`child_admission_is_localSelf_or_peer`: the child's admission record comes from
`build_session_message_request` (`lifecycle/materialize.rs`), local self or peer
by the target node, so `RequestAdmission` is unchanged. -/
theorem child_edge_names_job_row (job : Job) (own : Nat) :
    DurableLineage.jobSpawnWrite job.callerHop own =
        DurableLineage.sessionMessageWrite .request job.callerHop own ∧
      DurableLineage.admissible (DurableLineage.jobSpawnWrite job.callerHop own).lineage = true ∧
      (DurableLineage.jobSpawnWrite job.callerHop own).lineage.hasParentToolCallDocId = true :=
  ⟨rfl, DurableLineage.session_message_write_names_its_caller .request _ _⟩

/-- An admitted child cwd is within the pinned caller cwd and the host's cwd
policy. A cwd that policy rejects never passes as a spawn, as it never passes
as request input. -/
theorem child_cwd_within_caller_policy (ctx : StepContext) (p : Pin) (job : Job) (e : Effects)
    (s : Spawn) (c : String) (hc : s.cwd = some c) :
    (∀ job', admitStep ctx job (.effects e) = .ok job' → stepPin ctx job (.effects e) = some p →
      s ∈ freshSpawns (stepRow ctx job (.effects e)) e.spawns →
        ctx.within p.cwd c = true ∧ ctx.cwdAllowed c = true) ∧
      (ctx.cwdAllowed c = false → spawnRefusal ctx p job s ≠ none ∧
        ∀ (input : Enrollment.RequestInput) skills queueAllowed, input.cwd = some c →
          Enrollment.inputWithinContext input skills ctx.cwdAllowed queueAllowed = false) := by
  refine ⟨fun job' h hp hs => ?_, fun hdeny => ⟨fun hn => ?_, fun input skills q hi =>
    Enrollment.rejected_cwd_cannot_override_host_policy input c skills ctx.cwdAllowed q hi hdeny⟩⟩
  · have hcwd := (admitted_spawn_checks h hp hs).2.2.1
    rw [hc] at hcwd
    simpa only [cwdAdmitted, Bool.and_eq_true] using hcwd
  · have hcwd := (spawnRefusal_eq_none hn).2.2.1
    rw [hc] at hcwd
    simp [cwdAdmitted, hdeny] at hcwd

/-- Each message reaches its slot's latest attempt in this chain as the step
began, never an attempt the same panel spawns, and keeps it within the message
cap; messages are not charged to the spawn budget. -/
theorem messages_bounded_and_uncharged {ctx : StepContext} {job job' : Job} {e : Effects} {p : Pin}
    (h : admitStep ctx job (.effects e) = .ok job') (hp : stepPin ctx job (.effects e) = some p) :
    (∀ slot ∈ e.messages, ∃ ch,
        latestAttempt (stepRow ctx job (.effects e)) slot = some ch ∧
          ch.messages + e.messages.count slot ≤ (ctx.live.meet p.ceilingAtPin).maxMessagesPerSlot) ∧
      job'.spent = (stepRow ctx job (.effects e)).spent +
        (freshSpawns (stepRow ctx job (.effects e)) e.spawns).length := by
  have h' := admitStep_pinned h hp
  refine ⟨fun slot hslot => ?_, (admitEffects_ok h').2.2.2.1⟩
  have hm : messagesAdmitted (ctx.live.meet p.ceilingAtPin).maxMessagesPerSlot
      (effectiveTargets ctx p) (stepRow ctx job (.effects e)) e.messages = true := (admitEffects_ok_rules h').1
  simp only [messagesAdmitted, List.all_eq_true] at hm
  have hs := hm slot hslot
  cases hl : latestAttempt (stepRow ctx job (.effects e)) slot with
  | none => simp [hl] at hs
  | some ch =>
    simp only [hl, decide_eq_true_eq] at hs
    exact ⟨ch, rfl, hs.2⟩

/-- A message re-lives its slot's latest attempt, a new request into that
attempt's target, so it is admitted only while the effective target ceiling
(live, pinned, job, caller and dynamic scopes) still permits that target; a
target narrowed away from the job fails the step closed. -/
theorem messages_within_effective_targets {ctx : StepContext} {p : Pin} {job job' : Job}
    {e : Effects} (h : admitEffects ctx p job e = .ok job') :
    ∀ slot ∈ e.messages, ∃ ch, latestAttempt job slot = some ch ∧
      (effectiveTargets ctx p).permits ch.target := by
  intro slot hslot
  have hm := (admitEffects_ok_rules h).1
  simp only [messagesAdmitted, List.all_eq_true] at hm
  have hs := hm slot hslot
  cases hl : latestAttempt job slot with
  | none => simp [hl] at hs
  | some ch =>
    simp only [hl, decide_eq_true_eq] at hs
    exact ⟨ch, rfl, hs.1⟩

/-- Adoption takes one background slot of the calling request; a full budget
returns a capacity refusal, while a pinned row's
steps, and so its children, neither read nor take one. -/
theorem job_row_consumes_one_background_slot (ctx : StepContext) (job job' : Job)
    (out : StepOutput) (p : Pin) (m : Nat) :
    (job.pin = none → admitStep ctx job out = .ok job' → stepPin ctx job out = some p →
      Background.admitBackground ctx.liveBackground = some (ctx.liveBackground + 1)) ∧
      (job.pin = some p →
        admitStep { ctx with liveBackground := m } job out = admitStep ctx job out) := by
  refine ⟨fun hpin h hp => ?_, fun hpin => ?_⟩
  · have hb : (Background.admitBackground ctx.liveBackground).isNone = false := by
      by_contra hnone
      simp only [Bool.not_eq_false] at hnone
      unfold admitStep at h
      rw [hp] at h
      simp only [hpin, Option.isNone_none, Bool.true_and, Bool.and_true] at h
      simp only [if_pos hnone] at h
      cases h
    unfold Background.admitBackground at hb ⊢
    split <;> simp_all
  · unfold admitStep
    simp only [stepPin, hpin, Option.isNone_some, Bool.false_and, Bool.false_eq_true, if_false]
    rfl

/-- A step-0 output with no pin is a non-job call: it is never adopted, any
output it is admitted with leaves the row unchanged, and it does not read the
background budget. -/
theorem no_effects_without_adoption (ctx : StepContext) (job job' : Job) (out : StepOutput)
    (m : Nat) (hpin : job.pin = none) (hout : out.pin = none) :
    (admitStep ctx job out = .ok job' → job' = job) ∧
      admitStep { ctx with liveBackground := m } job out = admitStep ctx job out := by
  have hs : ∀ c : StepContext, stepPin c job out = none := fun c => by
    simp [stepPin, hpin, hout]
  refine ⟨fun h => ?_, ?_⟩
  · unfold admitStep at h
    rw [hs] at h
    dsimp only at h
    split_ifs at h
    cases out <;> simp only [admitNonJob] at h <;> (try split_ifs at h) <;> cases h <;> rfl
  · unfold admitStep
    rw [hs, hs]
    rfl

/-- A step-0 `effects` output with no pin is refused: only a pin adopts the row,
so its spawns, messages and interrupts have no driver to run them. -/
theorem pinless_effects_refused (ctx : StepContext) (job : Job) (e : Effects)
    (hp : stepPin ctx job (.effects e) = none) :
    ∀ job', admitStep ctx job (.effects e) ≠ .ok job' := by
  intro job' h
  unfold admitStep at h
  rw [hp] at h
  dsimp only at h
  split_ifs at h
  simp [admitNonJob] at h

/-- The host backstop for a dynamic origin, whatever the dynamic mode: a spawn
outside `policy.dynamicTargets` is refused `.policyDenied` and never admitted. -/
theorem dynamic_wider_target_refused (ctx : StepContext) (p : Pin) (job : Job) (s : Spawn)
    (ho : p.origin = .dynamic) (ht : ¬ ctx.policy.dynamicTargets.permits s.target) :
    spawnRefusal ctx p job s = some .policyDenied ∧
      ∀ e job', s ∈ freshSpawns (stepRow ctx job (.effects e)) e.spawns →
        stepPin ctx job (.effects e) = some p →
        admitStep ctx job (.effects e) ≠ .ok job' := by
  refine ⟨?_, fun e job' hs hp h => ht (dynamic_origin_targets_within_dynamic_ceiling h hp ho hs)⟩
  have hn : ¬ (effectiveTargets ctx p).permits s.target := fun h =>
    ht (((effectiveTargets_permits_iff ctx p s.target).mp h).2.2.2.2 ho)
  simp [spawnRefusal, hn]

/-- An admitted `effects` step leaves a live child, an unobserved terminal, a
fresh spawn or a message; one that leaves none of them is `.stalled`. An
interrupt is not new: it ends only a live child, which already keeps the job. -/
theorem stalled_step_refused (ctx : StepContext) (p : Pin) (job : Job) (e : Effects) :
    (∀ job', admitStep ctx job (.effects e) = .ok job' → stepPin ctx job (.effects e) = some p →
      0 < liveCount (stepRow ctx job (.effects e)) ∨
        (stepRow ctx job (.effects e)).observedTerminal < terminalCount (stepRow ctx job (.effects e)) ∨
        freshSpawns (stepRow ctx job (.effects e)) e.spawns ≠ [] ∨ e.messages ≠ []) ∧
      (stalled job (freshSpawns job e.spawns) e.messages = true →
        interruptsAdmitted job e.interrupts = true → admitEffects ctx p job e = .error .stalled) := by
  refine ⟨fun job' h hp => ?_, fun hs hia => ?_⟩
  · have hst : stalled (stepRow ctx job (.effects e)) (freshSpawns (stepRow ctx job (.effects e)) e.spawns)
        e.messages = false :=
      (admitEffects_ok_rules (admitStep_pinned h hp)).2
    by_contra hn
    simp only [not_or, not_lt, ne_eq, not_not, Nat.le_zero] at hn
    obtain ⟨h1, h2, h3, h4⟩ := hn
    simp [stalled, h1, h3, h4, Nat.not_lt.mpr h2] at hst
  · have hf : freshSpawns job e.spawns = [] := by simp_all [stalled]
    have hm : e.messages = [] := by simp_all [stalled]
    rw [hf, hm] at hs
    simp only [admitEffects, hf, hm]
    simp [messagesAdmitted, addSpawns, hia, hs]

/-- A host check, so it holds for any guest: an admitted terminal output
leaves no live child. -/
theorem terminal_requires_no_live_children {ctx : StepContext} {job job' : Job}
    {out : StepOutput} {p : Pin} (h : admitStep ctx job out = .ok job')
    (hp : stepPin ctx job out = some p) (hout : ∀ e, out ≠ .effects e) :
    liveCount (stepRow ctx job out) = 0 := by
  have h := admitStep_pinned h hp
  cases out with
  | effects e => exact absurd rfl (hout e)
  | result x c g r =>
    simp only [admitJobOutput] at h
    exact (endRow_ok_live h).1
  | fail x =>
    simp only [admitJobOutput] at h
    exact (endRow_ok_live h).1
  | suspend k r x g rs =>
    simp only [admitJobOutput] at h
    exact (endRow_ok_live h).1

/-- A panel the budget or live bound cannot hold is `.policyDenied`, and a
`.policyDenied` step ends the row `failed` and kills exactly its live own
children. -/
theorem panel_refusal_fails_closed (ctx : StepContext) (p : Pin) (job : Job) (out : StepOutput)
    (obs : SpawnKey → Recovery.KillObservation) :
    (∀ e : Effects, (∀ s ∈ freshSpawns job e.spawns, spawnRefusal ctx p job s = none) →
      freshSpawns job e.spawns ≠ [] →
        panelAdmitted ctx p job (freshSpawns job e.spawns).length = false →
          admitEffects ctx p job e = .error .policyDenied) ∧
      (admitStep ctx job out = .error .policyDenied →
        settleStep ctx job out obs =
          ({ job with state := .failed },
            (liveOwnChildren job).map fun ch => (ch.key, Recovery.killAction (obs ch.key)))) := by
  refine ⟨fun e hall hne hpanel => ?_, fun h => ?_⟩
  · have hnil : (freshSpawns job e.spawns).filterMap (spawnRefusal ctx p job) = [] :=
      List.filterMap_eq_nil_iff.mpr hall
    simp only [admitEffects, hnil, List.head?_nil, hpanel]
    simp [hne]
  · have hr : job.state = .running := by
      by_contra hn
      simp [admitStep, hn] at h
    simp [settleStep, h, terminalizeRow, Control.kill, Control.ends, hr]

/-- A terminal output with live children means guest and host diverged.
It is never held or admitted with the children left running: it is
`.policyDenied`, the row ends `failed` and kill runs over its own children. -/
theorem terminal_with_live_children_fails_closed (ctx : StepContext) (job : Job)
    (out : StepOutput) (p : Pin) (obs : SpawnKey → Recovery.KillObservation)
    (hr : job.state = .running) (ha : ctx.available = true)
    (ho : ctx.engineProvenanceOperator = true) (hpin : job.pin = some p)
    (he : engineChanged ctx job out = false) (hout : ∀ e, out ≠ .effects e)
    (hlive : 0 < liveCount job) :
    admitStep ctx job out = .error .policyDenied ∧
      settleStep ctx job out obs =
        ({ job with state := .failed },
          (liveOwnChildren job).map fun ch => (ch.key, Recovery.killAction (obs ch.key))) := by
  have hl : (liveCount job == 0) = false := by
    simp only [beq_eq_false_iff_ne, ne_eq]
    omega
  have hd : admitStep ctx job out = .error .policyDenied := by
    cases out with
    | effects e => exact absurd rfl (hout e)
    | result x c g r =>
      cases r <;> simp [admitStep, hr, ha, ho, he, hpin, stepPin, resumePlaced, StepOutput.resume,
        stepRow, admitJobOutput, endRow, liveCount_adopt, hl]
    | fail x =>
      simp [admitStep, hr, ha, ho, he, hpin, stepPin, StepOutput.resume, stepRow, admitJobOutput,
        endRow, liveCount_adopt, hl]
    | suspend k rs x g r =>
      cases r <;> simp [admitStep, hr, ha, ho, he, hpin, stepPin, resumePlaced, StepOutput.resume,
        stepRow, admitJobOutput, endRow, liveCount_adopt, hl]
  exact ⟨hd, (panel_refusal_fails_closed ctx p job out obs).2 hd⟩

/-- Rule 4: a `resume` on a step ≥ 1, or on a `result` or `suspend` without a
`pin`, is `.policyDenied`. -/
theorem resume_only_step0_beside_pin (ctx : StepContext) (job : Job) (out : StepOutput)
    (hr : job.state = .running) (ha : ctx.available = true)
    (ho : ctx.engineProvenanceOperator = true) (he : engineChanged ctx job out = false)
    (hres : out.resume.isSome = true)
    (hplace : job.pin.isSome = true ∨ ((∀ e, out ≠ .effects e) ∧ out.pin = none)) :
    admitStep ctx job out = .error .policyDenied := by
  have hp : resumePlaced job out = false := by
    rcases hplace with hs | ⟨hne, hn⟩
    · cases out <;> cases hj : job.pin <;> simp_all [resumePlaced]
    · cases out with
      | effects e => exact absurd rfl (hne e)
      | _ => simp_all [resumePlaced, StepOutput.pin]
  simp [admitStep, hr, ha, ho, he, hres, hp]

/-- An admitted step-0 `resume` sits beside the pin it adopts and names a
row `resumeValid` accepts under the caller's scope. -/
theorem admitted_resume_valid (ctx : StepContext) (job job' : Job) (out : StepOutput) (r : Resume)
    (h : admitStep ctx job out = .ok job') (hr : out.resume = some r) :
    ∃ g owner samePlugin prior root, out.pin = some g ∧
      ctx.resumeTarget r.rowDoc = some (owner, samePlugin, prior, root) ∧
      resumeValid ctx.callerScope owner samePlugin prior g ctx.resumeFacts r = true := by
  have hA : resumeAdmitted ctx out = true := by
    by_contra hn
    have hf : (!resumeAdmitted ctx out) = true := by simpa using hn
    unfold admitStep at h
    split_ifs at h
  unfold resumeAdmitted at hA
  rw [hr] at hA
  dsimp only at hA
  split at hA
  next g owner samePlugin prior root hg ht => exact ⟨g, owner, samePlugin, prior, root, hg, ht, (Bool.and_eq_true _ _ |>.mp hA).2⟩
  next => cases hA

/-- An admitted step-0 `resume` names the chain head: no earlier resume
continued its prior row. -/
theorem admitted_resume_head (ctx : StepContext) (job job' : Job) (out : StepOutput) (r : Resume)
    (h : admitStep ctx job out = .ok job') (hr : out.resume = some r) :
    ctx.resumeIsHead r.rowDoc = true := by
  have hA : resumeAdmitted ctx out = true := by
    by_contra hn
    have hf : (!resumeAdmitted ctx out) = true := by simpa using hn
    unfold admitStep at h
    split_ifs at h
  unfold resumeAdmitted at hA
  rw [hr] at hA
  dsimp only at hA
  split at hA
  next g owner samePlugin prior root hg ht => exact ((Bool.and_eq_true _ _ |>.mp (Bool.and_eq_true _ _ |>.mp hA).1).1)
  next => cases hA

/-- An admitted step-0 `resume` is adopted as the resume pin: the chain root and the prior row are both pinned, and the adopted
pin is built from the facts `resumeValid` checked, never from `adoptionFacts`. -/
theorem admitted_resume_adopts_resume_pin (ctx : StepContext) (job job' : Job) (out : StepOutput)
    (r : Resume) (h : admitStep ctx job out = .ok job') (hr : out.resume = some r) :
    ∃ g owner samePlugin prior root rootPin priorPin, out.pin = some g ∧
      ctx.resumeTarget r.rowDoc = some (owner, samePlugin, prior, root) ∧
      root.pin = some rootPin ∧ prior.pin = some priorPin ∧
      adoptedPin ctx out g = resumePin rootPin priorPin g ctx.resumeFacts := by
  obtain ⟨g, owner, samePlugin, prior, root, hg, ht, hv⟩ := admitted_resume_valid ctx job job' out r h hr
  have hA : resumeAdmitted ctx out = true := by
    by_contra hn
    have hf : (!resumeAdmitted ctx out) = true := by simpa using hn
    unfold admitStep at h
    split_ifs at h
  have hroot : root.pin.isSome = true := by
    simp only [resumeAdmitted, hr, hg, ht, Bool.and_eq_true] at hA
    exact hA.1.2
  have hprior : prior.pin.isSome = true := by
    simp only [resumeValid, Bool.and_eq_true] at hv
    cases hp : prior.pin <;> simp_all
  obtain ⟨rootPin, hrp⟩ := Option.isSome_iff_exists.mp hroot
  obtain ⟨priorPin, hpp⟩ := Option.isSome_iff_exists.mp hprior
  exact ⟨g, owner, samePlugin, prior, root, rootPin, priorPin, hg, ht, hrp, hpp,
    by simp [adoptedPin, hr, ht, hrp, hpp]⟩

private theorem admitEffects_children {ctx : StepContext} {p : Pin} {job job' : Job} {e : Effects}
    (h : admitEffects ctx p job e = .ok job') :
    ∀ ch ∈ job'.children, (∃ c0 ∈ job.children, ch.key = c0.key) ∨
      ∃ s ∈ freshSpawns job e.spawns, ch.key = spawnKey job s := by
  intro ch hch
  unfold admitEffects at h
  dsimp only at h
  split at h
  · cases h
  · split_ifs at h
    cases h
    simp only [addSpawns, List.mem_append, List.mem_map, addMessages] at hch
    rcases hch with ⟨c1, hc1, rfl⟩ | ⟨s, hs, rfl⟩
    · exact Or.inl ⟨c1, hc1, rfl⟩
    · exact Or.inr ⟨s, hs, rfl⟩

private theorem admitJobOutput_children {ctx : StepContext} {p : Pin} {j j' : Job}
    {out : StepOutput} (h : admitJobOutput ctx p j out = .ok j') :
    ∀ ch ∈ j'.children, (∃ c0 ∈ j.children, ch.key = c0.key) ∨
      ∃ e, out = .effects e ∧ ∃ s ∈ freshSpawns j e.spawns, ch.key = spawnKey j s := by
  intro ch hch
  have hend : ∀ (st : RowState) (c : List (DocId × ControlKind)), endRow j st c = .ok j' →
      ch ∈ j.children := by
    intro st c he
    unfold endRow at he
    split_ifs at he
    cases he
    exact hch
  cases out with
  | effects e =>
    rcases admitEffects_children h ch hch with h1 | ⟨s, hs, hk⟩
    · exact Or.inl h1
    · exact Or.inr ⟨e, rfl, s, hs, hk⟩
  | result x c g r => exact Or.inl ⟨ch, hend _ _ h, rfl⟩
  | fail x => exact Or.inl ⟨ch, hend _ _ h, rfl⟩
  | suspend k rs x g r => exact Or.inl ⟨ch, hend _ _ h, rfl⟩

set_option maxHeartbeats 400000 in
/-- A job gains a child key only when `admitStep` admits it from
the output's `effects.spawns`, or carries it over from the prior row of a
validated `resume`, and a job-bound tool is never a composed child
call (`job_tool_not_composable`). -/
theorem spawn_only_through_admitStep (ctx : StepContext) (job job' : Job) (out : StepOutput)
    (h : admitStep ctx job out = .ok job') :
    (∀ ch ∈ job'.children, (∃ c0 ∈ job.children, ch.key = c0.key) ∨
      (∃ r owner samePlugin prior root, out.resume = some r ∧
        ctx.resumeTarget r.rowDoc = some (owner, samePlugin, prior, root) ∧
        ∃ c0 ∈ prior.children, ch.key = c0.key) ∨
      ∃ e, out = .effects e ∧ ∃ s ∈ freshSpawns (stepRow ctx job out) e.spawns,
        ch.key = spawnKey (stepRow ctx job out) s) ∧
    ∀ selected staticHookOwned admitted,
      ToolPolicy.PluginCalls.childAdmissible selected
        (ToolPolicy.PluginCalls.routeOf staticHookOwned true) admitted = false := by
  refine ⟨fun ch hch => ?_, ToolPolicy.PluginCalls.job_tool_not_composable⟩
  unfold admitStep at h
  rcases hsp : stepPin ctx job out with _ | pin <;> rw [hsp] at h <;> dsimp only at h <;>
    split_ifs at h
  · have hj : job' = job := by
      cases out <;> simp only [admitNonJob] at h <;> (try split_ifs at h) <;> cases h <;> rfl
    subst hj
    exact Or.inl ⟨ch, hch, rfl⟩
  · rcases admitJobOutput_children h ch hch with ⟨c0, hc0, hk⟩ | hfresh
    · rcases stepRow_children hc0 with hj | ⟨r, owner, samePlugin, prior, root, hr, ht, hp⟩
      · exact Or.inl ⟨c0, hj, hk⟩
      · exact Or.inr (Or.inl ⟨r, owner, samePlugin, prior, root, hr, ht, c0, hp, hk⟩)
    · exact Or.inr (Or.inr hfresh)

/-- A non-job `result` naming `control` is never adopted: the row is unchanged.
On a job, `control` is refused. -/
theorem control_output_never_adopts (ctx : StepContext) (job job' : Job) (x : Extras)
    (control : List (DocId × ControlKind)) (g : Option GuestPin) (r : Option Resume) :
    (job.pin = none → g = none → admitStep ctx job (.result x control g r) = .ok job' →
      job' = job) ∧
      (control ≠ [] → ∀ p, stepPin ctx job (.result x control g r) = some p →
        admitStep ctx job (.result x control g r) ≠ .ok job') := by
  refine ⟨fun hpin hg h => ?_, fun hc p hp h => ?_⟩
  · subst hg
    unfold admitStep at h
    rw [show stepPin ctx job (.result x control none r) = none by
      simp [stepPin, hpin, StepOutput.pin]] at h
    dsimp only at h
    split_ifs at h
    simp only [admitNonJob] at h
    split_ifs at h
    cases h
    rfl
  · have h' := admitStep_pinned h hp
    simp only [admitJobOutput] at h'
    exact hc (endRow_ok_live h').2

/-- Rule 11: each row an admitted non-job `control` names exists, belongs to
the same plugin and is within the caller's process-control scope. -/
theorem control_output_requires_scope_and_plugin {ctx : StepContext} {job job' : Job}
    {x : Extras} {control : List (DocId × ControlKind)} {r : Option Resume}
    (hpin : job.pin = none) (h : admitStep ctx job (.result x control none r) = .ok job') :
    ∀ c ∈ control, ∃ owner, ctx.controlTarget c.1 = some (owner, true) ∧
      Background.ProcessControl.authorized ctx.callerScope owner = true := by
  have hall : control.all (fun c => controlAuthorized ctx c.1) = true := by
    unfold admitStep at h
    rw [show stepPin ctx job (.result x control none r) = none by
      simp [stepPin, hpin, StepOutput.pin]] at h
    dsimp only at h
    split_ifs at h
    simp only [admitNonJob] at h
    split_ifs at h with hz
    exact hz
  intro c hc
  have hca := List.all_eq_true.mp hall c hc
  unfold controlAuthorized at hca
  split at hca
  · rename_i owner same heq
    simp only [Bool.and_eq_true] at hca
    exact ⟨owner, by rw [heq, hca.2], hca.1⟩
  · cases hca

/-- Re-served chain keys of a resume step are no-ops: a spawn whose key the
chain's prior children already hold changes neither the spend nor the
refusal, so a replayed script neither charges nor spawns it again. -/
theorem resume_reserved_keys_uncharged (ctx : StepContext) (job : Job) (e : Effects) (s : Spawn)
    (r : Resume) (owner : Background.ProcessControl.Scope) (samePlugin : Bool) (prior root : Job)
    (hr : e.resume = some r) (ht : ctx.resumeTarget r.rowDoc = some (owner, samePlugin, prior, root))
    (h : ∃ ch ∈ prior.children, ch.key = spawnKey root s) :
    admitStep ctx job (.effects { e with spawns := s :: e.spawns }) = admitStep ctx job (.effects e) := by
  refine budget_counts_only_new_keys ctx job e s ?_
  rw [stepRow_resume (out := .effects e) hr ht]
  exact h

end PluginJob
