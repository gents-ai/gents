import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family
import Proofs.Tools.SelfConfig.Cases
import Proofs.Tools.SelfConfig.Auth
import Proofs.Tools.SelfConfig.Scope
import Proofs.Store.Identity.GrantSets

namespace Conformance.Contracts

open SelfConfig SelfConfig.ContractCases

private def scBool (b : Bool) : String :=
  if b then "true" else "false"

structure FieldTableInput where
  collection : String
  deriving Lean.ToJson

structure FieldTable where
  uniqueField : String
  category : String
  allFields : List String
  writableFields : List String
  protectedFields : List String
  deriving Lean.ToJson

def fieldTable (t : Target) : FieldTable :=
  { uniqueField := t.uniqueField, category := t.category, allFields := allFields t
    writableFields := writableFields t, protectedFields := protectedFields t }

@[conformance_family]
def selfConfigFieldTable : Family FieldTableInput (Option FieldTable) where
  inputs := allTargets.map fun t => { collection := t.collectionName }
  model := fun i => (allTargets.find? (·.collectionName == i.collection)).map fieldTable

structure ResourceReadExposureInput where
  resource : String
  categories : List String
  deriving Lean.ToJson

structure ResourceReadExposureOutput where
  exposed : Bool
  deriving Lean.ToJson

/-- The configuration document each config-tool read resource lists; the
`effective` read has none and is exposed by `effectiveReadExposed`. -/
def readResourceTarget : String → Option Target
  | "backend" => some .inferenceBackend
  | "tools" => some .tools
  | "profile" => some .inferenceProfile
  | "automation" => some .task
  | _ => none

@[conformance_family]
def selfConfigResourceReadExposure :
    Conformance.Family ResourceReadExposureInput ResourceReadExposureOutput where
  inputs :=
    [("backend", ["backend"]), ("backend", ["backend", "node"]), ("backend", ["backend", "agent"]),
     ("backend", ["node"]), ("backend", ["agent"]), ("tools", ["tools"]),
     ("profile", ["profile"]), ("automation", ["automation"]), ("automation", ["backend"]),
     ("effective", ["backend"]), ("effective", ["automation"]), ("effective", ["tools"])].map
      fun (resource, categories) => { resource, categories }
  model := fun i =>
    { exposed := match readResourceTarget i.resource with
        | some target => resourceReadExposed i.categories target
        | none => effectiveReadExposed i.categories }

structure PolicyResourceExposureInput where
  categories : List String
  deriving Lean.ToJson

@[conformance_family]
def policyResourceExposure : Family PolicyResourceExposureInput Bool where
  inputs := ([[], ["policy"], ["agent", "tools", "profile"], ["node", "policy"],
    ["backend", "mcp_service", "automation", "policy"]] : List (List String)).map
      fun categories => { categories }
  model := fun i => policyResourceExposed i.categories

def selfConfigPatchEntryJson (entry : FieldKey × Option FieldValue) : String :=
  "{"
    ++ "\"field\":" ++ jsonString entry.1 ++ ","
    ++ "\"action\":"
      ++ (match entry.2 with
          | some _ => jsonString "set"
          | none => jsonString "clear") ++ ","
    ++ "\"value\":"
      ++ (match entry.2 with
          | some v => jsonString v
          | none => "null")
  ++ "}"

structure PolicyGrantBoundInput where
  candidatePolicy : Bool
  storedPolicy : Bool
  heldPolicy : Bool
  deriving Lean.ToJson

/-- Every combination of the policy grant bit on the candidate, the stored Tools and
the invoker, with pack installation off. -/
@[conformance_family]
def policyGrantBound : Family PolicyGrantBoundInput Bool where
  inputs := [false, true].flatMap fun candidatePolicy => [false, true].flatMap fun storedPolicy =>
    [false, true].map fun heldPolicy => { candidatePolicy, storedPolicy, heldPolicy }
  model := fun i =>
    ({ Grants.bot with policy := i.candidatePolicy } : Grants).boundedBy
      { Grants.bot with policy := i.storedPolicy } { Grants.bot with policy := i.heldPolicy }

structure LockoutRow where
  permission : String
  delete : Bool
  noLockout : Bool
  isSelf : Bool
  deriving Lean.ToJson

structure LockoutInput where
  rows : List LockoutRow
  deriving Lean.ToJson

structure LockoutOutput where
  «protected» : List String
  kept : List Bool
  deriving Lean.ToJson

/-- The node permissions a no-lockout Agent keeps against its own revoke (as a
sorted set), and for each permission and change whether the lockout guard
keeps it. -/
@[conformance_family]
def policyLockout : Family LockoutInput LockoutOutput where
  inputs :=
    let permissions := (Identity.GrantSets.lockoutProtected ++ ["patch-collection"]).eraseDups
    [{ rows := permissions.flatMap fun permission =>
        [false, true].flatMap fun delete =>
          [false, true].flatMap fun noLockout =>
            [false, true].map fun isSelf => { permission, delete, noLockout, isSelf } }]
  model := fun i =>
    { «protected» := Identity.GrantSets.lockoutProtected.eraseDups.mergeSort fun a b => a ≤ b
      kept := i.rows.map fun row =>
        Identity.GrantSets.selfChangeKeepsLockout row.noLockout row.isSelf
          ⟨.permission row.permission, row.delete, true, false⟩ }

structure FieldValueWire where
  field : String
  value : String
  deriving Lean.ToJson

structure PatchEntryWire where
  field : String
  action : String
  value : Option String
  deriving Lean.ToJson

structure BackendWire where
  backendId : String
  providerKind : String
  auth : String
  deriving Lean.ToJson

structure GrantsWire where
  packInstall : Bool
  policy : Bool
  packAuthority : Bool
  deriving Lean.ToJson

structure SelfConfigCaseInput where
  name : String
  collection : String
  guarded : Bool
  validates : Bool
  heldGrants : GrantsWire
  selectedBefore : Option (List FieldValueWire)
  selectedAfter : Option (List FieldValueWire)
  doc : List FieldValueWire
  patch : List PatchEntryWire
  backends : List BackendWire
  deriving Lean.ToJson

structure SelfConfigCaseOutput where
  admissible : Bool
  accepted : Bool
  result : List FieldValueWire
  deriving Lean.ToJson

private def fieldValues (doc : List (FieldKey × FieldValue)) : List FieldValueWire :=
  doc.map fun (field, value) => { field, value }

def selfConfigCaseInput (w : CaseWitness) : SelfConfigCaseInput :=
  { name := w.row.name, collection := w.row.target.collectionName
    guarded := w.row.guarded, validates := w.row.validates
    heldGrants := ⟨w.row.held.packInstall, w.row.held.policy, w.row.held.packAuthority⟩
    selectedBefore := w.selectedBefore.map fieldValues
    selectedAfter := w.selectedAfter.map fieldValues
    doc := fieldValues w.row.doc
    patch := w.row.patch.map fun (field, value) =>
      { field, action := if value.isSome then "set" else "clear", value }
    backends := w.row.backends.map fun (backendId, providerKind, auth) =>
      { backendId, providerKind, auth } }

/-- The witness of the named scenario row: the row's target, documents, guard
fixtures and held grants are all fixed by the row, and the input carries their
native projection. -/
def selfConfigCaseOutcome (i : SelfConfigCaseInput) : Option SelfConfigCaseOutput :=
  (selfConfigCases.find? (·.row.name == i.name)).map fun w =>
    { admissible := w.admissiblePatch, accepted := w.accepted, result := fieldValues w.result }

@[conformance_family]
def selfConfigCase : Family SelfConfigCaseInput (Option SelfConfigCaseOutput) where
  inputs := selfConfigCases.map selfConfigCaseInput
  model := selfConfigCaseOutcome

/-- The served-node scenario stores a Backend whose auth is a credential
reference with its credential document and then attempts the raw-key patch;
the self-config fixture provisions only that credential, so only this row can
be built there. -/
@[conformance_family]
def selfConfigCaseRawKeyScenario : Family SelfConfigCaseInput (Option Bool) where
  inputs := (selfConfigCases.filter (·.row.name == "backend_credential_to_raw_key_rejected")).map
    selfConfigCaseInput
  model := fun i => (selfConfigCaseOutcome i).map (·.accepted)

structure SchemaPublicationInput where
  grant : Bool
  artifact : Bool
  compatible : Bool
  nativeAllowed : Bool
  deriving Lean.ToJson

structure SchemaPublicationOutput where
  accepted : Bool
  installed : Bool
  deriving Lean.ToJson

def schemaPublicationInputs : List SchemaPublicationInput :=
  [false, true].flatMap fun grant => [false, true].flatMap fun artifact =>
    [false, true].flatMap fun compatible => [false, true].map fun nativeAllowed =>
      { grant, artifact, compatible, nativeAllowed }

def schemaPublicationOutcome (i : SchemaPublicationInput) : SchemaPublicationOutput :=
  let accepted := SelfConfig.schemaPublicationAllowed i.grant i.artifact i.compatible i.nativeAllowed
  { accepted, installed := accepted }

@[conformance_family]
def schemaPublication : Family SchemaPublicationInput SchemaPublicationOutput where
  inputs := schemaPublicationInputs
  model := schemaPublicationOutcome

/-- The runtime-owned schema tool acts with the node's own authority, which
natively holds every collection permission, so only natively allowed
publications can be built for it. -/
@[conformance_family]
def runtimeSchemaPublication : Family SchemaPublicationInput SchemaPublicationOutput where
  inputs := schemaPublicationInputs.filter (·.nativeAllowed)
  model := schemaPublicationOutcome

structure EditInput where
  name : String
  categories : List String
  category : String
  scope : String
  deriving Lean.ToJson

structure GroupInput where
  name : String
  enabled : Bool
  categories : Option (List String)
  deriving Lean.ToJson

structure ScopeOfInput where
  name : String
  invokingAgent : String
  targetAgent : String
  shared : Bool
  deriving Lean.ToJson

structure ScopeInput where
  edit : List EditInput
  groups : List GroupInput
  scopeOf : List ScopeOfInput
  deriving Lean.ToJson

structure GroupOutput where
  effective : List String
  mayCreateAgent : Bool
  deriving Lean.ToJson

structure PresetOutput where
  enabled : Bool
  effective : List String
  editsOwnTools : Bool
  createsAgents : Bool
  deriving Lean.ToJson

structure ScopeOutput where
  categories : List String
  defaultCategories : List String
  allowed : List Bool
  groups : List GroupOutput
  scope : List String
  engineer : PresetOutput
  yolo : PresetOutput
  deriving Lean.ToJson

private def sortedNames (names : List String) : List String :=
  names.eraseDups.mergeSort fun a b => a ≤ b

private def scopeName (scope : SelfConfig.Scope) : String :=
  if scope == .own then "own" else "foreign"

def presetOutput (group : SelfConfig.SelfConfigGroup) : PresetOutput :=
  let effective := SelfConfig.effectiveCategories group
  { enabled := group.enabled, effective := sortedNames effective
    editsOwnTools := SelfConfig.mayEdit effective "tools" .own
    createsAgents := SelfConfig.mayCreateAgent effective }

/-- Self-config edit authority by category and scope, the effective categories
of a group, whose configuration an edit touches, the category vocabulary and the
bundled Engineer and yolo presets, replayed as one table. -/
@[conformance_family]
def selfConfigScope : Family ScopeInput ScopeOutput where
  inputs :=
    let categorySets : List (String × List String) :=
      [("none", []), ("agent-only", ["agent"]), ("tools-only", ["tools"]),
       ("catalog-only", ["node"]), ("catalog-and-agent", ["node", "agent"]),
       ("catalog-and-tools", ["node", "tools"]), ("agent-and-tools", ["agent", "tools"]),
       ("engineer", SelfConfig.effectiveCategories SelfConfig.engineerGroup)]
    let documents := ["agent", "tools", "profile", "backend", "automation", "policy"]
    let groups : List (String × SelfConfig.SelfConfigGroup) :=
      [("created", SelfConfig.createdGroup), ("clone-of-engineer", SelfConfig.cloneGroup SelfConfig.engineerGroup),
       ("engineer", SelfConfig.engineerGroup), ("yolo", SelfConfig.yoloGroup),
       ("enabled-default", ⟨true, none⟩),
       ("disabled-with-list", ⟨false, some ["node", "tools"]⟩),
       ("enabled-catalog", ⟨true, some ["node"]⟩)]
    [{ edit := categorySets.flatMap fun (setName, categories) =>
          documents.flatMap fun document =>
            ["own", "foreign"].map fun scope =>
              { name := setName ++ "/" ++ document ++ "/" ++ scope, categories, category := document, scope }
       groups := groups.map fun (name, g) => { name, enabled := g.enabled, categories := g.categories }
       scopeOf :=
         [("self-exclusive", "agent-a", "agent-a", false), ("self-shared", "agent-a", "agent-a", true),
          ("other-exclusive", "agent-a", "agent-b", false), ("other-shared", "agent-a", "agent-b", true)].map
           fun (name, invokingAgent, targetAgent, shared) => { name, invokingAgent, targetAgent, shared } }]
  model := fun i =>
    { categories := sortedNames SelfConfig.selfConfigCategories
      defaultCategories := sortedNames SelfConfig.defaultCategories
      allowed := i.edit.map fun e =>
        SelfConfig.mayEdit e.categories e.category (if e.scope == "own" then .own else .foreign)
      groups := i.groups.map fun g =>
        let effective := SelfConfig.effectiveCategories ⟨g.enabled, g.categories⟩
        { effective := sortedNames effective, mayCreateAgent := SelfConfig.mayCreateAgent effective }
      scope := i.scopeOf.map fun c => scopeName (SelfConfig.scopeOf c.invokingAgent c.targetAgent c.shared)
      engineer := presetOutput SelfConfig.engineerGroup
      yolo := presetOutput SelfConfig.yoloGroup }

end Conformance.Contracts
