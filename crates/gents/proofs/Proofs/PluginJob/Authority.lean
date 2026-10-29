import Proofs.ToolPolicy.Configuration
import Proofs.PluginJob.Admission

/-!
# Plugin-job authority: install provenance and script origin

Origin is a host fact decided by the call (`callOrigin`): a call that carries a
`script` runs model-authored code and is `dynamic`; any other call runs the
plugin artifact itself and is `installed`. The artifact's pack must be
operator-installed, or every step is refused. The dynamic policy gates
dynamic-origin scripts, pack-shipped job blocks stay within what the operator
installed, a job-descended caller cannot start a job, and a script reaches
child requests and data only through admission.
-/

namespace PluginJob

open ToolPolicy

/-- The pack kinds that write an install record. -/
inductive PackKind | documents | assets | plugins
  deriving DecidableEq, Repr

/-- The native record is `HomePackInstall.installed_via`, written by every
install path (CLI, desktop, self-configuration) for every kind above. `legacy`
is a record written before that field existed. -/
inductive Record
  | absent
  | legacy
  | written (via : InstallProvenance)
  deriving DecidableEq, Repr

/-- A document pack with desktop UX needs a local home record and explicit
operator installation. A model-authored installation does not authorize UI code. -/
def documentUxAllowed (hasUx hasLocalHome : Bool) (via : InstallProvenance) : Bool :=
  !hasUx || (hasLocalHome && via == .operator)

theorem document_ux_requires_operator_home (hasHome : Bool) (via : InstallProvenance)
    (h : documentUxAllowed true hasHome via = true) :
    hasHome = true ∧ via = .operator := by
  cases hasHome <;> cases via <;> simp_all [documentUxAllowed]

/-- A written record reads as written. A legacy record reads as `operator` for
assets and plugins, because the CLI was their only writer, and as `selfConfig`
for documents, which never had such a record. A missing record reads as
`selfConfig`, so it fails closed. -/
def provenanceOf : PackKind → Record → InstallProvenance
  | _, .written v => v
  | .assets, .legacy => .operator
  | .plugins, .legacy => .operator
  | .documents, .legacy => .selfConfig
  | _, .absent => .selfConfig

/-- An explicit install of the pack itself writes its installer's provenance.
A dependency install never lowers an existing `operator` provenance; otherwise
it writes its installer's. There is one record per coordinate per node, so the
last explicit installer decides; this fails only in the safe direction. -/
def recordAfterInstall (prior : Record) (kind : PackKind) (explicit : Bool)
    (via : InstallProvenance) : Record :=
  if explicit then .written via
  else if provenanceOf kind prior = .operator then .written .operator
  else .written via

/-- The origin of one call: a call that carries a `script`
(`CallArgs.carriesScript`) runs model-authored code and is `dynamic`; any
other call runs the operator-installed artifact the engine guard and
`engineChanged` check, and is `installed`. -/
def callOrigin (c : CallArgs) : Origin :=
  if c.carriesScript then .dynamic else .installed

/-- A missing record, and a legacy documents record, read as `selfConfig`. -/
theorem missing_record_is_self_config (k : PackKind) :
    provenanceOf k .absent = .selfConfig ∧ provenanceOf .documents .legacy = .selfConfig := by
  cases k <;> exact ⟨rfl, rfl⟩

theorem legacy_record_is_operator :
    provenanceOf .assets .legacy = .operator ∧ provenanceOf .plugins .legacy = .operator :=
  ⟨rfl, rfl⟩

theorem dependency_never_lowers_provenance (k : PackKind) (prior : Record)
    (v : InstallProvenance) (h : provenanceOf k prior = .operator) :
    provenanceOf k (recordAfterInstall prior k false v) = .operator := by
  have hw : recordAfterInstall prior k false v = .written .operator := by
    simp [recordAfterInstall, h]
  rw [hw]
  cases k <;> rfl

/-- An explicit install of the pack itself writes its installer's provenance,
so an explicit self-configuration install may lower an operator record. -/
theorem explicit_install_decides (prior : Record) (k : PackKind) (v : InstallProvenance) :
    recordAfterInstall prior k true v = .written v ∧
      provenanceOf k (recordAfterInstall prior k true v) = v := by
  simp [recordAfterInstall, provenanceOf]

/-- An engine pack whose provenance is not `operator` refuses every step of a
running row with `plugin_job_not_available`. -/
theorem self_config_engine_refused (ctx : StepContext) (k : PackKind) (r : Record)
    (hctx : ctx.engineProvenanceOperator = decide (provenanceOf k r = .operator))
    (hp : provenanceOf k r ≠ .operator) (job : Job) (out : StepOutput)
    (hr : job.state = .running) :
    admitStep ctx job out = .error .notAvailable := by
  have he : ctx.engineProvenanceOperator = false := by simp [hctx, hp]
  simp [admitStep, hr, he]

/-! ## Dynamic policy, pack-shipped job blocks and wiring -/

/-- Validation: `dynamicTargets ⊆ agentTargets`, checked only when the mode
is not `off`. The type default of `dynamicTargets` is the empty list. -/
def Policy.valid (agentTargets : List String) (p : Policy) : Bool :=
  p.mode == .off || decide (p.dynamicTargets ⊆ agentTargets)

/-- A job block a pack ships in its Tools document. `targetsFrom` and
`trusted` are pack coordinates. -/
structure PackJobBlock where
  targetsFrom : List String
  trusted : List String
  dynamic : DynamicMode
  deriving DecidableEq, Repr

/-- A pack-shipped block installs only through an operator install; its
targets and trusted packs come from `closure`, the pack and its dependencies;
and it never runs dynamic scripts. -/
def packJobBlockAdmissible (via : InstallProvenance) (closure : List String)
    (b : PackJobBlock) : Bool :=
  decide (via = .operator ∧ b.targetsFrom ⊆ closure ∧ b.trusted ⊆ closure ∧
    b.dynamic ∈ [.off, .check])

/-- `gents pack wire … --targets-from <pack>`: the pack's targets join
the job targets, every job target joins the caller's AgentTools target list,
and the pack joins `trusted_packs`. Content packs ship no job block, so
this operator wiring, not a pack-shipped block, grants their targets.
`trusted_packs` does not decide origin: the call does (`callOrigin`). -/
def wireTargetsFrom (pack : String) (packTargets : List Target) (p : Policy)
    (jobTargets callerTargets : List Target) : Policy × List Target × List Target :=
  let job := jobTargets ∪ packTargets
  ({ p with trustedPacks := p.trustedPacks.insert pack }, job, callerTargets ∪ job)

/-- With a dynamic-origin pin and `policy.mode = off`, `admitStep` refuses
with `policyDenied` once rules 1-4 pass; the mode is checked before the
step-0 rules. -/
theorem dynamic_off_refuses_run (ctx : StepContext) (job : Job) (out : StepOutput) (p : Pin)
    (h : ctx.policy.mode = .off) (hr : job.state = .running)
    (ha : (ctx.available && ctx.engineProvenanceOperator) = true)
    (he : engineChanged ctx job out = false)
    (hres : (out.resume.isSome && !resumePlaced job out) = false)
    (hadm : resumeAdmitted ctx out = true)
    (hp : stepPin ctx job out = some p) (ho : p.origin = .dynamic) :
    admitStep ctx job out = .error .policyDenied := by
  simp only [Bool.and_eq_true] at ha
  simp [admitStep, hr, ha.1, ha.2, he, hres, hadm, hp, dynamicModeRefusal, ho, h]

/-- Under `policy.mode = off`, a step that carries no installed pin (no pin at
all, or a dynamic-origin one) and whose call carries a `script`
(`CallArgs.carriesScript`) is refused with `dynamicCodeDisabled` for a pinless
call, or `policyDenied` for a pinned step, once rules 1-4 pass. -/
theorem dynamic_off_refuses_pinless_step (ctx : StepContext) (job : Job) (out : StepOutput)
    (h : ctx.policy.mode = .off) (hr : job.state = .running)
    (ha : (ctx.available && ctx.engineProvenanceOperator) = true)
    (he : engineChanged ctx job out = false)
    (hres : (out.resume.isSome && !resumePlaced job out) = false)
    (hadm : resumeAdmitted ctx out = true)
    (hp : ∀ p, stepPin ctx job out = some p → p.origin = .dynamic)
    (hs : job.call.carriesScript = true) :
    admitStep ctx job out = .error .dynamicCodeDisabled ∨
      admitStep ctx job out = .error .policyDenied := by
  cases hstep : stepPin ctx job out with
  | none =>
    simp only [Bool.and_eq_true] at ha
    left
    simp [admitStep, hr, ha.1, ha.2, he, hres, hadm, hstep, h, hs]
  | some p =>
    right
    exact dynamic_off_refuses_run ctx job out p h hr ha he hres hadm hstep (hp p hstep)

/-- Under `policy.mode = off`, a pinless call that carries no `script` (an
artifact call that ends inline) is admitted as the non-job call it is: `off`
refuses dynamic origin only. -/
theorem dynamic_off_admits_sourceless_call (ctx : StepContext) (job : Job)
    (out : StepOutput) (h : ctx.policy.mode = .off)
    (hr : job.state = .running)
    (ha : (ctx.available && ctx.engineProvenanceOperator) = true)
    (he : engineChanged ctx job out = false) (hres : out.resume = none)
    (hp : stepPin ctx job out = none) (hs : job.call.carriesScript = false) :
    admitStep ctx job out = admitNonJob ctx job out := by
  simp only [Bool.and_eq_true] at ha
  simp [admitStep, hr, ha.1, ha.2, he, hres, resumeAdmitted, hp, h, hs]

/-- Under `policy.mode = check`, a dynamic-origin effects output with a fresh
spawn is refused with `policyDenied` once rules 1-4 pass. -/
theorem dynamic_check_refuses_spawn (ctx : StepContext) (job : Job) (e : Effects) (p : Pin)
    (h : ctx.policy.mode = .check) (hs : freshSpawns (stepRow ctx job (.effects e)) e.spawns ≠ [])
    (hr : job.state = .running)
    (ha : (ctx.available && ctx.engineProvenanceOperator) = true)
    (he : engineChanged ctx job (.effects e) = false)
    (hres : ((StepOutput.effects e).resume.isSome && !resumePlaced job (.effects e)) = false)
    (hadm : resumeAdmitted ctx (.effects e) = true)
    (hp : stepPin ctx job (.effects e) = some p) (ho : p.origin = .dynamic) :
    admitStep ctx job (.effects e) = .error .policyDenied := by
  simp only [Bool.and_eq_true] at ha
  simp [admitStep, hr, ha.1, ha.2, he, hres, hadm, hp, dynamicModeRefusal, ho, h, hs]

/-- Under `policy.mode = check`, a dynamic-origin effects output with a message
is refused with `policyDenied` once rules 1-4 pass: a message would start an
agent request in a child session. -/
theorem dynamic_check_refuses_message (ctx : StepContext) (job : Job) (e : Effects) (p : Pin)
    (h : ctx.policy.mode = .check) (hm : e.messages ≠ [])
    (hr : job.state = .running)
    (ha : (ctx.available && ctx.engineProvenanceOperator) = true)
    (he : engineChanged ctx job (.effects e) = false)
    (hres : ((StepOutput.effects e).resume.isSome && !resumePlaced job (.effects e)) = false)
    (hadm : resumeAdmitted ctx (.effects e) = true)
    (hp : stepPin ctx job (.effects e) = some p) (ho : p.origin = .dynamic) :
    admitStep ctx job (.effects e) = .error .policyDenied := by
  simp only [Bool.and_eq_true] at ha
  simp [admitStep, hr, ha.1, ha.2, he, hres, hadm, hp, dynamicModeRefusal, ho, h, hm]

/-- An installed-origin pin is never gated by the mode. -/
theorem installed_ignores_mode (ctx : StepContext) (job : Job) (out : StepOutput) (p : Pin)
    (m : DynamicMode) (hp : stepPin ctx job out = some p) (ho : p.origin = .installed) :
    admitStep { ctx with policy := { ctx.policy with mode := m } } job out = admitStep ctx job out := by
  have hp' : stepPin { ctx with policy := { ctx.policy with mode := m } } job out = some p := hp
  unfold admitStep
  rw [hp', hp]
  simp only [dynamicModeRefusal, ho]
  rfl

/-- `Policy.valid` ignores `dynamicTargets` when the mode is `off`, bounds them
by `agentTargets` otherwise, and the default empty list is valid for any
`agentTargets`. -/
theorem policy_subset_checked_only_when_dynamic (agentTargets : List String) :
    (∀ p : Policy, p.mode = .off → p.valid agentTargets = true) ∧
      (∀ p : Policy, p.mode ≠ .off → p.valid agentTargets = true →
        ∀ t ∈ p.dynamicTargets, t ∈ agentTargets) ∧
      ∀ p : Policy, p.dynamicTargets = [] → p.valid agentTargets = true := by
  refine ⟨fun p h => by simp [Policy.valid, h], fun p h hv t ht => ?_, fun p h => ?_⟩
  · simp only [Policy.valid, Bool.or_eq_true, beq_iff_eq, h, false_or] at hv
    exact (of_decide_eq_true hv) ht
  · simp [Policy.valid, h]

theorem pack_job_block_requires_operator_install (via : InstallProvenance)
    (closure : List String) (b : PackJobBlock) (h : packJobBlockAdmissible via closure b = true) :
    via = .operator := by
  simp only [packJobBlockAdmissible, decide_eq_true_eq] at h
  exact h.1

theorem pack_job_block_within_closure (via : InstallProvenance) (closure : List String)
    (b : PackJobBlock) (h : packJobBlockAdmissible via closure b = true) :
    b.targetsFrom ⊆ closure ∧ b.trusted ⊆ closure := by
  simp only [packJobBlockAdmissible, decide_eq_true_eq] at h
  exact ⟨h.2.1, h.2.2.1⟩

theorem pack_job_block_never_run (via : InstallProvenance) (closure : List String)
    (b : PackJobBlock) (h : packJobBlockAdmissible via closure b = true) :
    b.dynamic ≠ .run := by
  simp only [packJobBlockAdmissible, decide_eq_true_eq] at h
  intro hr
  simp [hr] at h

/-- After `wireTargetsFrom pack`, the pack is trusted and the job targets are
within the caller's targets, so an installed-origin pin spawns exactly the
live ∩ pinned ∩ job targets (`wiring_makes_job_targets_spawnable`). -/
theorem targets_from_wiring_trusts_pack (pack : String) (packTargets : List Target)
    (p p' : Policy) (jobTargets callerTargets job' caller' : List Target)
    (hw : wireTargetsFrom pack packTargets p jobTargets callerTargets = (p', job', caller')) :
    pack ∈ p'.trustedPacks ∧ packTargets ⊆ job' ∧ job' ⊆ caller' ∧
      ∀ (ctx : StepContext) (pin : Pin),
        ctx.jobTargets = .only job'.toFinset (fun _ => ()) →
        ctx.callerTargets = .only caller'.toFinset (fun _ => ()) →
        pin.origin = .installed →
          ∀ t, (effectiveTargets ctx pin).permits t ↔
            ((TargetResolution.scope ctx.bindings (ctx.live.meet pin.ceilingAtPin).targets).meet
              unitVM ctx.jobTargets).permits t := by
  simp only [wireTargetsFrom, Prod.mk.injEq] at hw
  obtain ⟨rfl, rfl, rfl⟩ := hw
  have htrust : pack ∈ (p.trustedPacks.insert pack) := List.mem_insert_self _ _
  have hsub : jobTargets ∪ packTargets ⊆ callerTargets ∪ (jobTargets ∪ packTargets) :=
    fun t ht => List.mem_union_iff.mpr (Or.inr ht)
  refine ⟨htrust, fun t ht => List.mem_union_iff.mpr (Or.inr ht), hsub, ?_⟩
  intro ctx pin hj hc hi
  refine wiring_makes_job_targets_spawnable ctx pin ?_ hi
  rw [hj, hc]
  simp only [scopeLe, decide_eq_true_eq]
  intro t ht
  rw [List.mem_toFinset] at ht ⊢
  exact hsub ht

/-! ## Nested jobs -/

section Nested

variable {Req : Type}

/-- A request whose cross-session ancestry contains a plugin-job call. `edges r`
lists each `(p, k)`: `r` was caused from another session by a tool call of
kind `k` in request `p`. Same-session continuations are collapsed into their
session's requests, so they are never edges. -/
inductive JobAncestor (edges : Req → List (Req × ToolExecution.ToolOperation)) : Req → Prop
  | direct {r p : Req} : (p, .pluginJob) ∈ edges r → JobAncestor edges r
  | through {r p : Req} {k : ToolExecution.ToolOperation} :
      (p, k) ∈ edges r → JobAncestor edges p → JobAncestor edges r

/-- Every cross-session edge strictly lowers the hop going upward. -/
def hopDecreasing (edges : Req → List (Req × ToolExecution.ToolOperation)) (hop : Req → Nat) :
    Prop :=
  ∀ r p k, (p, k) ∈ edges r → hop p < hop r

/-- The walk up cross-session edges with `fuel` steps left. -/
def startedByJobWithin (edges : Req → List (Req × ToolExecution.ToolOperation)) :
    Nat → Req → Bool
  | 0, _ => false
  | n + 1, r => (edges r).any fun e => e.2 == .pluginJob || startedByJobWithin edges n e.1

/-- The host's ancestry walk behind `StepContext.startedByJob`. Its fuel is the
caller's `request_hop`; every cross-session ancestor strictly lowers that hop,
so the provenance value supplies sufficient fuel independently of whether the
node configures an admission cap. It is not the graph Goal-ancestry kernel,
which carries Goal semantics and multi-parent fuel. -/
def startedByJob (edges : Req → List (Req × ToolExecution.ToolOperation)) (hop : Req → Nat)
    (r : Req) : Bool :=
  startedByJobWithin edges (hop r) r

/-- Materialization gives every cross-session edge a decreasing hop
(`cross_session_exceeds_cause`). -/
theorem cross_session_edges_lower_hop (edges : Req → List (Req × ToolExecution.ToolOperation))
    (hop own : Req → Nat)
    (h : ∀ r p k, (p, k) ∈ edges r → hop r = CausalHop.nextHop (.crossSession (hop p)) (own r)) :
    hopDecreasing edges hop := by
  intro r p k he
  have := CausalHop.cross_session_exceeds_cause (hop p) (own r)
  rw [← h r p k he] at this
  omega

private theorem startedByJobWithin_complete {edges : Req → List (Req × ToolExecution.ToolOperation)}
    {hop : Req → Nat} (hd : hopDecreasing edges hop) {r : Req} (ha : JobAncestor edges r) :
    ∀ n, hop r ≤ n → startedByJobWithin edges n r = true := by
  induction ha with
  | @direct r p he =>
    intro n hn
    cases n with
    | zero => exact absurd (hd _ _ _ he) (by omega)
    | succ n =>
      simp only [startedByJobWithin, List.any_eq_true, Bool.or_eq_true, beq_iff_eq]
      exact ⟨_, he, Or.inl rfl⟩
  | @through r p k he _ ih =>
    intro n hn
    have hlt := hd _ _ _ he
    cases n with
    | zero => omega
    | succ n =>
      simp only [startedByJobWithin, List.any_eq_true, Bool.or_eq_true, beq_iff_eq]
      exact ⟨_, he, Or.inr (ih n (by omega))⟩

/-- A request descended from a plugin job through any number of cross-session
edges is flagged, so the host refuses its adoption with
`plugin_job_depth_exceeded`. -/
theorem nested_job_refused_at_any_depth
    (edges : Req → List (Req × ToolExecution.ToolOperation)) (hop : Req → Nat) (r : Req)
    (ha : JobAncestor edges r) (hd : hopDecreasing edges hop) :
    startedByJob edges hop r = true :=
  startedByJobWithin_complete hd ha (hop r) (Nat.le_refl _)

end Nested

/-! ## Scripts grant nothing -/

/-- What a job has caused: its row, whose children are its child requests, and
the data writes committed for it. -/
structure World where
  job : Job
  writes : List (ToolPolicy.ApplicationWrite.Operation × ToolPolicy.PluginData.Observation)

/-- One host transition. The guest's output is arbitrary: a step commits only
what `admitStep` admits under the context re-read in its transaction, a refused
step ends the row through `settleStep`, a control, deadline or child terminal
changes no child set, and a data write commits only when
`PluginData.admitted`. -/
inductive Moves : World → World → Prop
  | step {w : World} {job' : Job} (ctx : StepContext) (out : StepOutput)
      (h : admitStep ctx w.job out = .ok job') : Moves w { w with job := job' }
  | refuse {w : World} (ctx : StepContext) (out : StepOutput) (r : Refusal)
      (obs : SpawnKey → Recovery.KillObservation) (h : admitStep ctx w.job out = .error r) :
      Moves w { w with job := (settleStep ctx w.job out obs).1 }
  | kill {w : World} (c : Control) (obs : SpawnKey → Recovery.KillObservation) :
      Moves w { w with job := (Control.kill w.job c obs).1 }
  | observe {w : World} (k : SpawnKey) (s : ChildState) :
      Moves w { w with job := observeChild w.job k s }
  | write {w : World} (op : ToolPolicy.ApplicationWrite.Operation)
      (o : ToolPolicy.PluginData.Observation) (h : ToolPolicy.PluginData.admitted op o = true) :
      Moves w { w with writes := w.writes ++ [(op, o)] }

private theorem settleStep_children {ctx : StepContext} {job : Job} {out : StepOutput} {r : Refusal}
    (obs : SpawnKey → Recovery.KillObservation) (h : admitStep ctx job out = .error r) :
    (settleStep ctx job out obs).1.children = job.children := by
  unfold settleStep
  rw [h]
  cases r <;> simp only [terminalizeRow] <;> split <;> rfl

private theorem kill_children (job : Job) (c : Control) (obs : SpawnKey → Recovery.KillObservation) :
    (Control.kill job c obs).1.children = job.children := by
  unfold Control.kill terminalizeRow
  split
  · dsimp only
    split <;> rfl
  · rfl

private theorem observeChild_keys (job : Job) (k : SpawnKey) (s : ChildState) :
    (observeChild job k s).children.map (·.key) = job.children.map (·.key) := by
  simp only [observeChild, List.map_map]
  congr 1
  funext ch
  simp only [Function.comp_apply]
  split <;> rfl

/-- Every child request and every data write in a state reachable from a job
with no writes came through `admitStep` or `PluginData.admitted`: a child key
is one the job started with, or one an admitted step added to a state
reachable from the start. -/
theorem script_grants_nothing {w0 w : World} (h0 : w0.writes = [])
    (hr : Relation.ReflTransGen Moves w0 w) :
    (∀ k ∈ w.job.children.map (·.key), k ∈ w0.job.children.map (·.key) ∨
      ∃ b ctx out job', Relation.ReflTransGen Moves w0 b ∧ admitStep ctx b.job out = .ok job' ∧
        k ∈ job'.children.map (·.key) ∧ k ∉ b.job.children.map (·.key)) ∧
      ∀ x ∈ w.writes, ToolPolicy.PluginData.admitted x.1 x.2 = true := by
  induction hr with
  | refl => exact ⟨fun k hk => Or.inl hk, by simp [h0]⟩
  | @tail b _ hb hm ih =>
    obtain ⟨ihk, ihw⟩ := ih
    cases hm with
    | @step job' ctx out h =>
      refine ⟨fun k hk => ?_, ihw⟩
      by_cases hold : k ∈ b.job.children.map (·.key)
      · exact ihk k hold
      · exact Or.inr ⟨b, ctx, out, job', hb, h, hk, hold⟩
    | refuse ctx out r obs h =>
      exact ⟨fun k hk => ihk k (by rwa [settleStep_children obs h] at hk), ihw⟩
    | kill c obs =>
      exact ⟨fun k hk => ihk k (by rwa [kill_children] at hk), ihw⟩
    | observe k' s =>
      exact ⟨fun k hk => ihk k (by rwa [observeChild_keys] at hk), ihw⟩
    | write op o h =>
      refine ⟨ihk, fun x hx => ?_⟩
      rcases List.mem_append.mp hx with hx | hx
      · exact ihw x hx
      · simp only [List.mem_singleton] at hx
        subst hx
        exact h

end PluginJob
