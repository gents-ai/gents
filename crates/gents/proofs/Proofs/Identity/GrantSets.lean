import Proofs.Identity.NodeProvisioning

/-!
The named node-permission sets Gents issues, who receives each, and the
native held-bounded rule that bounds every later change.

Rust owner: `crates/gents/src/store/identity/grant_sets.rs` (`GrantSet`),
with the Agent baseline projected by
`crates/gents/src/store/local/tool_permission_projection.rs`.
-/
namespace Identity.GrantSets

/-- The native admin class, by wire name.

External premise: the pinned DefraDB revision's
`proofs/lean/NodeAcpDelegation.lean` (`adminClass`) and
`crates/acp/src/nac/delegation.rs` (`ADMIN_CLASS`). These permissions widen
authority or switch enforcement off, so only the owner or an admin changes
who holds them. -/
def adminClass : List String :=
  ["bypass-dac", "add-nac-relation", "delete-nac-relation", "disable-nac",
   "re-enable-nac", "purge-nac", "enable-dac", "disable-dac", "purge-dac"]

def nacOperation (delete : Bool) : String :=
  if delete then "delete-nac-relation" else "add-nac-relation"

inductive GrantSet where
  | agentBaseline
  | operatorUser
  | householdOwner
  | policyDelegate
  deriving BEq, DecidableEq, Repr

def GrantSet.name : GrantSet → String
  | .agentBaseline => "agent-baseline"
  | .operatorUser => "operator-user"
  | .householdOwner => "household-owner"
  | .policyDelegate => "policy-delegate"

/-- `agentBaseline` stays `NodeProvisioning.requiredPermissions`. Every Agent
turn reads its configuration and session documents, writes its requests,
outputs and tool calls, and links the documents it creates, all as its own
signer, so no tool selection can be the removable source of these three.
Session-history, goal and goal-create tool sources therefore add nothing
beyond the baseline: removing those tools withdraws the tool surface, and
document ACP still decides each document. Every other tool-derived
permission has the tool source as its only grant, so removing the tool
removes the permission (`tool_removal_removes_permission`).

`operatorUser` is what an enrolled User needs to author configuration whose
projection stages source edges: the document operations, DAC relationship
management, and the NAC relation operations a delegate needs under the
native held-bounded rule. Schema authority is not in it.

`householdOwner` is the native `admin` relation, which also confers DAC
bypass. It carries no permission list because admin implies every one.

`policyDelegate` is the relationship-management authority: an Agent holding
it can hand out only the permissions it already holds. -/
def GrantSet.permissions : GrantSet → List String
  | .agentBaseline => NodeProvisioning.requiredPermissions
  | .operatorUser =>
      ["get-collection", "read-document", "update-document", "delete-document",
       "add-dac-relation", "delete-dac-relation", "add-nac-relation", "delete-nac-relation"]
  | .householdOwner => []
  | .policyDelegate =>
      ["add-nac-relation", "delete-nac-relation", "add-dac-relation", "delete-dac-relation"]

def GrantSet.nativeAdmin : GrantSet → Bool
  | .householdOwner => true
  | _ => false

def all : List GrantSet := [.agentBaseline, .operatorUser, .householdOwner, .policyDelegate]

/-- Who receives a set. The init User is the first User `gents init` creates:
the human owner. An enrolled User is admitted by verified enrollment. The
default Agent is the Agent init publishes. -/
inductive Recipient where
  | initUser
  | enrolledUser
  | defaultAgent
  | agent
  deriving BEq, DecidableEq, Repr

def Recipient.name : Recipient → String
  | .initUser => "init-user"
  | .enrolledUser => "enrolled-user"
  | .defaultAgent => "default-agent"
  | .agent => "agent"

/-- Direct grants, issued once by the admission that creates the recipient:
init for the init User and the default Agent, enrollment for an enrolled
User. They persist until an explicit revocation. -/
def directSets : Recipient → List GrantSet
  | .initUser => [.householdOwner]
  | .enrolledUser => [.operatorUser]
  | .defaultAgent => [.policyDelegate]
  | .agent => []

/-- Sets projected from the Agent document in the transaction that publishes,
disables or deletes it. -/
def projectedSets : Recipient → List GrantSet
  | .defaultAgent | .agent => [.agentBaseline]
  | _ => []

def receives (r : Recipient) : List GrantSet := directSets r ++ projectedSets r

/-- What one actor holds natively: admin, plus its effective permissions. -/
structure Holder where
  admin : Bool
  held : List String
  deriving DecidableEq, Repr

def holderOf (sets : List GrantSet) : Holder :=
  { admin := sets.any GrantSet.nativeAdmin, held := sets.flatMap GrantSet.permissions }

def Holder.holds (h : Holder) (p : String) : Bool := h.admin || h.held.contains p

inductive Relation where
  | admin
  | permission (name : String)
  deriving BEq, DecidableEq, Repr

/-- One requested NAC change against one subject. -/
structure Change where
  relation : Relation
  delete : Bool
  concreteSubject : Bool := true
  privilegedSubject : Bool := false
  deriving DecidableEq, Repr

/-- The native gate, restated over a holder. An admin changes anything. A
delegate changes one permission outside the admin class that it holds, holding
the operation too, for a concrete subject, and never deletes an owner's or
admin's tuple. External premise: the pinned native `NodeAcpDelegation.authorized`
and `DelegationFacts::admits`; grants carry no provenance, so a delegate holding
`p` may revoke another subject's direct `p`. -/
def admits (r : Holder) (c : Change) : Bool :=
  r.admin ||
    match c.relation with
    | .admin => false
    | .permission p =>
        !adminClass.contains p && r.holds (nacOperation c.delete) && r.holds p &&
          c.concreteSubject && !(c.delete && c.privilegedSubject)

/-- Granting adds the permission to the subject; admin is a separate flag. -/
def grant (target : Holder) (c : Change) : Holder :=
  match c.relation with
  | .admin => { target with admin := !c.delete }
  | .permission p =>
      if c.delete then { target with held := target.held.filter (· != p) }
      else { target with held := p :: target.held }

theorem delegate_grants_only_held (r : Holder) (c : Change) (p : String)
    (notAdmin : r.admin = false) (perm : c.relation = .permission p)
    (ok : admits r c = true) :
    r.held.contains p = true ∧ adminClass.contains p = false := by
  revert ok
  cases h1 : adminClass.contains p <;> cases h2 : r.held.contains p <;>
    simp_all [admits, Holder.holds]

theorem delegate_never_changes_admin (r : Holder) (c : Change)
    (notAdmin : r.admin = false) (rel : c.relation = .admin) : admits r c = false := by
  simp [admits, notAdmin, rel]

/-- No escalation at the Gents level: after an admitted delegated change the
subject holds nothing beyond what it held or the requester held, and its admin
standing is unchanged. -/
theorem delegated_no_escalation (r target : Holder) (c : Change) (q : String)
    (notAdmin : r.admin = false) (ok : admits r c = true)
    (after : (grant target c).held.contains q = true) :
    (target.held.contains q = true ∨ r.held.contains q = true) ∧
      (grant target c).admin = target.admin := by
  cases rel : c.relation with
  | admin => simp [delegate_never_changes_admin r c notAdmin rel] at ok
  | permission p =>
      have held := delegate_grants_only_held r c p notAdmin rel ok
      by_cases del : c.delete
      · simp [grant, rel, del] at after ⊢
        exact Or.inl after.1
      · simp [grant, rel, del] at after ⊢
        rcases after with eq | old
        · subst eq; exact Or.inr (by simpa using held.1)
        · exact Or.inl old

theorem missing_operation_admits_nothing (r : Holder) (c : Change)
    (notAdmin : r.admin = false) (missing : r.holds (nacOperation c.delete) = false) :
    admits r c = false := by
  cases rel : c.relation <;> simp [admits, notAdmin, rel, missing]

theorem recipients_receive_expected_sets :
    receives .initUser = [.householdOwner] ∧
    receives .enrolledUser = [.operatorUser] ∧
    receives .defaultAgent = [.policyDelegate, .agentBaseline] ∧
    receives .agent = [.agentBaseline] := by decide

theorem only_init_user_is_admin :
    [Recipient.initUser, .enrolledUser, .defaultAgent, .agent].map
      (fun r => (holderOf (receives r)).admin) = [true, false, false, false] := by decide

def schemaPermissions : List String :=
  ["get-collection", "patch-collection", "set-migration", "add-view"]

/-- An enrolled User may project any permission it holds and no schema
authority; the household owner may project schema authority. -/
theorem enrolled_user_cannot_grant_schema :
    ["patch-collection", "set-migration", "add-view"].all (fun p =>
      admits (holderOf (receives .enrolledUser)) ⟨.permission p, false, true, false⟩) = false := by
  decide

theorem enrolled_user_grants_held_operations :
    (GrantSet.operatorUser.permissions.filter (!adminClass.contains ·)).all (fun p =>
      admits (holderOf (receives .enrolledUser)) ⟨.permission p, false, true, false⟩) = true := by
  decide

theorem household_owner_grants_schema :
    schemaPermissions.all (fun p =>
      admits (holderOf (receives .initUser)) ⟨.permission p, false, true, false⟩) = true := by
  decide

theorem baseline_agent_cannot_delegate (p : String) (delete : Bool) :
    admits (holderOf (receives .agent)) ⟨.permission p, delete, true, false⟩ = false := by
  cases delete <;>
    exact missing_operation_admits_nothing _ _ (by decide)
      (by simp [nacOperation, holderOf, receives, directSets, projectedSets, Holder.holds,
        GrantSet.permissions, NodeProvisioning.requiredPermissions, GrantSet.nativeAdmin])

theorem default_agent_delegates_only_held :
    admits (holderOf (receives .defaultAgent)) ⟨.permission "read-document", false, true, false⟩ = true ∧
    admits (holderOf (receives .defaultAgent)) ⟨.permission "patch-collection", false, true, false⟩ = false ∧
    admits (holderOf (receives .defaultAgent)) ⟨.permission "add-nac-relation", false, true, false⟩ = false ∧
    admits (holderOf (receives .defaultAgent)) ⟨.admin, false, true, false⟩ = false := by
  decide

/-! ### The projected Agent baseline -/

/-- The Agent document as the projection observes it in its transaction. -/
structure AgentObservation where
  present : Bool
  enabled : Bool
  managed : Bool
  deriving DecidableEq, Repr

def AgentObservation.live (o : AgentObservation) : Bool := o.present && o.enabled && o.managed

def baselineWanted (o : AgentObservation) : List String :=
  if o.live then GrantSet.agentBaseline.permissions else []

/-- The projection stages exactly the missing and stale baseline edges as the
mutation's actor, in the mutation's transaction. Every edge that would change
must be admitted, or the mutation and all its edges are discarded, so no
partial grant survives a refusal. An unchanged edge needs no authority, so an
actorless startup reconcile succeeds exactly when nothing is missing. -/
def projectBaseline (actor : Holder) (after : AgentObservation) (installed : List String) :
    Option (List String) :=
  let wanted := baselineWanted after
  let adds := wanted.filter (!installed.contains ·)
  let removes := installed.filter (!wanted.contains ·)
  if adds.all (fun p => admits actor ⟨.permission p, false, true, false⟩) &&
      removes.all (fun p => admits actor ⟨.permission p, true, true, false⟩) then
    some wanted
  else none

def actorless : Holder := { admin := false, held := [] }

theorem baseline_commits_whole_or_not_at_all (actor : Holder) (after : AgentObservation)
    (installed result : List String) (h : projectBaseline actor after installed = some result) :
    result = baselineWanted after := by
  simp only [projectBaseline] at h
  split at h <;> simp_all

theorem disabled_agent_loses_baseline (actor : Holder) (o : AgentObservation)
    (installed result : List String) (off : o.enabled = false)
    (h : projectBaseline actor o installed = some result) : result = [] := by
  rw [baseline_commits_whole_or_not_at_all actor o installed result h]
  simp [baselineWanted, AgentObservation.live, off]

theorem deleted_agent_loses_baseline (actor : Holder) (o : AgentObservation)
    (installed result : List String) (gone : o.present = false)
    (h : projectBaseline actor o installed = some result) : result = [] := by
  rw [baseline_commits_whole_or_not_at_all actor o installed result h]
  simp [baselineWanted, AgentObservation.live, gone]

theorem actorless_reconcile_cannot_mint :
    projectBaseline actorless ⟨true, true, true⟩ [] = none ∧
    projectBaseline actorless ⟨true, true, true⟩ GrantSet.agentBaseline.permissions =
      some GrantSet.agentBaseline.permissions := by decide

theorem enrolled_user_publishes_and_disables_agent :
    projectBaseline (holderOf (receives .enrolledUser)) ⟨true, true, true⟩ [] =
      some GrantSet.agentBaseline.permissions ∧
    projectBaseline (holderOf (receives .enrolledUser)) ⟨true, false, true⟩
      GrantSet.agentBaseline.permissions = some [] := by decide

theorem baseline_agent_cannot_publish_agent :
    projectBaseline (holderOf (receives .agent)) ⟨true, true, true⟩ [] = none := by decide

theorem default_agent_publishes_agent :
    projectBaseline (holderOf (receives .defaultAgent)) ⟨true, true, true⟩ [] =
      some GrantSet.agentBaseline.permissions := by decide

/-- Effective node permissions of a managed Agent: its direct grants, plus the
baseline and its tool-derived sources while its document is live. -/
def agentEffective (o : AgentObservation) (tools direct : List String) (p : String) : Bool :=
  direct.contains p || (o.live && (GrantSet.agentBaseline.permissions.contains p || tools.contains p))

theorem tool_removal_removes_permission (o : AgentObservation) (tools direct : List String)
    (p : String) (notBaseline : GrantSet.agentBaseline.permissions.contains p = false)
    (notDirect : direct.contains p = false) :
    agentEffective o (tools.filter (· != p)) direct p = false := by
  have removed : (tools.filter (· != p)).contains p = false := by simp
  unfold agentEffective
  rw [notDirect, notBaseline, removed]
  simp

theorem baseline_survives_tool_removal (o : AgentObservation) (tools direct : List String)
    (p : String) (live : o.live = true) (base : GrantSet.agentBaseline.permissions.contains p = true) :
    agentEffective o (tools.filter (· != p)) direct p = true := by
  unfold agentEffective
  rw [live, base]
  simp

/-! ### The policy resource

The config tool's `policy` resource (`get`, `check`, `explain`, `grant`,
`revoke`) runs only with the `policy` category selected and only as the calling
Agent's own signer. It adds no tool-derived native source: a policy grant is
admitted by the Agent's direct grants through the native held-bounded rule, so
the Engineer's authority is its `policyDelegate` set and nothing else. -/

/-- Selected by the Engineer preset; other Agents receive it only by explicit
configuration. -/
def policyCategory : String := "policy"

/-- One policy grant or revoke issued through the config tool. The category gate
is necessary, and native admission is exactly `admits`, so the tool can refuse
nothing the native rule would allow and allow nothing it would refuse. -/
def policyToolAdmits (categories : List String) (actor : Holder) (c : Change) : Bool :=
  categories.contains policyCategory && admits actor c

theorem policy_tool_requires_category (categories : List String) (actor : Holder) (c : Change)
    (absent : categories.contains policyCategory = false) :
    policyToolAdmits categories actor c = false := by
  simp only [policyToolAdmits, absent, Bool.false_and]

theorem policy_tool_never_changes_admin (categories : List String) (actor : Holder)
    (d : Bool) (notAdmin : actor.admin = false) :
    policyToolAdmits categories actor ⟨.admin, d, true, false⟩ = false := by
  simp [policyToolAdmits, admits, notAdmin]

/-- A non-admin policy actor cannot change an admission-class permission, whether
it grants or revokes it. Native `ADMIN_CLASS` is the external premise. -/
theorem policy_tool_refuses_admin_class (categories : List String) (actor : Holder)
    (p : String) (d : Bool) (notAdmin : actor.admin = false) (cls : adminClass.contains p = true) :
    policyToolAdmits categories actor ⟨.permission p, d, true, false⟩ = false := by
  have refused : admits actor ⟨.permission p, d, true, false⟩ = false := by
    simp only [admits, notAdmin, cls, Bool.false_or, Bool.not_true, Bool.false_and]
  simp [policyToolAdmits, refused]

/-- A non-admin policy actor cannot grant or revoke a permission it does not hold. -/
theorem policy_tool_refuses_unheld (categories : List String) (actor : Holder)
    (p : String) (d : Bool) (notAdmin : actor.admin = false) (unheld : actor.held.contains p = false) :
    policyToolAdmits categories actor ⟨.permission p, d, true, false⟩ = false := by
  have refused : admits actor ⟨.permission p, d, true, false⟩ = false := by
    simp only [admits, Holder.holds, notAdmin, unheld, Bool.false_or, Bool.and_false,
      Bool.false_and]
  simp [policyToolAdmits, refused]

/-- No escalation through the policy tool: whatever the subject gains is held by
the subject already or by the acting Agent. -/
theorem policy_tool_no_escalation (categories : List String) (r target : Holder) (c : Change)
    (q : String) (notAdmin : r.admin = false) (ok : policyToolAdmits categories r c = true)
    (after : (grant target c).held.contains q = true) :
    (target.held.contains q = true ∨ r.held.contains q = true) ∧
      (grant target c).admin = target.admin := by
  simp only [policyToolAdmits, Bool.and_eq_true] at ok
  exact delegated_no_escalation r target c q notAdmin ok.2 after

theorem engineer_policy_grants_only_held (p : String) (d : Bool)
    (ok : policyToolAdmits [policyCategory] (holderOf (receives .defaultAgent))
      ⟨.permission p, d, true, false⟩ = true) :
    (holderOf (receives .defaultAgent)).held.contains p = true := by
  simp only [policyToolAdmits, Bool.and_eq_true] at ok
  have notAdmin : (holderOf (receives .defaultAgent)).admin = false := by decide
  exact (delegate_grants_only_held _ _ p notAdmin rfl ok.2).1

theorem engineer_policy_refuses_admin_class :
    (adminClass.all fun p => [false, true].all fun d =>
      !policyToolAdmits [policyCategory] (holderOf (receives .defaultAgent))
        ⟨.permission p, d, true, false⟩) = true := by decide

theorem engineer_without_policy_category_refused (p : String) (d : Bool) :
    policyToolAdmits ["agent", "tools", "profile"] (holderOf (receives .defaultAgent))
      ⟨.permission p, d, true, false⟩ = false := by
  exact policy_tool_requires_category _ _ _ (by decide)

theorem engineer_policy_admits_held_operations :
    policyToolAdmits [policyCategory] (holderOf (receives .defaultAgent))
      ⟨.permission "read-document", false, true, false⟩ = true := by decide

theorem engineer_policy_refuses_unheld :
    policyToolAdmits [policyCategory] (holderOf (receives .defaultAgent))
      ⟨.permission "patch-collection", false, true, false⟩ = false := by decide

/-! ### Self-lockout under `self_config_no_lockout`

An Agent that set `self_config_no_lockout` may not, through the policy tool,
revoke from itself a permission that keeps it able to configure itself: its
baseline and its policy delegation. Other Agents' grants and all grants are
otherwise governed by `policyToolAdmits`. Document relationships on the Agent's
own documents are not modeled here; they stay governed by native document
authorization. -/

/-- Permissions the acting Agent keeps while no-lockout is set. -/
def lockoutProtected : List String :=
  GrantSet.agentBaseline.permissions ++ GrantSet.policyDelegate.permissions

/-- Whether a policy change keeps the acting Agent's configuration reachable. A
grant never removes access, so only a self revoke of a protected permission is
refused, and only while no-lockout is set. -/
def selfChangeKeepsLockout (noLockout isSelf : Bool) (c : Change) : Bool :=
  !(noLockout && isSelf) ||
    match c.relation with
    | .admin => true
    | .permission p => !(c.delete && lockoutProtected.contains p)

/-- The policy tool admits a change when the category gate, native admission and
the self-lockout guard all admit it. -/
def policyToolAdmitsSelf (categories : List String) (actor : Holder) (noLockout isSelf : Bool)
    (c : Change) : Bool :=
  policyToolAdmits categories actor c && selfChangeKeepsLockout noLockout isSelf c

theorem no_lockout_refuses_self_revoke_of_protected (categories : List String) (actor : Holder)
    (p : String) (guarded : lockoutProtected.contains p = true) :
    policyToolAdmitsSelf categories actor true true ⟨.permission p, true, true, false⟩ = false := by
  have lockout : selfChangeKeepsLockout true true ⟨.permission p, true, true, false⟩ = false := by
    simp only [selfChangeKeepsLockout, guarded, Bool.and_self, Bool.and_true,
      Bool.not_true, Bool.false_or]
  simp [policyToolAdmitsSelf, lockout]

theorem no_lockout_keeps_self_grant (categories : List String) (actor : Holder) (p : String)
    (ok : policyToolAdmits categories actor ⟨.permission p, false, true, false⟩ = true) :
    policyToolAdmitsSelf categories actor true true ⟨.permission p, false, true, false⟩ = true := by
  have lockout : selfChangeKeepsLockout true true ⟨.permission p, false, true, false⟩ = true := by
    simp [selfChangeKeepsLockout]
  simp [policyToolAdmitsSelf, lockout, ok]

theorem engineer_cannot_revoke_own_baseline :
    policyToolAdmitsSelf [policyCategory] (holderOf (receives .defaultAgent)) true true
      ⟨.permission "read-document", true, true, false⟩ = false := by decide

theorem engineer_can_revoke_own_grant_without_no_lockout :
    policyToolAdmitsSelf [policyCategory] (holderOf (receives .defaultAgent)) false true
      ⟨.permission "read-document", true, true, false⟩ = true := by decide

theorem engineer_can_revoke_baseline_from_others_under_no_lockout :
    policyToolAdmitsSelf [policyCategory] (holderOf (receives .defaultAgent)) true false
      ⟨.permission "read-document", true, true, false⟩ = true := by decide

end Identity.GrantSets
