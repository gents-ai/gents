import Proofs.SelfConfig.Types

/-!
# Self-config scope: own configuration versus foreign configuration

An invoking Agent edits its own documents under the self-config category that
owns the document. Any edit that reaches another Agent's configuration, or a
Context or Tools document that another Agent also selects, is foreign and
needs the agent catalog grant (the `node` category). The Agent document itself
is governed by the catalog alone when foreign, because the catalog is the grant
that manages Agents on the node.

Rust owner: `crates/gents/src/tools/builtin/self_config/command/grants.rs`
(`may_edit`, `ensure_edit_scope`).
-/

namespace SelfConfig

/-- Whose configuration an edit touches. -/
inductive Scope where
  | own
  | foreign
  deriving DecidableEq, Repr

/-- Shared documents are foreign even when the invoking Agent is one of their
selectors: an acknowledged shared edit still needs the catalog. -/
def scopeOf (invokingAgent targetAgent : String) (shared : Bool) : Scope :=
  if targetAgent == invokingAgent && !shared then .own else .foreign

def catalogCategory : String := "node"

def agentCategory : String := "agent"

/-- Categories an edit needs, given the category of the edited document. -/
def requiredCategories (category : String) : Scope → List String
  | .own => [category]
  | .foreign =>
      if category == agentCategory then [catalogCategory]
      else [catalogCategory, category]

theorem requiredCategories_foreign (c : String) :
    requiredCategories c .foreign =
      if c == agentCategory then [catalogCategory] else [catalogCategory, c] := rfl

def mayEdit (categories : List String) (category : String) (scope : Scope) : Bool :=
  (requiredCategories category scope).all (fun c => categories.contains c)

/-- The self-config group of an Agent's Tools document: `enabled` gates the
whole tool, and an absent category list selects `defaultCategories`. -/
structure SelfConfigGroup where
  enabled : Bool
  categories : Option (List String)
  deriving DecidableEq, Repr

def effectiveCategories (g : SelfConfigGroup) : List String :=
  if g.enabled then g.categories.getD defaultCategories else []

/-- An Agent created without a clone starts without self-config, and a clone
never inherits its source's group: self-config is an explicit grant from the
configuring holder, not a property copied with the Tools document. -/
def createdGroup : SelfConfigGroup := ⟨false, none⟩

def cloneGroup (_source : SelfConfigGroup) : SelfConfigGroup := createdGroup

/-- The bundled Engineer preset (`engineer-self-config.json`), and the only
group that holds every category out of the box.

Decision: the Engineer holds every self-config category, the agent catalog
(`node`) and the held-bounded `policy` category, so it can configure itself,
configure other Agents and shared documents, and manage relationships through
its `Identity.GrantSets.GrantSet.policyDelegate` set. It is the full
`selfConfigCategories` list by definition, so adding a category gives it to the
Engineer and to nobody else. Agents the Engineer or the User creates or clones
start from `createdGroup`; a later explicit grant gives self-config (own only)
or the catalog. Capability comes from these grants alone: no Agent name or
tag widens them, and none of them is native admin, which only the client key
holds (`Identity.GrantSets.only_init_user_is_admin`). -/
def engineerGroup : SelfConfigGroup :=
  ⟨true, some selfConfigCategories⟩

/-- Creating or cloning an Agent adds an Agent to the node, which is catalog
management. -/
def mayCreateAgent (categories : List String) : Bool :=
  categories.contains catalogCategory

theorem mayEdit_iff (cs : List String) (c : String) (s : Scope) :
    mayEdit cs c s = true ↔ ∀ x ∈ requiredCategories c s, x ∈ cs := by
  unfold mayEdit
  rw [List.all_eq_true]
  simp

theorem own_edit_needs_only_its_category (cs : List String) (c : String) :
    mayEdit cs c .own = cs.contains c := by
  simp [mayEdit, requiredCategories]

theorem own_edit_never_needs_catalog (cs : List String) (c : String)
    (hc : c ≠ catalogCategory) (h : mayEdit cs c .own = true) :
    mayEdit (cs.filter (· != catalogCategory)) c .own = true := by
  rw [own_edit_needs_only_its_category] at h ⊢
  have hm : c ∈ cs := by simpa using h
  have hf : c ∈ cs.filter (· != catalogCategory) :=
    List.mem_filter.mpr ⟨hm, by simpa [bne_iff_ne] using hc⟩
  simpa using hf

theorem foreign_edit_needs_catalog (cs : List String) (c : String)
    (h : mayEdit cs c .foreign = true) : catalogCategory ∈ cs := by
  have hreq : catalogCategory ∈ requiredCategories c .foreign := by
    rw [requiredCategories_foreign]
    split <;> simp
  exact (mayEdit_iff cs c .foreign).mp h catalogCategory hreq

theorem foreign_edit_needs_category (cs : List String) (c : String)
    (hc : c ≠ agentCategory) (h : mayEdit cs c .foreign = true) : c ∈ cs := by
  have hreq : c ∈ requiredCategories c .foreign := by
    rw [requiredCategories_foreign]
    by_cases hA : (c == agentCategory) = true
    · exact absurd (beq_iff_eq.mp hA) hc
    · simp [hA]
  exact (mayEdit_iff cs c .foreign).mp h c hreq

theorem created_agent_holds_no_self_config :
    effectiveCategories createdGroup = [] := rfl

theorem mayEdit_nil (c : String) (s : Scope) : mayEdit [] c s = false := by
  cases hm : mayEdit [] c s
  · rfl
  · have hx : ∃ x, x ∈ requiredCategories c s := by
      cases s with
      | own => exact ⟨c, by simp [requiredCategories]⟩
      | foreign =>
          exact ⟨catalogCategory, by rw [requiredCategories_foreign]; split <;> simp⟩
    obtain ⟨x, hxm⟩ := hx
    have := (mayEdit_iff [] c s).mp hm x hxm
    simp at this

theorem created_agent_edits_nothing (c : String) (s : Scope) :
    mayEdit (effectiveCategories createdGroup) c s = false := by
  rw [created_agent_holds_no_self_config]
  exact mayEdit_nil c s

theorem created_agent_cannot_create_agents :
    mayCreateAgent (effectiveCategories createdGroup) = false := rfl

theorem clone_inherits_no_self_config (source : SelfConfigGroup) :
    effectiveCategories (cloneGroup source) = [] := rfl

theorem clone_of_engineer_edits_nothing (c : String) (s : Scope) :
    mayEdit (effectiveCategories (cloneGroup engineerGroup)) c s = false :=
  created_agent_edits_nothing c s

theorem default_selection_has_no_catalog :
    catalogCategory ∉ effectiveCategories ⟨true, none⟩ := by
  decide

theorem engineer_edits_own_tools_and_catalog :
    mayEdit (effectiveCategories engineerGroup) "tools" .own = true ∧
    mayEdit (effectiveCategories engineerGroup) "tools" .foreign = true ∧
    mayEdit (effectiveCategories engineerGroup) agentCategory .foreign = true ∧
    mayCreateAgent (effectiveCategories engineerGroup) = true := by
  decide

theorem engineer_holds_every_category :
    effectiveCategories engineerGroup = selfConfigCategories := rfl

theorem engineer_holds_catalog_and_policy :
    catalogCategory ∈ effectiveCategories engineerGroup ∧
    policyResourceExposed (effectiveCategories engineerGroup) = true := by
  decide

/-- Holding the agent catalog never stands in for the own category: the
Engineer edits its own documents through each document's category. -/
theorem engineer_edits_own_documents_of_every_category :
    selfConfigCategories.all (fun c => mayEdit (effectiveCategories engineerGroup) c .own) =
      true := by
  decide

theorem own_only_grant_cannot_reach_others :
    mayEdit ["agent", "tools"] "tools" .own = true ∧
    mayEdit ["agent", "tools"] "tools" .foreign = false ∧
    mayEdit ["agent", "tools"] agentCategory .foreign = false := by
  decide

theorem catalog_grant_reaches_foreign_configuration :
    mayEdit ["node", "tools"] "tools" .foreign = true ∧
    mayEdit ["node"] agentCategory .foreign = true := by
  decide

theorem catalog_alone_does_not_grant_own_configuration :
    mayEdit ["node"] "tools" .own = false := by
  decide

theorem scope_is_foreign_when_shared (invoking target : String) :
    scopeOf invoking target true = .foreign := by
  simp [scopeOf]

theorem scope_is_own_only_when_exclusive (invoking : String) :
    scopeOf invoking invoking false = .own := by
  simp [scopeOf]

end SelfConfig
