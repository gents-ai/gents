import Proofs.Store.Identity.NodeProvisioning
import Proofs.Store.Identity.Household

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
outputs and tool calls, and links the documents it creates and can withdraw
those links (`agent_revokes_what_it_grants`), all as its own signer, so no tool
selection can be the removable source of these. It includes `get-collection`,
which the pinned DefraDB requires before any document write (defradb.rs
#2744), so removing the schema or datastore tools no longer withdraws
`get-collection` from a live Agent (`get_collection_survives_tool_removal`);
those tools still own every schema change permission.
Session-history and goal tool sources therefore add nothing beyond the
baseline: removing those tools withdraws the tool surface, and document ACP
still decides each document. Goal creation adds only `delete-document`, so a
creator Agent can retire the Goals it owns
(`Configuration.goal_creator_deletes_its_goal`). Every other tool-derived
permission has the tool source as its only grant, so removing the tool
removes the permission (`tool_removal_removes_permission`).

`operatorUser` is what an enrolled User needs to author configuration whose
projection stages source edges: the document operations, DAC relationship
management, and the NAC relation operations a delegate needs under the
native held-bounded rule. Schema authority is not in it.

`householdOwner` is the native `admin` relation, which also confers DAC
bypass. It carries no permission list because admin implies every one.

`policyDelegate` is the relationship-management authority: an Agent holding
it can hand out only the permissions it already holds. It also holds the
read-only `get-nac-status`, so it can explain any identity's node grants; that
permission is not admin-class and confers no change authority.

Decision: `policyDelegate` holds the read-only `get-collection` because native
document relationship changes outside a transaction require it (external
premise: the pinned DefraDB revision, defradb.rs #2770), and the Engineer's
policy tool grants and revokes the document relations it manages. Every Agent
now holds it through the baseline as well; the delegate keeps its own so the
Engineer may still hand it on, which the baseline does not allow
(`baseline_agent_cannot_delegate`). It is the
read half of `schemaPermissions` only and confers no schema change authority:
the set holds none of `schemaChangePermissions`
(`policy_delegate_holds_no_schema_change`), so the held-bounded rule refuses
every schema change the Engineer would grant
(`engineer_policy_refuses_schema_change`). Holding it, the Engineer may hand
`get-collection` on (`engineer_policy_grants_get_collection`). -/
def GrantSet.permissions : GrantSet → List String
  | .agentBaseline => NodeProvisioning.requiredPermissions
  | .operatorUser =>
      ["get-collection", "read-document", "update-document", "delete-document",
       "add-dac-relation", "delete-dac-relation", "add-nac-relation", "delete-nac-relation"]
  | .householdOwner => []
  | .policyDelegate =>
      ["get-collection", "add-nac-relation", "delete-nac-relation", "add-dac-relation",
       "delete-dac-relation", "get-nac-status"]

def GrantSet.nativeAdmin : GrantSet → Bool
  | .householdOwner => true
  | _ => false

def all : List GrantSet := [.agentBaseline, .operatorUser, .householdOwner, .policyDelegate]

/-- Who receives a set. The init User is the User that creates the node: the
human owner `gents init` creates, whose one client key desktop, CLI and shims
share, or an evaluation trial's evaluator, which creates and owns its
throwaway trial node. An enrolled User is admitted by verified enrollment. The
default Agent is the Agent init publishes, or the subject Agent a trial selects
as its node's default. -/
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
init (or trial installation) for the init User and the default Agent,
enrollment for an enrolled User. They persist until an explicit revocation. -/
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

/-- The schema change permissions. `get-collection` is the read half of
`schemaPermissions` and is deliberately not among them: it reads collection
definitions and, natively, admits document writes and relationship changes
outside a transaction, but changes no schema. Every live Agent holds it
through the baseline; none holds a schema change permission through it
(`baseline_holds_no_schema_change`). -/
def schemaChangePermissions : List String :=
  ["patch-collection", "truncate-collection", "set-migration", "add-view"]

def schemaPermissions : List String :=
  "get-collection" :: schemaChangePermissions

/-- An enrolled User may project any permission it holds and no schema change
authority; it holds and may project the read-only `get-collection`. The
household owner may project schema authority. -/
theorem enrolled_user_cannot_grant_schema :
    schemaChangePermissions.all (fun p =>
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
actorless startup reconcile succeeds exactly when nothing is missing.

Every edge is judged against the actor as it stood before the mutation. The
native gate reads the actor's current effective permissions, so the Rust owner
stages all additions before any removal: a re-keyed source (a new Context or
Tools path to a permission the actor already holds) is then admitted through
the path it replaces, as this definition admits it. -/
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

/-- The baseline as stores created before `get-collection` joined it hold it. -/
def baselineBeforeGetCollection : List String :=
  GrantSet.agentBaseline.permissions.filter (· != "get-collection")

/-- A release may widen the baseline. An Agent published under an earlier one
holds part of it, installed under its publisher's authority, and the actorless
startup reconcile cannot add the rest (`actorless_reconcile_cannot_mint`), so
without this step a store created before the widening would not open
(`widened_baseline_blocks_actorless_reconcile`).

Decision (maintainer, 2026-11-13, gents#3747): when the hosting node opens its
store it completes, as the node key that owns NAC, the baseline of every live
managed Agent that already holds some of it. The publisher authorized the
baseline for that Agent; the release defines what the baseline contains. The
step adds baseline permissions only, removes nothing, and mints nothing for an
Agent that holds none of the baseline, so a never-published or tampered Agent
is still refused. -/
def completeBaseline (o : AgentObservation) (installed : List String) : List String :=
  if o.live && installed.any (GrantSet.agentBaseline.permissions.contains ·) then
    installed ++ GrantSet.agentBaseline.permissions.filter (!installed.contains ·)
  else installed

theorem widened_baseline_blocks_actorless_reconcile :
    projectBaseline actorless ⟨true, true, true⟩ baselineBeforeGetCollection = none := by decide

theorem completed_baseline_admits_actorless_reconcile :
    (projectBaseline actorless ⟨true, true, true⟩
      (completeBaseline ⟨true, true, true⟩ baselineBeforeGetCollection)).isSome = true := by decide

theorem completion_mints_nothing_for_an_unpublished_agent (o : AgentObservation) :
    completeBaseline o [] = [] := by
  simp [completeBaseline]

theorem completion_leaves_a_dead_agent (o : AgentObservation) (installed : List String)
    (dead : o.live = false) : completeBaseline o installed = installed := by
  simp [completeBaseline, dead]

theorem completion_adds_only_baseline (o : AgentObservation) (installed : List String) (p : String)
    (h : p ∈ completeBaseline o installed) :
    p ∈ installed ∨ GrantSet.agentBaseline.permissions.contains p = true := by
  unfold completeBaseline at h
  split at h
  · rcases List.mem_append.mp h with h | h
    · exact Or.inl h
    · exact Or.inr (by simpa using (List.mem_filter.mp h).1)
  · exact Or.inl h

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

/-- What the baseline's `get-collection` weakens: before it, a live Agent held
`get-collection` only through a schema or datastore tool source or a direct
grant, so removing those tools withdrew it. Now it survives their removal. -/
theorem get_collection_survives_tool_removal (o : AgentObservation) (tools direct : List String)
    (live : o.live = true) :
    agentEffective o (tools.filter (· != "get-collection")) direct "get-collection" = true := by
  unfold agentEffective
  rw [live]
  simp [GrantSet.permissions, NodeProvisioning.requiredPermissions]

theorem baseline_holds_no_schema_change :
    schemaChangePermissions.all (fun p => !GrantSet.agentBaseline.permissions.contains p) = true := by
  decide

theorem baseline_survives_tool_removal (o : AgentObservation) (tools direct : List String)
    (p : String) (live : o.live = true) (base : GrantSet.agentBaseline.permissions.contains p = true) :
    agentEffective o (tools.filter (· != p)) direct p = true := by
  unfold agentEffective
  rw [live, base]
  simp

/-! ### Revoke follows grant

Maintainer decision (2026-11-12, gents#3683): whoever can grant access on a
document can revoke it. A native relationship change passes two gates: the
node operation (`add-dac-relation` or `delete-dac-relation`, NAC) and the
document's manager check (`Household.mayManage`, DAC), which grants and revokes
share. Every issued set that holds an add operation holds its delete, so an
actor's revoke is scoped exactly as its grant: the same documents, relations
and parent chain. Revoking only narrows (`Household.revocation_only_narrows`). -/

/-- Each relationship-management operation and its revoke. -/
def relationOperationPairs : List (String × String) :=
  [("add-dac-relation", "delete-dac-relation"), ("add-nac-relation", "delete-nac-relation")]

theorem grant_set_revokes_what_it_grants (g : GrantSet) (add delete : String)
    (pair : (add, delete) ∈ relationOperationPairs) (holds : add ∈ g.permissions) :
    delete ∈ g.permissions := by
  simp only [relationOperationPairs, List.mem_cons, Prod.mk.injEq, List.mem_nil_iff,
    or_false] at pair
  rcases pair with ⟨rfl, rfl⟩ | ⟨rfl, rfl⟩ <;> revert holds <;> cases g <;> decide

theorem holder_revokes_what_it_grants (sets : List GrantSet) (add delete : String)
    (pair : (add, delete) ∈ relationOperationPairs) (holds : (holderOf sets).holds add = true) :
    (holderOf sets).holds delete = true := by
  simp only [Holder.holds, holderOf, Bool.or_eq_true, List.contains_iff_mem, List.mem_flatMap]
    at holds ⊢
  rcases holds with admin | ⟨g, member, held⟩
  · exact Or.inl admin
  · exact Or.inr ⟨g, member, grant_set_revokes_what_it_grants g add delete pair held⟩

/-- A live managed Agent holds `delete-dac-relation` through its baseline, so
whatever adds relationships for it, baseline, Tools source or direct grant, it
can revoke them, given direct grants are issued as whole sets. -/
theorem agent_revokes_what_it_grants (o : AgentObservation) (tools direct : List String)
    (directPaired : direct.contains "add-dac-relation" = true →
      direct.contains "delete-dac-relation" = true)
    (holds : agentEffective o tools direct "add-dac-relation" = true) :
    agentEffective o tools direct "delete-dac-relation" = true := by
  unfold agentEffective at holds ⊢
  cases added : direct.contains "add-dac-relation"
  · simp only [added, Bool.false_or, Bool.and_eq_true] at holds
    simp [holds.1, GrantSet.permissions, NodeProvisioning.requiredPermissions]
  · rw [directPaired added, Bool.true_or]

def dacOperation (delete : Bool) : String :=
  if delete then "delete-dac-relation" else "add-dac-relation"

/-- One document relationship change as `actor`, whose node permissions are
`holds`: the native operation gate, then the document's manager gate. -/
def relationChangeAdmitted (holds : String → Bool) (state : Household.State) (actor : DID)
    (relation : Household.Relation) (delete : Bool) : Bool :=
  holds (dacOperation delete) && Household.mayManage state actor relation

/-- An actor whose add operation is paired with its delete can revoke the
relationship it just granted. -/
theorem revoke_follows_grant (holds : String → Bool)
    (paired : holds "add-dac-relation" = true → holds "delete-dac-relation" = true)
    (state : Household.State) (actor : DID) (relation : Household.Relation)
    (granted : relationChangeAdmitted holds state actor relation false = true) :
    relationChangeAdmitted holds (Household.apply state (.grantAs actor relation)) actor relation
      true = true := by
  simp only [relationChangeAdmitted, dacOperation, Bool.and_eq_true, if_false, if_true]
    at granted ⊢
  exact ⟨paired granted.1, (Household.grantor_may_revoke state actor relation granted.2).1⟩

theorem issued_sets_revoke_what_they_grant (sets : List GrantSet) (state : Household.State)
    (actor : DID) (relation : Household.Relation)
    (granted : relationChangeAdmitted (holderOf sets).holds state actor relation false = true) :
    relationChangeAdmitted (holderOf sets).holds (Household.apply state (.grantAs actor relation))
      actor relation true = true :=
  revoke_follows_grant _ (holder_revokes_what_it_grants sets _ _ (by decide)) state actor relation
    granted

/-- The Agent's own session, shared with a grantee and then withdrawn. The
session is agent-owned (`Household.commitAgentSession`); the sibling is another
baseline Agent of the same Node. -/
def shareFixture : Household.State :=
  Household.commitAgentSession
    { objects := [⟨"nodes", "home", "user"⟩, ⟨"agents", "general", "user"⟩],
      relations := [⟨"agents", "general", "parent", .object "nodes" "home"⟩],
      enrolled := ["user", "grantee"] }
    "agent-session" "agent" "general" true

def shareRelation : Household.Relation :=
  ⟨"sessions", "agent-session", "reader", .actor "grantee"⟩

theorem baseline_agent_revokes_its_share :
    let agent := (holderOf (receives .agent)).holds
    let shared := Household.apply shareFixture (.grantAs "agent" shareRelation)
    let revoked := Household.apply shared (.revokeAs "agent" shareRelation)
    relationChangeAdmitted agent shareFixture "agent" shareRelation false = true ∧
    Household.allowed 5 shared "grantee" "sessions" "agent-session" "read" = true ∧
    relationChangeAdmitted agent shared "sibling" shareRelation true = false ∧
    relationChangeAdmitted agent shared "agent" shareRelation true = true ∧
    Household.allowed 5 revoked "grantee" "sessions" "agent-session" "read" = false := by
  native_decide

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

/-! ### Read-only schema access

The policy delegate holds `get-collection` and no schema change permission, so
the Engineer can change document relationships natively and hand the read on,
but neither change a schema nor grant or revoke a schema change. -/

theorem policy_delegate_holds_no_schema_change :
    schemaChangePermissions.all (fun p => !GrantSet.policyDelegate.permissions.contains p) = true := by
  decide

theorem policy_delegate_holds_get_collection :
    GrantSet.policyDelegate.permissions.contains "get-collection" = true := by decide

theorem engineer_holds_no_schema_change :
    schemaChangePermissions.all (fun p => !(holderOf (receives .defaultAgent)).holds p) = true := by
  decide

theorem engineer_policy_refuses_schema_change :
    (schemaChangePermissions.all fun p => [false, true].all fun d =>
      !policyToolAdmits [policyCategory] (holderOf (receives .defaultAgent))
        ⟨.permission p, d, true, false⟩) = true := by decide

theorem engineer_policy_grants_get_collection :
    policyToolAdmits [policyCategory] (holderOf (receives .defaultAgent))
      ⟨.permission "get-collection", false, true, false⟩ = true := by decide

/-! ### Explaining other identities' node grants

`get-nac-status` lets its holder read any subject's node permissions through
native NAC. It is outside the admin class, so the policy tool may grant it
exactly when the Agent holds it and `add-nac-relation`. -/

theorem engineer_holds_nac_status :
    (holderOf (receives .defaultAgent)).holds "get-nac-status" = true := by decide

theorem nac_status_outside_admin_class : adminClass.contains "get-nac-status" = false := by
  decide

theorem engineer_policy_grants_nac_status :
    policyToolAdmits [policyCategory] (holderOf (receives .defaultAgent))
      ⟨.permission "get-nac-status", false, true, false⟩ = true := by decide

theorem enrolled_user_cannot_grant_nac_status :
    admits (holderOf (receives .enrolledUser))
      ⟨.permission "get-nac-status", false, true, false⟩ = false := by decide

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

/-- Under no-lockout the Engineer keeps `get-collection`: without it, native
document relationship changes outside a transaction are refused, and the policy
tool could no longer manage relations. -/
theorem engineer_cannot_revoke_own_get_collection :
    policyToolAdmitsSelf [policyCategory] (holderOf (receives .defaultAgent)) true true
      ⟨.permission "get-collection", true, true, false⟩ = false := by decide

theorem engineer_can_revoke_baseline_from_others_under_no_lockout :
    policyToolAdmitsSelf [policyCategory] (holderOf (receives .defaultAgent)) true false
      ⟨.permission "read-document", true, true, false⟩ = true := by decide

/-! ### The yolo agent

`gents --yolo` issues no direct grant set: the yolo agent is an ordinary
Agent recipient, holding the projected agent baseline only. Its
self-configuration is a document relation on its own Agent, not a node grant
(`SelfConfig.yoloGroup`). -/

def yoloRecipient : Recipient := .agent

theorem yolo_receives_agent_baseline_only : receives yoloRecipient = [.agentBaseline] := by
  decide

/-- No grant set gives it any of the policy delegation's permissions beyond the
baseline's own `add-dac-relation` and `delete-dac-relation`, which every
Agent holds to link its documents and withdraw those links. Its self-config
Tools still project `delete-document` as a tool-sourced node operation, as the
Engineer's do (`Identity.Configuration.toolOperationSources`); that grants
nothing and confers no document ACP, so it manages no NAC relation and reads no
NAC status. -/
theorem yolo_holds_no_policy_delegate_permission :
    (GrantSet.policyDelegate.permissions.filter
        (fun p => !GrantSet.agentBaseline.permissions.contains p)).all
      (fun p => !(holderOf (receives yoloRecipient)).held.contains p) = true := by
  decide

theorem yolo_holds_no_schema_change_permission :
    ["patch-collection", "set-migration", "add-view"].all
      (fun p => !(holderOf (receives yoloRecipient)).held.contains p) = true := by
  decide

theorem yolo_is_not_admin : (holderOf (receives yoloRecipient)).admin = false := by decide

/-- Under the preset's `self_config_no_lockout` the yolo agent cannot revoke
its own baseline. -/
theorem yolo_cannot_lock_itself_out :
    policyToolAdmitsSelf [policyCategory] (holderOf (receives yoloRecipient)) true true
      ⟨.permission "read-document", true, true, false⟩ = false := by decide

end Identity.GrantSets
