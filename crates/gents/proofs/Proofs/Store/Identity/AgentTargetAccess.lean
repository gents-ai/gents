import Lean
import Proofs.Store.Identity.DeclaredAccess

namespace Identity.AgentTargetAccess

open Identity.Household

/-- An AgentTarget of the configuring Node's configuration. It is `isLocal`
when its target Node is that Node; a target on another Node is admitted there
as a Peer request under that Node's ACP, so this Node declares nothing for it. -/
structure Target where
  id : String
  agent : String
  isLocal : Bool
  deriving BEq, Repr

/-- The agent targets an Agent selects: its AgentContext's Tools, whose
AgentTools group exposes `agent_new`/`agent_message` over `targetIds` only
when `enabled`, and whose plugin job blocks name `jobTargetIds`
(`job.agent_targets`). A plugin job spawns into `jobTargets ∩ callerTargets`
(`PluginJob.effectiveTargets`), where `callerTargets` is `targetIds` whether or
not the group is enabled: `gents pack wire` adds job targets to `targetIds`
and leaves `enabled` unchanged. -/
structure Selection where
  agent : String
  enabled : Bool
  targetIds : List String
  jobTargetIds : List String := []
  deriving BEq, Repr

/-- One Node's configuration: its Agent documents (keyed by Agent DID), each
Agent's agent-target selection and its AgentTargets. -/
structure Config where
  agents : List String
  selections : List Selection
  targets : List Target
  deriving Repr

/-- The target ids the Agent may start a session on: every `targetIds` entry
for an enabled group, and otherwise those a plugin job block also names. -/
def startable (selection : Selection) : List String :=
  if selection.enabled then selection.targetIds
  else selection.targetIds.filter selection.jobTargetIds.contains

def targetAgents (config : Config) (selection : Selection) : List String :=
  (startable selection).filterMap fun id =>
    (config.targets.find? fun target => target.id == id && target.isLocal).map (·.agent)

/-- Configured delegation is declared access (`DeclaredAccess`): an Agent
that selects an enabled agent target on its own Node declares itself a
`session_writer` of the target's Agent document, the relation behind native
`invoke` that `agent_new`, `agent_message` and a plugin job's child spawn
need. Nothing else is declared: no reader, executor or managing relation, and
nothing for a disabled group's target no plugin job names, an unselected
target, a remote target or an absent Agent. -/
def writers (config : Config) (target : String) : List String :=
  if config.agents.contains target then
    ((config.selections.filter fun selection =>
        config.agents.contains selection.agent &&
          (targetAgents config selection).contains target).map (·.agent)).eraseDups
  else []

def sessionWriter (target subject : String) : Relation :=
  DeclaredAccess.declaredRelation "agents" target "session_writer" subject

/-- The declaration of each Agent document before and after one configuration
apply. An Agent the apply removes declares no writers afterwards, so its
tuples are revoked before the document is deleted. -/
def changes (prior next : Config) : List (String × List String × List String) :=
  (prior.agents ++ next.agents).eraseDups.map fun target =>
    (target, writers prior target, writers next target)

/-- The configuring actor (the User, or an Engineer holding the authority)
must natively manage every tuple the apply changes. -/
def authorized (state : State) (configurer : String) (prior next : Config) : Bool :=
  (changes prior next).all fun change =>
    (DeclaredAccess.removed change.2.1 change.2.2 ++ DeclaredAccess.added change.2.1 change.2.2).all
      fun subject => mayManage state configurer (sessionWriter change.1 subject)

def step (configurer : String) (state : State) (change : String × List String × List String) : State :=
  DeclaredAccess.applyDeclaredRelation state configurer "agents" change.1 "session_writer"
    change.2.1 change.2.2

/-- The configuration apply and its declared tuples share one transaction: a
change the configuring actor may not apply refuses the whole configuration
(`none`), so stored configuration never declares delegation its tuples lack. -/
def reconcile (state : State) (configurer : String) (prior next : Config) : Option State :=
  if authorized state configurer prior next then
    some ((changes prior next).foldl (step configurer) state)
  else none

theorem fold_adds_only_declared (next : Config) (configurer : String) :
    ∀ (cs : List (String × List String × List String)) (state : State),
      (∀ c ∈ cs, c.2.2 = writers next c.1) →
      ∀ fact ∈ (cs.foldl (step configurer) state).relations,
        fact ∈ state.relations ∨
          ∃ target, ∃ subject ∈ writers next target, fact = sessionWriter target subject := by
  intro cs
  induction cs with
  | nil => intro state _ fact member; exact Or.inl member
  | cons c cs ih =>
    intro state declared fact member
    simp only [List.foldl_cons] at member
    rcases ih _ (fun c' mem => declared c' (List.mem_cons_of_mem _ mem)) fact member with
      inner | done
    · rcases DeclaredAccess.applyDeclaredRelation_adds_only_declared _ _ _ _ _ _ _ _ inner with
        old | ⟨subject, mem, eq⟩
      · exact Or.inl old
      · rw [declared c (List.mem_cons_self _ _)] at mem
        exact Or.inr ⟨c.1, subject, mem, eq⟩
    · exact Or.inr done

/-- Exactly the declared relation: every tuple a configuration apply adds is
`session_writer` for an Agent that selects the target on its Node. -/
theorem reconcile_adds_only_declared (state result : State) (configurer : String)
    (prior next : Config) (applied : reconcile state configurer prior next = some result)
    (fact : Relation) (member : fact ∈ result.relations) :
    fact ∈ state.relations ∨
      ∃ target, ∃ subject ∈ writers next target, fact = sessionWriter target subject := by
  simp only [reconcile] at applied
  split at applied
  · cases applied
    exact fold_adds_only_declared next configurer _ state
      (fun c mem => by
        simp only [changes, List.mem_map] at mem
        obtain ⟨target, _, eq⟩ := mem
        rw [← eq]) fact member
  · cases applied

/-- An applied configuration never exceeds the configuring actor's authority:
it changes only tuples that actor natively manages. -/
theorem reconcile_within_configurer_authority (state result : State) (configurer : String)
    (prior next : Config) (applied : reconcile state configurer prior next = some result)
    (change : String × List String × List String) (changed : change ∈ changes prior next)
    (subject : String)
    (member : subject ∈ DeclaredAccess.removed change.2.1 change.2.2 ++
      DeclaredAccess.added change.2.1 change.2.2) :
    mayManage state configurer (sessionWriter change.1 subject) = true := by
  simp only [reconcile] at applied
  split at applied
  · rename_i ok
    simp only [authorized, List.all_eq_true] at ok
    exact ok change changed subject member
  · cases applied

theorem reconcile_refuses_without_authority (state : State) (configurer : String)
    (prior next : Config) (denied : authorized state configurer prior next = false) :
    reconcile state configurer prior next = none := by
  simp [reconcile, denied]

theorem unchanged_fold_is_inert (config : Config) (configurer : String) :
    ∀ (targets : List String) (state : State),
      (targets.map fun target => (target, writers config target, writers config target)).foldl
        (step configurer) state = state := by
  intro targets
  induction targets with
  | nil => intro state; rfl
  | cons target targets ih =>
    intro state
    simp only [List.map_cons, List.foldl_cons, step,
      DeclaredAccess.unchanged_declaration_relation_is_inert]
    exact ih state

/-- An unchanged configuration republish grants and revokes nothing, so a
revoked tuple stays revoked. -/
theorem unchanged_configuration_is_inert (state : State) (configurer : String) (config : Config) :
    reconcile state configurer config config = some state := by
  have ok : authorized state configurer config config = true := by
    simp only [authorized, changes, DeclaredAccess.added, DeclaredAccess.removed, List.all_eq_true]
    intro change member subject subjectMember
    simp only [List.mem_map] at member
    obtain ⟨target, _, eq⟩ := member
    subst eq
    simp at subjectMember
  simp only [reconcile, ok, if_true, changes]
  rw [unchanged_fold_is_inert]

/-- The User owns the Node, so it administers each Agent through the Node
parent; `admin` is an Engineer granted Node administration; `lead` holds only
its own Agent. Each Agent executes itself. -/
def fixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"agents", "lead", "lead"⟩,
      ⟨"agents", "reviewer", "reviewer"⟩, ⟨"agents", "other", "other"⟩],
    relations := [⟨"nodes", "node", "administrator", .actor "admin"⟩] ++
      (["lead", "reviewer", "other"].flatMap fun agent =>
        [⟨"agents", agent, "parent", .object "nodes" "node"⟩,
         ⟨"agents", agent, "administrator", .entitySet "nodes" "node" "owner"⟩,
         ⟨"agents", agent, "administrator", .entitySet "nodes" "node" "administrator"⟩,
         ⟨"agents", agent, "executor", .actor agent⟩]),
    enrolled := [] }

def agents : List String := ["lead", "reviewer", "other"]

/-- `lead` selects the local `reviewer` target and a target that names
`other` on another Node. -/
def targets : List Target := [⟨"reviewer-target", "reviewer", true⟩, ⟨"remote-target", "other", false⟩]

def unconfigured : Config := ⟨agents, [], targets⟩
def configured : Config := ⟨agents, [⟨"lead", true, ["reviewer-target", "remote-target"], []⟩], targets⟩
def disabled : Config := ⟨agents, [⟨"lead", false, ["reviewer-target", "remote-target"], []⟩], targets⟩
def removedTarget : Config := ⟨agents, [⟨"lead", true, ["remote-target"], []⟩], targets⟩
/-- `gents pack wire` with the agents group disabled: the plugin job names
`reviewer-target`, which also joins the caller's target ids. -/
def jobOnly : Config :=
  ⟨agents, [⟨"lead", false, ["reviewer-target", "remote-target"], ["reviewer-target"]⟩], targets⟩

def configure (state : State) (configurer : String) (prior next : Config) : State :=
  (reconcile state configurer prior next).getD state

def byUser : State := configure fixture "user" unconfigured configured
def byEngineer : State := configure fixture "admin" unconfigured configured
def afterDisable : State := configure byUser "user" configured disabled
def afterRemoval : State := configure byUser "user" configured removedTarget
def byJob : State := configure fixture "user" unconfigured jobOnly
def afterJobRemoval : State := configure byJob "user" jobOnly disabled

def invokes (state : State) (caller target : String) : Bool :=
  allowed 5 state caller "agents" target "invoke"

def pairs : List (String × String) :=
  [("lead", "reviewer"), ("lead", "other"), ("other", "reviewer"), ("reviewer", "lead")]

theorem configured_target_grants_exactly_session_writer :
    pairs.map (fun (caller, target) => invokes byUser caller target) =
      [true, false, false, false] ∧
    byUser.relations.length = fixture.relations.length + 1 ∧
    byUser.relations.contains (sessionWriter "reviewer" "lead") = true := by native_decide

theorem engineer_with_authority_applies_the_same_relation :
    pairs.map (fun (caller, target) => invokes byEngineer caller target) =
      [true, false, false, false] := by native_decide

/-- An actor without authority over the target Agent cannot configure the
delegation at all: the apply is refused and nothing is granted. -/
theorem configuration_never_exceeds_the_configurer :
    (reconcile fixture "lead" unconfigured configured).isNone = true ∧
    pairs.map (fun (caller, target) => invokes (configure fixture "lead" unconfigured configured) caller target) =
      [false, false, false, false] := by native_decide

theorem removing_or_disabling_the_target_revokes :
    pairs.map (fun (caller, target) => invokes afterRemoval caller target) =
      [false, false, false, false] ∧
    pairs.map (fun (caller, target) => invokes afterDisable caller target) =
      [false, false, false, false] ∧
    afterRemoval.relations.length = fixture.relations.length := by native_decide

/-- A plugin job's target is declared with the agents group disabled, so a
job child spawn has the same native `invoke` as `agent_new`; dropping the job
block revokes it. -/
theorem job_target_grants_session_writer_while_disabled :
    pairs.map (fun (caller, target) => invokes byJob caller target) =
      [true, false, false, false] ∧
    pairs.map (fun (caller, target) => invokes afterJobRemoval caller target) =
      [false, false, false, false] ∧
    afterJobRemoval.relations.length = fixture.relations.length := by native_decide

theorem undeclared_target_stays_denied :
    invokes fixture "lead" "reviewer" = false ∧
    invokes byUser "lead" "other" = false ∧
    invokes byUser "other" "reviewer" = false := by native_decide

/-- The modeled apply sequence: each step names its configuring actor and the
`lead` selection it publishes, whether the apply commits, and the resulting
native `invoke` decisions. -/
def sequence : List (String × String × Config) :=
  [("unconfigured", "user", unconfigured), ("configured", "user", configured),
   ("disabled", "user", disabled), ("reenabled", "user", configured),
   ("removed", "user", removedTarget), ("refused", "lead", configured),
   ("job_only", "user", jobOnly), ("job_dropped", "user", disabled)]

end Identity.AgentTargetAccess
