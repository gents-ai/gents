import Proofs.Tools.ToolPolicy.PluginPublication
import Proofs.Tools.ToolPolicy.Configuration
import Proofs.Tools.ToolPolicy.PluginNetwork
import Proofs.Tools.ToolPolicy.PluginInvocation
import Proofs.Conformance.PluginReceipt

import Proofs.Tools.ToolPolicy.PluginCalls
import Proofs.Tools.ToolPolicy.PluginData
import Proofs.Conformance.PluginEffects
import Proofs.Conformance.Family
import Std.Data.HashMap
import Lean

namespace Conformance.PluginResources
open ToolPolicy Lean

structure LimitConsentInput where
  requested : Nat
  previous : Nat
  consent : Bool
  deriving ToJson

@[conformance_family]
def limitConsent : Family LimitConsentInput Bool where
  inputs := [0, 1, 64, 1536, 4096, 4294967295].flatMap fun requested =>
    [0, 1, 64, 1536, 4096].flatMap fun previous =>
    [false, true].map fun consent => { requested, previous, consent }
  model := fun i => pluginResourceConsented i.requested i.previous i.consent

structure LimitBudgetInput where
  baseline : Nat
  requested : Nat
  ceiling : Nat
  deriving ToJson

@[conformance_family]
def limitBudget : Family LimitBudgetInput Nat where
  inputs := [(64, 4096), (5, 900), (60, 900), (1, 4)].flatMap fun (baseline, ceiling) =>
    [0, 1, baseline, ceiling, ceiling + 1, 4294967295].map fun requested =>
      { baseline, requested, ceiling }
  model := fun i => effectivePluginResource i.baseline i.requested i.ceiling

attribute [conformance_function] ToolPolicy.pluginModelSlotAllowed

section Network
open ToolPolicy.PluginNetwork

private def renderV4 (ip : V4) : String := s!"{ip.a}.{ip.b}.{ip.c}.{ip.d}"

private def renderV6 (ip : V6) : String :=
  ":".intercalate <| [ip.s0, ip.s1, ip.s2, ip.s3, ip.s4, ip.s5, ip.s6, ip.s7].map
    fun n => String.mk (Nat.toDigits 16 n)

private def renderAddr : Addr → String
  | .v4 ip => renderV4 ip
  | .v6 ip => renderV6 ip

private def renderHost : Host → String
  | .name n => n
  | .ip (.v4 ip) => renderV4 ip
  | .ip (.v6 ip) => "[" ++ renderV6 ip ++ "]"

private def renderEntry (e : Entry) : String :=
  let scheme := if e.plaintext then "http://" else ""
  let host := match e.pattern with
    | .exact h => renderHost h
    | .suffix d => "*." ++ d
  let port := match e.port with
    | some p => s!":{p}"
    | none => ""
  scheme ++ host ++ port

private def renderTarget (t : Target) : String :=
  (if t.https then "https://" else "http://") ++ renderHost t.host ++ s!":{t.port}/"

private def v4 (a b c d : Nat) : Addr := .v4 ⟨a, b, c, d⟩
private def v6 (s0 s1 s2 s3 s4 s5 s6 s7 : Nat) : Addr := .v6 ⟨s0, s1, s2, s3, s4, s5, s6, s7⟩
private def publicIp : Addr := v4 93 184 216 34
private def publicIps : List Addr :=
  [publicIp, v6 0x2606 0x4700 0 0 0 0 0 0x1111, v6 0 0 0 0 0 0xffff 0x5db8 0xd822,
   v6 0x2002 0x5db8 0xd822 0 0 0 0 0]
private def loopback6 : Addr := v6 0 0 0 0 0 0 0 1
private def internalIps : List Addr :=
  [v4 127 0 0 1, v4 10 1 2 3, v4 172 20 0 1, v4 192 168 1 1, v4 169 254 169 254,
   v4 100 64 0 1, v4 0 0 0 0, v4 198 18 0 1, v4 224 0 0 1, v4 255 255 255 255,
   loopback6, v6 0 0 0 0 0 0 0 0, v6 0 0 0 0 0 0xffff 0x7f00 1, v6 0xfe80 0 0 0 0 0 0 1,
   v6 0xfc00 0 0 0 0 0 0 1, v6 0xfd12 0 0 0 0 0 0 1, v6 0xff02 0 0 0 0 0 0 1,
   v6 0x2001 0xdb8 0 0 0 0 0 1, v6 0x2001 0 0 0 0 0 0 1, v6 0x2001 0x10 0 0 0 0 0 1,
   v6 0x2001 0x2f 0 0 0 0 0 1, v6 0x2002 0x7f00 1 0 0 0 0 0, v6 0x64 0xff9b 0 0 0 0 0x7f00 1]

private def grants : List Grant :=
  [ .sealed, .anyHost
  , .hosts [⟨.exact (.name "api.example.com"), false, none⟩]
  , .hosts [⟨.suffix "example.com", false, none⟩]
  , .hosts [⟨.exact (.name "api.example.com"), true, some 8080⟩]
  , .hosts [⟨.exact (.ip (v4 127 0 0 1)), true, some 8080⟩]
  , .hosts [⟨.exact (.ip loopback6), true, some 8080⟩]
  , .hosts [⟨.exact (.ip (v4 169 254 169 254)), true, none⟩, ⟨.exact (.name "example.com"), false, none⟩] ]

private def targets : List (Target × List Addr) :=
  let names := ["api.example.com", "example.com", "deep.api.example.com", "other.test"]
  let shapes := [[publicIp], [], [publicIp, v4 10 0 0 5], [publicIp, loopback6]]
    ++ (publicIps ++ internalIps).map ([·])
  let named := names.flatMap fun n => [(true, 443), (false, 80), (true, 8080), (false, 8080)].flatMap
    fun (https, port) => shapes.map fun addrs => (Target.mk https (.name n) port, addrs)
  let literal := (publicIps ++ internalIps).flatMap fun ip =>
    [(true, 443), (false, 80), (false, 8080)].map fun (https, port) =>
      (Target.mk https (.ip ip) port, [ip])
  named ++ literal

inductive GrantScope where
  | any
  deriving ToJson, BEq, Hashable

/-- A plugin's HTTP grant: any host, or the listed host entries. -/
structure HttpGrant where
  scope : Option GrantScope
  hosts : List String
  deriving ToJson, BEq, Hashable

structure NetworkInput where
  grant : Option HttpGrant
  url : String
  addresses : List String
  deriving ToJson, BEq, Hashable

private def grantWire : Grant → Option HttpGrant
  | .sealed => none
  | .anyHost => some ⟨some .any, []⟩
  | .hosts es => some ⟨none, es.map renderEntry⟩

private def networkCases : List (Grant × Target × List Addr) :=
  grants.flatMap fun g => targets.map fun (t, addrs) => (g, t, addrs)

private def networkWire (c : Grant × Target × List Addr) : NetworkInput :=
  { grant := grantWire c.1, url := renderTarget c.2.1, addresses := c.2.2.map renderAddr }

private def networkAllowed : Std.HashMap NetworkInput Bool :=
  networkCases.foldl (fun table (g, t, addrs) => table.insert (networkWire (g, t, addrs)) (allowed g t addrs)) {}

@[conformance_family]
def networkAdmission : Family NetworkInput Bool where
  inputs := networkCases.map networkWire
  model := fun i => networkAllowed.getD i false

private def addressSets : List (List Addr) := (targets.map (·.2)).eraseDups

private def addressesPublic : Std.HashMap (List String) (List Bool) :=
  addressSets.foldl (fun table addrs => table.insert (addrs.map renderAddr) (addrs.map ipPublic)) {}

structure AddressInput where
  addresses : List String
  deriving ToJson

@[conformance_family]
def addressPublic : Family AddressInput (List Bool) where
  inputs := addressSets.map fun addrs => { addresses := addrs.map renderAddr }
  model := fun i => addressesPublic.getD i.addresses []

end Network

deriving instance ToJson for PluginAccess

structure CallAccessInput where
  declared : PluginAccess
  writeFields : Bool
  setsWriteField : Bool
  granted : Option PluginAccess
  deriving ToJson

private def callAccessInputs : List CallAccessInput :=
  [PluginAccess.read, .readWrite].flatMap fun declared =>
  [false, true].flatMap fun writeFields =>
  [false, true].flatMap fun setsWriteField =>
  [none, some PluginAccess.read, some .readWrite].map fun granted =>
    { declared, writeFields, setsWriteField, granted }

structure BindingOutput where
  valid : Bool
  call : PluginAccess
  deriving ToJson

/-- The declaration's validity and the access a call needs are decided on the
declaration alone, so every declared shape is replayed. -/
@[conformance_family]
def callBindings : Family CallAccessInput BindingOutput where
  inputs := callAccessInputs
  model := fun i =>
    { valid := pluginBindingValid i.declared i.writeFields
      call := pluginCallAccess i.declared i.writeFields i.setsWriteField }

structure AdmissionOutput where
  admitted : Bool
  deriving ToJson

/-- An installed plugin's bind admission. Only a valid declaration is installed,
so only valid shapes reach it. -/
@[conformance_family]
def callAdmissions : Family CallAccessInput AdmissionOutput where
  inputs := callAccessInputs.filter fun i => pluginBindingValid i.declared i.writeFields
  model := fun i =>
    { admitted := pluginCallAdmitted i.declared i.granted i.writeFields i.setsWriteField }

/-- model-production-gap: a declaration that names write fields under read
access fails validation, so no plugin with it is installed and bind admission
is never reached for it; the model still decides its admission. -/
@[conformance_family]
def callAdmissionsUnreached : Family CallAccessInput AdmissionOutput where
  inputs := callAccessInputs.filter fun i => !pluginBindingValid i.declared i.writeFields
  model := fun i =>
    { admitted := pluginCallAdmitted i.declared i.granted i.writeFields i.setsWriteField }

deriving instance ToJson for PluginInvocation.Record

structure InvocationInput where
  current : Option PluginInvocation.Record
  restart : Bool
  bound : Bool
  deriving ToJson

structure InvocationOutput where
  admitted : Bool
  reusable : Bool
  deriving ToJson

private def originalInvocation : PluginInvocation.Record :=
  ⟨"artifact", "sealed", "original", "original"⟩

@[conformance_family]
def invocations : Family InvocationInput InvocationOutput where
  inputs := ["same", "removed", "replaced", "grant", "declaration", "binding"].flatMap fun change =>
    [false, true].flatMap fun restart =>
    [false, true].map fun bound =>
      let current : Option PluginInvocation.Record := match change with
        | "removed" => none
        | "replaced" => some { originalInvocation with digest := "replacement" }
        | "grant" => some { originalInvocation with grant := "changed" }
        | "declaration" => some { originalInvocation with declaration := "changed" }
        | "binding" => some { originalInvocation with binding := "changed" }
        | _ => some originalInvocation
      { current, restart, bound }
  model := fun i =>
    { admitted := PluginInvocation.admitted i.current originalInvocation.digest
        (if i.bound then some originalInvocation else none)
      reusable := !i.restart && match i.current with
        | none => false
        | some current => PluginInvocation.reusable originalInvocation current }

section ToolCalls
open PluginCalls

deriving instance ToJson for DispatchRoute, ChildOutcome, Settlement, ConsentRoute

structure LifetimeInput where
  declared : Nat
  background : Bool
  remaining : Option Nat
  deriving ToJson

structure LifetimeOutput where
  hostCallLifetime : Nat
  deriving ToJson

@[conformance_family]
def hostCallLifetimes : Family LifetimeInput LifetimeOutput where
  inputs := [(300, false, none), (300, false, some 100), (300, false, some 600),
    (300, true, some 36000), (300, true, some 0)].map
      fun (declared, background, remaining) => { declared, background, remaining }
  model := fun i => { hostCallLifetime := hostCallLifetime i.declared i.background i.remaining }

structure AllowanceInput where
  declared : Nat
  remaining : Nat
  stopped : Bool
  deriving ToJson

structure AllowanceOutput where
  computeAllowance : Nat
  deriving ToJson

@[conformance_family]
def computeAllowances : Family AllowanceInput AllowanceOutput where
  inputs := [(300, 36000, false), (300, 100, false), (300, 0, false), (300, 36000, true),
    (0, 10, false)].map fun (declared, remaining, stopped) => { declared, remaining, stopped }
  model := fun i => { computeAllowance := computeAllowance i.declared i.remaining i.stopped }

structure SleepInput where
  milliseconds : Nat
  remaining : Nat
  deriving ToJson

structure SleepOutput where
  sleep : Bool
  deriving ToJson

@[conformance_family]
def sleeps : Family SleepInput SleepOutput where
  inputs := [(0, 0), (1, 0), (0, 1), (30, 30), (31, 30), (1000, 60000)].map
    fun (milliseconds, remaining) => { milliseconds, remaining }
  model := fun i => { sleep := sleepAdmissible i.milliseconds i.remaining }

structure RouteInput where
  route : DispatchRoute
  deriving ToJson

structure SupportedOutput where
  supported : Bool
  deriving ToJson

@[conformance_family]
def supportedRoutes : Family RouteInput SupportedOutput where
  inputs := [.ordinary, .hookOwned].map fun route => { route }
  model := fun i => { supported := supported i.route }

/-- `dispatcher` names the scope a call runs in: `live` under the model's
dispatcher, `absent` with none, `child` inside a plugin child's scope. -/
structure AvailabilityInput where
  grant : Bool
  dispatcher : String
  deriving ToJson

structure AvailabilityOutput where
  availability : Bool
  deriving ToJson

private def dispatcherScopes : List (String × Bool) :=
  [("live", true), ("absent", false), ("child", childDispatcher)]

@[conformance_family]
def availabilities : Family AvailabilityInput AvailabilityOutput where
  inputs := [false, true].flatMap fun grant =>
    dispatcherScopes.map fun (dispatcher, _) => { grant, dispatcher }
  model := fun i =>
    { availability := available i.grant ((dispatcherScopes.lookup i.dispatcher).getD false) }

/-- `spawner` is what spawned the background invocation: `accepted` (the
model's own call) or `plugin_child`. -/
structure SpawnerInput where
  spawner : String
  deriving ToJson

structure SpawnedOutput where
  spawnedDispatcher : Bool
  deriving ToJson

@[conformance_family]
def spawnedDispatchers : Family SpawnerInput SpawnedOutput where
  inputs := ["accepted", "plugin_child"].map fun spawner => { spawner }
  model := fun i =>
    { spawnedDispatcher := dispatcherFor
        (if i.spawner == "plugin_child" then .spawnedByPluginChild else .spawnedByAccepted) }

structure ReservationInput where
  used : Nat
  deriving ToJson

structure ReservationOutput where
  reservation : Option Nat
  deriving ToJson

@[conformance_family]
def reservations : Family ReservationInput ReservationOutput where
  inputs := [0, 1, 63, 64, 65, 1000].map fun used => { used }
  model := fun i => { reservation := reserve i.used }

structure BytesInput where
  bytes : Nat
  deriving ToJson

structure BytesOutput where
  resultBytes : Bool
  deriving ToJson

@[conformance_family]
def resultSizes : Family BytesInput BytesOutput where
  inputs := [0, 1048575, 1048576, 1048577].map fun bytes => { bytes }
  model := fun i => { resultBytes := resultFits i.bytes }

structure SettlementInput where
  outcome : ChildOutcome
  deriving ToJson

structure SettlementOutput where
  settlement : Settlement
  deriving ToJson

@[conformance_family]
def settlements : Family SettlementInput SettlementOutput where
  inputs := [.completed, .failed, .unavailable, .timedOut, .cancelled, .fatal].map
    fun outcome => { outcome }
  model := fun i => { settlement := settle i.outcome }

structure ConsentInput where
  interactive : Bool
  composed : Bool
  deriving ToJson

structure ConsentOutput where
  consent : ConsentRoute
  deriving ToJson

@[conformance_family]
def consents : Family ConsentInput ConsentOutput where
  inputs := [false, true].flatMap fun interactive => [false, true].map
    fun composed => { interactive, composed }
  model := fun i => { consent := consentRoute i.interactive i.composed }

end ToolCalls

deriving instance ToJson for PluginCalls.DispatchRoute

attribute [conformance_function] PluginCalls.childAdmissible

inductive PublicationOutcome where
  | localFailure | notificationSuccess | notificationFailure | notificationCancelled
  deriving ToJson

/-- The installation a home shows after an install attempt. -/
inductive VisibleInstallation where
  | new | previous
  deriving ToJson

structure PublicationInput where
  previous : Bool
  outcome : PublicationOutcome
  deriving ToJson

def publicationEvents : PublicationOutcome → List PluginPublication.Event
  | .localFailure => [.localFailure]
  | .notificationSuccess => [.localPublished, .notificationSucceeded]
  | .notificationFailure => [.localPublished, .notificationFailed]
  | .notificationCancelled => [.localPublished, .cancelled]

@[conformance_family]
def publication : Family PublicationInput (Option VisibleInstallation) where
  inputs := [false, true].flatMap fun previous =>
    [PublicationOutcome.localFailure, .notificationSuccess, .notificationFailure,
      .notificationCancelled].map fun outcome => { previous, outcome }
  model := fun i =>
    if PluginPublication.installedNew (publicationEvents i.outcome) then some .new
    else if i.previous then some .previous else none

inductive ImportAnswer where
  | result | noScript
  deriving ToJson

structure HostImportInput where
  declared : Bool
  callHasScript : Bool
  deriving ToJson

structure HostImportObservation where
  linked : Bool
  answer : Option ImportAnswer
  deriving ToJson

/-- `PackPlugin.host_imports`: a plugin that declares `run_script` gets the
`gents` imports linked on every call; one that does not gets none, so a guest
importing them fails to instantiate. The import answers `result` when the call
carries a script and `no_script` when it does not. -/
def hostImportVerdict (i : HostImportInput) : HostImportObservation :=
  { linked := i.declared
    answer := if i.declared then some (if i.callHasScript then .result else .noScript) else none }

@[conformance_family]
def hostImport : Family HostImportInput HostImportObservation where
  inputs := [false, true].flatMap fun declared => [false, true].map fun callHasScript =>
    { declared, callHasScript }
  model := hostImportVerdict

section Scratch

structure ScratchCase where
  cwdAllowed : Bool
  toolCallId : String
  handle : String
  name : String
  size : Nat
  count : Nat

private def scratchCwd : String := "/work/repo"

/-- `handle` is not an owner input: rows that differ only in it share a path. -/
def scratchCases : List ScratchCase :=
  [ ⟨true, "call-1", "fetch", "notes.md", 0, 1⟩, ⟨true, "call-1", "fetch-2", "notes.md", 0, 1⟩
  , ⟨true, "call-2", "fetch", "notes.md", 0, 1⟩, ⟨true, "call-1", "fetch", "a/b", 0, 1⟩
  , ⟨true, "call-1", "fetch", "..", 0, 1⟩, ⟨true, "call-1", "fetch", ".", 0, 1⟩
  , ⟨true, "call-1", "fetch", "", 0, 1⟩, ⟨true, "../call-1", "fetch", "notes.md", 0, 1⟩
  , ⟨true, "..", "fetch", "notes.md", 0, 1⟩
  , ⟨true, "call-1", "fetch", "notes.md", scratchMaxBytes, scratchMaxFiles⟩
  , ⟨true, "call-1", "fetch", "notes.md", scratchMaxBytes + 1, 1⟩
  , ⟨true, "call-1", "fetch", "notes.md", 0, scratchMaxFiles + 1⟩
  , ⟨false, "call-1", "fetch", "notes.md", 0, 1⟩ ]

def scratchVerdict (c : ScratchCase) : Option String :=
  scratchAdmitted scratchCwd (fun _ => c.cwdAllowed) c.toolCallId c.name c.size c.count

structure ScratchInput where
  cwd : String
  cwdAllowed : Bool
  toolCallId : String
  handle : String
  name : String
  size : Nat
  count : Nat
  deriving ToJson

@[conformance_family]
def scratchAdmissions : Family ScratchInput (Option String) where
  inputs := scratchCases.map fun c =>
    { cwd := scratchCwd, cwdAllowed := c.cwdAllowed, toolCallId := c.toolCallId,
      handle := c.handle, name := c.name, size := c.size, count := c.count }
  model := fun i => scratchVerdict ⟨i.cwdAllowed, i.toolCallId, i.handle, i.name, i.size, i.count⟩

theorem job_resource_regressions :
    scratchCases.map scratchVerdict =
      [ some "/work/repo/.gents/workflows/runs/call-1/notes.md"
      , some "/work/repo/.gents/workflows/runs/call-1/notes.md"
      , some "/work/repo/.gents/workflows/runs/call-2/notes.md"
      , none, none, none, none, none, none
      , some "/work/repo/.gents/workflows/runs/call-1/notes.md"
      , none, none, none ] := by
  native_decide

end Scratch

section Data
open PluginData

structure DataWriteCase where
  op : ApplicationWrite.Operation
  inOwnSdl : Bool
  runtimeOrProtected : Bool
  actorIsInvoker : Bool
  opGranted : Bool
  stampedSuppliedByGuest : Bool
  invocation : Invocation
  collectionFields : List String
  write : ApplicationWrite.Observation

private def declaredStamps : List (String × Stamp) :=
  [("job_row_doc_id", .jobRowDocId), ("chain_root_doc_id", .chainRootDocId),
   ("caller_did", .callerDid)]

private def noteFields : List String := ["note_id", "body", "job_row_doc_id", "chain_root_doc_id"]
private def nameFields : List String := ["name", "script", "caller_did"]

private def boundedWrite : ApplicationWrite.Observation :=
  { granted := true, application := true, blocked := false, credential := false, bounded := true,
    targets := 1, limit := 1, preview := false, digestMatches := true }

private def noteWrite : DataWriteCase :=
  { op := .create, inOwnSdl := true, runtimeOrProtected := false, actorIsInvoker := true,
    opGranted := true, stampedSuppliedByGuest := false, invocation := .jobStep,
    collectionFields := noteFields, write := boundedWrite }

/-- A note collection carries the job stamps; a name collection carries only
`caller_did`, so job stamps do not apply to it. -/
def dataWriteCases : List DataWriteCase :=
  [ noteWrite, { noteWrite with inOwnSdl := false }, { noteWrite with runtimeOrProtected := true }
  , { noteWrite with actorIsInvoker := false }, { noteWrite with opGranted := false }
  , { noteWrite with stampedSuppliedByGuest := true }
  , { noteWrite with invocation := .callback }, { noteWrite with invocation := .toolCall }
  , { noteWrite with invocation := .callback, collectionFields := nameFields }
  , { noteWrite with invocation := .ux, collectionFields := nameFields }
  , { noteWrite with invocation := .ux }
  , { noteWrite with invocation := .toolCall, collectionFields := nameFields }
  , { noteWrite with write := { boundedWrite with targets := 101, limit := 100 } } ]

def dataWriteVerdict (c : DataWriteCase) : List (String × Stamp) × Bool × Option Refusal :=
  let o : Observation :=
    { inOwnSdl := c.inOwnSdl, runtimeOrProtected := c.runtimeOrProtected,
      actorIsInvoker := c.actorIsInvoker, opGranted := c.opGranted,
      stampedSuppliedByGuest := c.stampedSuppliedByGuest, invocation := c.invocation,
      applicable := applicableStamps declaredStamps c.collectionFields, write := c.write }
  (o.applicable, admitted c.op o, refusal c.op o)

def dataQueryCases : List (Nat × Bool × Bool × Bool) :=
  [0, 1, 256, 257].map (·, true, false, true) ++
    [(1, false, false, true), (1, true, true, true), (1, true, false, false)]

def dataQueryVerdict (c : Nat × Bool × Bool × Bool) : Bool :=
  queryAdmitted c.1 c.2.1 c.2.2.1 c.2.2.2

private def callerDid : String := "did:key:agent"

def dataFilterCases : List Filter :=
  [ [("owner", .callerDid), ("name", .literal "draft")]
  , [("owner", .literal "did:key:guest")], [] ]

def dataFilterVerdict (f : Filter) : Filter := substituteCallerDid f callerDid

def dataNoteCases : List (Option Fingerprint × Fingerprint × OnConflict) :=
  [none, some "fp-a"].flatMap fun existing => ["fp-a", "fp-b"].flatMap fun new =>
    [OnConflict.ignore, .fail].map (existing, new, ·)

def dataNoteVerdict (c : Option Fingerprint × Fingerprint × OnConflict) : InsertOutcome :=
  insertNote c.1 c.2.1 c.2.2

deriving instance ToJson for ApplicationWrite.Operation, ApplicationWrite.Observation, Invocation,
  Stamp, OnConflict, InsertOutcome

structure StampField where
  field : String
  stamp : Stamp
  deriving ToJson

private def stampFields (stamps : List (String × Stamp)) : List StampField :=
  stamps.map fun (field, stamp) => { field, stamp }

structure WriteInput where
  op : ApplicationWrite.Operation
  inOwnSdl : Bool
  runtimeOrProtected : Bool
  actorIsInvoker : Bool
  opGranted : Bool
  stampedSuppliedByGuest : Bool
  invocation : Invocation
  declaredStamps : List StampField
  collectionFields : List String
  write : ApplicationWrite.Observation
  deriving ToJson

/-- `kind` is `stamp_unavailable` (with its stamp and invocation) or `other`. -/
structure RefusalWire where
  kind : String
  stamp : Option Stamp
  invocation : Option Invocation
  deriving ToJson

private def refusalWire : Refusal → RefusalWire
  | .stampUnavailable s inv => ⟨"stamp_unavailable", some s, some inv⟩
  | .other => ⟨"other", none, none⟩

structure WriteOutput where
  applicable : List StampField
  admitted : Bool
  refusal : Option RefusalWire
  deriving ToJson

@[conformance_family]
def dataWrites : Family WriteInput WriteOutput where
  inputs := dataWriteCases.map fun c =>
    { op := c.op, inOwnSdl := c.inOwnSdl, runtimeOrProtected := c.runtimeOrProtected,
      actorIsInvoker := c.actorIsInvoker, opGranted := c.opGranted,
      stampedSuppliedByGuest := c.stampedSuppliedByGuest, invocation := c.invocation,
      declaredStamps := stampFields declaredStamps, collectionFields := c.collectionFields,
      write := c.write }
  model := fun i =>
    let (applicable, ok, refused) := dataWriteVerdict
      { op := i.op, inOwnSdl := i.inOwnSdl, runtimeOrProtected := i.runtimeOrProtected,
        actorIsInvoker := i.actorIsInvoker, opGranted := i.opGranted,
        stampedSuppliedByGuest := i.stampedSuppliedByGuest, invocation := i.invocation,
        collectionFields := i.collectionFields, write := i.write }
    { applicable := stampFields applicable, admitted := ok, refusal := refused.map refusalWire }

structure QueryInput where
  limit : Nat
  inOwnSdl : Bool
  runtimeOrProtected : Bool
  queryGranted : Bool
  deriving ToJson

@[conformance_family]
def dataQueries : Family QueryInput Bool where
  inputs := dataQueryCases.map fun (limit, inOwnSdl, runtimeOrProtected, queryGranted) =>
    { limit, inOwnSdl, runtimeOrProtected, queryGranted }
  model := fun i => dataQueryVerdict (i.limit, i.inOwnSdl, i.runtimeOrProtected, i.queryGranted)

/-- `$caller.did` is the placeholder's wire spelling. -/
structure FilterEntry where
  field : String
  value : String
  deriving ToJson

private def filterEntries (f : Filter) : List FilterEntry :=
  f.map fun (field, value) =>
    { field, value := match value with | .literal v => v | .callerDid => "$caller.did" }

private def filterOf (entries : List FilterEntry) : Filter :=
  entries.map fun e =>
    (e.field, if e.value == "$caller.did" then .callerDid else .literal e.value)

structure FilterInput where
  callerDid : String
  filter : List FilterEntry
  deriving ToJson

@[conformance_family]
def dataFilters : Family FilterInput (List FilterEntry) where
  inputs := dataFilterCases.map fun f => { callerDid, filter := filterEntries f }
  model := fun i => filterEntries (substituteCallerDid (filterOf i.filter) i.callerDid)

structure NoteInput where
  existing : Option String
  new : String
  onConflict : OnConflict
  deriving ToJson

@[conformance_family]
def dataNotes : Family NoteInput InsertOutcome where
  inputs := dataNoteCases.map fun (existing, new, onConflict) => { existing, new, onConflict }
  model := fun i => dataNoteVerdict (i.existing, i.new, i.onConflict)

private def noteStamps : List (String × Stamp) :=
  [("job_row_doc_id", .jobRowDocId), ("chain_root_doc_id", .chainRootDocId)]

theorem data_regressions :
    dataWriteCases.map dataWriteVerdict =
      [ (noteStamps, true, none), (noteStamps, false, some .other)
      , (noteStamps, false, some .other), (noteStamps, false, some .other)
      , (noteStamps, false, some .other), (noteStamps, false, some .other)
      , (noteStamps, false, some (.stampUnavailable .jobRowDocId .callback))
      , (noteStamps, false, some (.stampUnavailable .jobRowDocId .toolCall))
      , ([("caller_did", .callerDid)], true, none), ([("caller_did", .callerDid)], true, none)
      , (noteStamps, false, some (.stampUnavailable .jobRowDocId .ux))
      , ([("caller_did", .callerDid)], true, none)
      , (noteStamps, false, some .other) ] ∧
    dataQueryCases.map dataQueryVerdict = [false, true, true, false, false, false, false] ∧
    dataFilterCases.map dataFilterVerdict =
      [ [("owner", .literal "did:key:agent"), ("name", .literal "draft")]
      , [("owner", .literal "did:key:guest")], [] ] ∧
    dataNoteCases.map dataNoteVerdict =
      [ .inserted, .inserted, .inserted, .inserted
      , .unchanged, .unchanged, .unchanged, .abortStep ] := by
  native_decide

end Data

def routeCases : List (Bool × Bool × Bool × Bool) :=
  [false, true].flatMap fun staticHookOwned => [false, true].flatMap fun jobBound =>
    [false, true].flatMap fun selected => [false, true].map fun admitted =>
      (staticHookOwned, jobBound, selected, admitted)

def routeVerdict (c : Bool × Bool × Bool × Bool) : Bool :=
  PluginCalls.childAdmissible c.2.2.1 (PluginCalls.routeOf c.1 c.2.1) c.2.2.2

theorem route_regressions :
    routeCases.map routeVerdict =
      [ false, false, false, true, false, false, false, true
      , false, false, false, true, false, false, false, true ] := by
  native_decide

structure ChildRouteInput where
  staticHookOwned : Bool
  jobBound : Bool
  selected : Bool
  admitted : Bool
  deriving ToJson

/-- The child route a plugin's tool call takes: a static hook-owned or
job-bound tool still runs only when it is on the selected surface and its own
admission accepts the call. -/
@[conformance_family]
def childRoutes : Family ChildRouteInput Bool where
  inputs := routeCases.map fun (staticHookOwned, jobBound, selected, admitted) =>
    { staticHookOwned, jobBound, selected, admitted }
  model := fun i => routeVerdict (i.staticHookOwned, i.jobBound, i.selected, i.admitted)

end Conformance.PluginResources

namespace Conformance.PluginScratch

open Lean ToolPolicy

structure Write where
  toolCallId : String
  name : String
  size : Nat
  deriving ToJson

structure Trace where
  cwd : String
  writes : List Write
  deriving ToJson

structure WrittenFile where
  path : String
  write : Nat
  deriving ToJson

structure Outcome where
  written : List Bool
  files : List WrittenFile
  deriving ToJson

private def at6 (toolCallId name : String) : Write := { toolCallId, name, size := 6 }

/-- Scratch writes in order on one cwd, each one call of the row's scratch
writer; the files are listed in path order. -/
@[conformance_family]
def writes : Family Trace Outcome where
  inputs :=
    [ { cwd := "/work/repo", writes := [at6 "call-1" "notes.md", at6 "call-2" "notes.md"] }
    , { cwd := "/work/repo", writes := [at6 "call-1" "notes.md", at6 "call-1" "notes.md"] }
    , { cwd := "/work/repo"
        writes := [at6 "call-1" "notes.md", at6 "call-1" "other.md", at6 "call-1" "..",
          at6 ".." "notes.md", at6 "call-2" "a/b"] }
    , { cwd := "/work/repo"
        writes := [at6 "call-1" "notes.md",
          { toolCallId := "call-1", name := "big.bin", size := scratchMaxBytes + 1 },
          { toolCallId := "call-1", name := "edge.bin", size := scratchMaxBytes }] } ]
  model := fun t =>
    let (written, files) := scratchWritesFrom t.cwd (fun _ => true) 0 []
      (t.writes.map fun w => (w.toolCallId, w.name, w.size))
    { written
      files := (files.mergeSort fun a b => !(decide (b.path < a.path))).map fun f =>
        { path := f.path, write := f.write } }

end Conformance.PluginScratch
