import Proofs.Identity.State

namespace Configuration.PackIdentity

/-- Required installation contract: authored slots select public bindings saved in
PackInstallation. They are not key-derivation inputs. Provisioning occurs before
this plan and the client's configuration transaction must persist `recorded`
with the selected documents. An authorized Agent installation uses the same fresh
binding plan as a user installation and publishes each Agent under its own actor;
its Node relationship preserves the Node owner's control. The runtime alone derives
keys and grants capabilities after native publication. This model does not establish
that native publication is atomic; native conformance must exercise that transaction owner.
At the start of a pack operation, the runtime observes its Node enabled state and
physical binding through its own reader grant and passes that metadata to preparation.
The observation is separate from the later write and cannot fence a subsequent disable.
It does not grant the executing Agent access to Node or inference backend credentials. Removal,
restoration and tombstone policy are deliberately outside this operation. -/
structure Binding where
  slot : String
  creationNonce : Identity.CreationId
  agentDid : Identity.DID
  deriving DecidableEq, Repr

structure Plan where
  selected : List Binding
  recorded : List Binding
  deriving DecidableEq, Repr

def distinct (values : List String) : Bool := decide values.Nodup

def valid (bindings : List Binding) : Bool :=
  distinct (bindings.map (·.slot)) && distinct (bindings.map (·.creationNonce)) &&
    distinct (bindings.map (·.agentDid)) &&
    bindings.all (fun binding => binding.slot != "" && binding.creationNonce != "" &&
      binding.agentDid != "")

def choose (recorded provisioned : List Binding) (slot : String) : Option Binding :=
  (recorded.find? (fun binding => binding.slot == slot)).orElse
    (fun _ => provisioned.find? (fun binding => binding.slot == slot))

/-- Existing slots retain their binding even if a caller supplies replacement
material. Newly requested slots require provisioned material. Unrequested saved
slots remain recorded; this operation is not an uninstall or identity deletion. -/
def plan (recorded : List Binding) (requested : List String)
    (provisioned : List Binding) : Option Plan := do
  if !(valid recorded && valid provisioned && distinct requested) then none else do
    let selected ← requested.mapM (choose recorded provisioned)
    let added := selected.filter (fun binding =>
      !(recorded.any (fun prior => prior.slot == binding.slot)))
    let next := recorded ++ added
    if valid next then some ⟨selected, next⟩ else none

theorem existing_slot_ignores_replacement (recorded provisioned : List Binding)
    (slot : String) (binding : Binding)
    (found : recorded.find? (fun prior => prior.slot == slot) = some binding) :
    choose recorded provisioned slot = some binding := by
  simp [choose, found]

def first : Binding := ⟨"worker", String.mk (List.replicate 64 'a'),
  "did:key:z6MkeXBLjYiSvqnhFb6D7sHm8yKm4jV45wwBFRaatf1cfZ76"⟩
def second : Binding := ⟨"reviewer", String.mk (List.replicate 64 'b'),
  "did:key:z6Mkeb6dsrBTX95vPgLiZAcgRr6XJthFm1czoqFEx34DQtRo"⟩
def replacement : Binding := ⟨"worker", String.mk (List.replicate 64 'c'),
  "did:key:z6Mkef1w29eU7SP9XmbDzTwbiisHZ3uTS5JpNEuu1R6pADkW"⟩

structure Case where
  name : String
  recorded : List Binding
  requested : List String
  provisioned : List Binding
  deriving Repr

def cases : List Case :=
  [ ⟨"new-install-uses-provisioned-binding", [], ["worker"], [first]⟩
  , ⟨"upgrade-keeps-existing-binding", [first], ["worker"], []⟩
  , ⟨"replacement-does-not-rotate-existing-slot", [first], ["worker"], [replacement]⟩
  , ⟨"new-slot-extends-installation", [first], ["worker", "reviewer"], [second]⟩
  , ⟨"new-install-with-same-slot-gets-new-binding", [], ["worker"], [replacement]⟩
  , ⟨"missing-provisioning-rejected", [], ["worker"], []⟩
  , ⟨"duplicate-requested-slot-rejected", [], ["worker", "worker"], [first]⟩
  , ⟨"ambiguous-recorded-slot-rejected", [first, replacement], ["worker"], []⟩
  , ⟨"duplicate-agent-did-rejected", [first], ["reviewer"], [{ second with agentDid := first.agentDid }]⟩
  , ⟨"duplicate-creation-nonce-rejected", [first], ["reviewer"], [{ second with creationNonce := first.creationNonce }]⟩
  , ⟨"empty-selection-does-not-uninstall", [first], [], []⟩ ]

theorem case_acceptance_pinned :
    cases.map (fun c => (plan c.recorded c.requested c.provisioned).isSome) =
      [true, true, true, true, true, false, false, false, false, false, true] := by
  native_decide

theorem upgrade_preserves_binding : plan [first] ["worker"] [replacement] =
    some ⟨[first], [first]⟩ := by native_decide

theorem fresh_install_does_not_reuse_previous_binding :
    plan [] ["worker"] [replacement] = some ⟨[replacement], [replacement]⟩ ∧
    replacement.agentDid ≠ first.agentDid := by native_decide

/-- Qualified references use only declared direct dependencies, after coordinates are
normalized by the existing pack loader. Pre-install eval subjects, trials and
scenario selections name authored agent slots; only the installed binding map
resolves those slots into Agent DIDs. An authored slot is never a persisted Agent
reference or a derivation input. Dependencies are prepared exactly once and
retain their own installation records. Resolving a reference never copies a foreign
binding into the importing pack's recorded map or changes that identity. -/
structure DependencyBindings where
  coordinate : String
  bindings : List Binding
  deriving DecidableEq, Repr

def resolveDependency (declared : List String) (available : List DependencyBindings)
    (dependency slot : String) : Option Binding :=
  if !declared.contains dependency then none else
  match available.filter (fun entry => entry.coordinate == dependency) with
  | [entry] => if valid entry.bindings then entry.bindings.find? (fun b => b.slot == slot) else none
  | _ => none

structure ReferenceCase where
  name : String
  declared : List String
  available : List DependencyBindings
  dependency : String
  slot : String
  deriving Repr

def referenceCases : List ReferenceCase :=
  [ ⟨"direct-dependency-reference", ["family/tools"], [⟨"family/tools", [first]⟩], "family/tools", "worker"⟩
  , ⟨"undeclared-dependency-rejected", [], [⟨"family/tools", [first]⟩], "family/tools", "worker"⟩
  , ⟨"transitive-dependency-rejected", ["family/wrapper"], [⟨"family/tools", [first]⟩], "family/tools", "worker"⟩
  , ⟨"unprepared-dependency-rejected", ["family/tools"], [], "family/tools", "worker"⟩
  , ⟨"unknown-slot-rejected", ["family/tools"], [⟨"family/tools", [first]⟩], "family/tools", "missing"⟩
  , ⟨"same-slot-keeps-dependency-identity", ["family/tools", "family/other"],
      [⟨"family/tools", [first]⟩, ⟨"family/other", [replacement]⟩], "family/other", "worker"⟩
  , ⟨"ambiguous-dependency-rejected", ["family/tools"],
      [⟨"family/tools", [first]⟩, ⟨"family/tools", [replacement]⟩], "family/tools", "worker"⟩
  , ⟨"ambiguous-slot-rejected", ["family/tools"], [⟨"family/tools", [first, replacement]⟩], "family/tools", "worker"⟩ ]

theorem reference_acceptance_pinned : referenceCases.map (fun c =>
    (resolveDependency c.declared c.available c.dependency c.slot).isSome) =
    [true, false, false, false, false, true, false, false] := by native_decide

theorem dependency_retains_its_binding :
    resolveDependency ["family/tools", "family/other"]
      [⟨"family/tools", [first]⟩, ⟨"family/other", [replacement]⟩] "family/other" "worker" =
      some replacement := by native_decide

/-- An eval reference resolves only the selected subject slot. Its binding is
retained as evidence so capture and grading use the same installation identity. -/
def resolveEvalSubject (selected : Option Binding) (slot : String) : Option Identity.DID := do
  let binding ← selected
  if binding.slot == slot && binding.slot != "" && binding.agentDid != "" then
    some binding.agentDid
  else none

structure EvalSubjectCase where
  name : String
  selected : Option Binding
  slot : String
  deriving Repr

def evalSubjectCases : List EvalSubjectCase :=
  [ ⟨"selected", some first, "worker"⟩
  , ⟨"missing-binding", none, "worker"⟩
  , ⟨"foreign-slot", some first, "reviewer"⟩
  , ⟨"empty-slot", some first, ""⟩
  , ⟨"empty-did", some { first with agentDid := "" }, "worker"⟩ ]

theorem eval_subject_acceptance : evalSubjectCases.map (fun c =>
    (resolveEvalSubject c.selected c.slot).isSome) = [true, false, false, false, false] := by
  native_decide

end Configuration.PackIdentity
