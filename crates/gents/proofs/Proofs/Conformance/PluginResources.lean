import Proofs.ToolPolicy.PluginPublication
import Proofs.ToolPolicy.Configuration
import Proofs.ToolPolicy.PluginNetwork
import Proofs.ToolPolicy.PluginInvocation
import Proofs.Conformance.PluginReceipt

import Proofs.ToolPolicy.PluginCalls
import Proofs.ToolPolicy.PluginData
import Proofs.Conformance.PluginEffects
import Lean

namespace Conformance.PluginResources
open ToolPolicy Lean

private def consentJson : Json := toJson <| [0, 1, 64, 1536, 4096, 4294967295].flatMap fun requested =>
  [0, 1, 64, 1536, 4096].flatMap fun previous =>
  [false, true].map fun consent => Json.mkObj [
    ("requested", toJson requested), ("previous", toJson previous), ("consent", toJson consent),
    ("expected", toJson (pluginResourceConsented requested previous consent))]

private def budgetsJson : Json := toJson <| [(64, 4096), (5, 900), (60, 900), (1, 4)].flatMap fun (baseline, ceiling) =>
  [0, 1, baseline, ceiling, ceiling + 1, 4294967295].map fun requested => Json.mkObj [
    ("baseline", toJson baseline), ("requested", toJson requested), ("ceiling", toJson ceiling),
    ("expected", toJson (effectivePluginResource baseline requested ceiling))]

private def modelSlotsJson : Json := toJson <| [false, true].flatMap fun declared =>
  [false, true].flatMap fun optional =>
  [false, true].map fun agentFree => Json.mkObj [
    ("declared", toJson declared), ("optional", toJson optional), ("agent_free", toJson agentFree),
    ("expected", toJson (pluginModelSlotAllowed declared optional agentFree))]

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

private def renderGrant : Grant → Json
  | .sealed => Json.null
  | .anyHost => Json.str "any"
  | .hosts es => toJson (es.map renderEntry)

private def networkJson : Json := toJson <| grants.flatMap fun g =>
  targets.map fun (t, addrs) => Json.mkObj [
    ("grant", renderGrant g), ("url", toJson (renderTarget t)),
    ("addresses", toJson (addrs.map renderAddr)),
    ("public", toJson (addrs.map ipPublic)),
    ("expected", toJson (allowed g t addrs))]

end Network

private def callAccessJson : Json := toJson <| [PluginAccess.read, .readWrite].flatMap fun declared =>
  [false, true].flatMap fun writeFields =>
  [false, true].flatMap fun setsWriteField =>
  [none, some PluginAccess.read, some .readWrite].map fun granted => Json.mkObj [
    ("declared", toJson declared.spelling), ("write_fields", toJson writeFields),
    ("sets_write_field", toJson setsWriteField),
    ("granted", match granted with | none => Json.null | some access => toJson access.spelling),
    ("valid", toJson (pluginBindingValid declared writeFields)),
    ("call", toJson (pluginCallAccess declared writeFields setsWriteField).spelling),
    ("admitted", toJson (pluginCallAdmitted declared granted writeFields setsWriteField))]

private def invocationJson : Json :=
  let original : PluginInvocation.Record := ⟨"artifact", "sealed", "original", "original"⟩
  toJson <| ["same", "removed", "replaced", "grant", "declaration", "binding"].flatMap fun change =>
    [false, true].flatMap fun restart =>
    [false, true].map fun bound =>
      let current : Option PluginInvocation.Record := match change with
        | "removed" => none
        | "replaced" => some { original with digest := "replacement" }
        | "grant" => some { original with grant := "changed" }
        | "declaration" => some { original with declaration := "changed" }
        | "binding" => some { original with binding := "changed" }
        | _ => some original
      Json.mkObj [
        ("current", match current with
          | none => Json.null
          | some record => Json.mkObj [("digest", toJson record.digest),
              ("grant", toJson record.grant), ("declaration", toJson record.declaration),
              ("binding", toJson record.binding)]),
        ("restart", toJson restart), ("bound", toJson bound),
        ("admitted", toJson (PluginInvocation.admitted current original.digest
          (if bound then some original else none))),
        ("reusable", toJson (!restart && match current with
          | none => false
          | some current => PluginInvocation.reusable original current))]

private def toolCallsJson : Json := Json.mkObj [
  ("supported", toJson <| [("ordinary", PluginCalls.DispatchRoute.ordinary),
    ("hook_owned", .hookOwned)].map fun (route, value) =>
      Json.mkObj [("route", toJson route), ("expected", toJson (PluginCalls.supported value))]),
  ("availability", toJson <| [false, true].flatMap fun grant =>
    [("live", true), ("absent", false), ("child", PluginCalls.childDispatcher)].map fun (scope, live) =>
      Json.mkObj [("grant", toJson grant), ("dispatcher", toJson scope),
        ("expected", toJson (PluginCalls.available grant live))]),
  ("reservation", toJson <| [0, 1, 63, 64, 65, 1000].map fun used => Json.mkObj [
    ("used", toJson used), ("expected", toJson (PluginCalls.reserve used))]),
  ("result_bytes", toJson <| [0, 1048575, 1048576, 1048577].map fun bytes => Json.mkObj [
    ("bytes", toJson bytes), ("expected", toJson (PluginCalls.resultFits bytes))]),
  ("settlement", toJson <| [("completed", PluginCalls.ChildOutcome.completed), ("failed", .failed),
    ("unavailable", .unavailable), ("timed_out", .timedOut), ("cancelled", .cancelled),
    ("fatal", .fatal)].map fun (name, outcome) => Json.mkObj [("outcome", toJson name),
      ("expected", toJson (match PluginCalls.settle outcome with
        | .result => "result" | .guestError => "guest_error" | .parentFatal => "parent_fatal"))]),
  ("consent", toJson <| [false, true].flatMap fun interactive => [false, true].map fun composed =>
    Json.mkObj [("interactive", toJson interactive), ("composed", toJson composed),
      ("expected", toJson (match PluginCalls.consentRoute interactive composed with
        | .ask => "ask" | .deny => "deny"))])]

private def childAdmissionJson : Json := toJson <| [false, true].flatMap fun selected =>
  [("ordinary", PluginCalls.DispatchRoute.ordinary), ("hook_owned", .hookOwned)].flatMap
    fun (name, route) => [false, true].map fun admitted => Json.mkObj [
      ("selected", toJson selected), ("route", toJson name), ("admitted", toJson admitted),
      ("expected", toJson (PluginCalls.childAdmissible selected route admitted))]

private def publicationJson : Json := toJson <| [false, true].flatMap fun previous =>
  [("local_failure", [PluginPublication.Event.localFailure]),
   ("notification_success", [.localPublished, .notificationSucceeded]),
   ("notification_failure", [.localPublished, .notificationFailed]),
   ("notification_cancelled", [.localPublished, .cancelled])].map fun (outcome, events) =>
    Json.mkObj [("previous", toJson previous), ("outcome", toJson outcome),
      ("expected", if PluginPublication.installedNew events then toJson "new"
        else if previous then toJson "previous" else Json.null)]

/-- `PackPlugin.host_imports`: a plugin that declares `run_script` gets the
`gents` imports linked on every call; one that does not gets none, so a guest
importing them fails to instantiate. The import answers `result` when the call
carries a script and `no_script` when it does not. (declared, callHasScript) -/
def hostImportCases : List (Bool × Bool) :=
  [(false, false), (false, true), (true, false), (true, true)]

/-- (linked, answer). -/
def hostImportVerdict (c : Bool × Bool) : Bool × Option String :=
  (c.1, if c.1 then some (if c.2 then "result" else "no_script") else none)

private def hostImportJson : Json := toJson <| hostImportCases.map fun c =>
  let (linked, answer) := hostImportVerdict c
  Json.mkObj [("declared", toJson c.1), ("call_has_script", toJson c.2),
    ("linked", toJson linked), ("answer", toJson answer)]

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

private def scratchJson : Json := toJson <| scratchCases.map fun c => Json.mkObj [
  ("cwd", toJson scratchCwd), ("cwd_allowed", toJson c.cwdAllowed),
  ("tool_call_id", toJson c.toolCallId), ("handle", toJson c.handle), ("name", toJson c.name),
  ("size", toJson c.size), ("count", toJson c.count), ("expected", toJson (scratchVerdict c))]

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
   ("node_did", .nodeDid)]

private def noteFields : List String := ["note_id", "body", "job_row_doc_id", "chain_root_doc_id"]
private def nameFields : List String := ["name", "script", "node_did"]

private def boundedWrite : ApplicationWrite.Observation :=
  { granted := true, application := true, blocked := false, credential := false, bounded := true,
    targets := 1, limit := 1, preview := false, digestMatches := true }

private def noteWrite : DataWriteCase :=
  { op := .create, inOwnSdl := true, runtimeOrProtected := false, actorIsInvoker := true,
    opGranted := true, stampedSuppliedByGuest := false, invocation := .jobStep,
    collectionFields := noteFields, write := boundedWrite }

/-- A note collection carries the job stamps; a name collection carries only
`node_did`, so job stamps do not apply to it. -/
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

private def hostNodeDid : String := "did:key:host"

def dataFilterCases : List Filter :=
  [ [("owner", .callerNodeDid), ("name", .literal "draft")]
  , [("owner", .literal "did:key:guest")], [] ]

def dataFilterVerdict (f : Filter) : Filter := substituteCallerNodeDid f hostNodeDid

def dataNoteCases : List (Option Fingerprint × Fingerprint × OnConflict) :=
  [none, some "fp-a"].flatMap fun existing => ["fp-a", "fp-b"].flatMap fun new =>
    [OnConflict.ignore, .fail].map (existing, new, ·)

def dataNoteVerdict (c : Option Fingerprint × Fingerprint × OnConflict) : InsertOutcome :=
  insertNote c.1 c.2.1 c.2.2

private def opSpelling : ApplicationWrite.Operation → String
  | .create => "create" | .update => "update" | .delete => "delete"

private def invocationSpelling : Invocation → String
  | .toolCall => "tool_call" | .jobStep => "job_step" | .callback => "callback" | .ux => "ux"

private def stampSpelling : Stamp → String
  | .nodeDid => "node_did" | .sessionId => "session_id" | .requestDocId => "request_doc_id"
  | .toolCallDocId => "tool_call_doc_id" | .jobRowDocId => "job_row_doc_id"
  | .chainRootDocId => "chain_root_doc_id"

private def stampsJson (stamps : List (String × Stamp)) : Json :=
  Json.mkObj (stamps.map fun (field, s) => (field, toJson (stampSpelling s)))

private def refusalJson : Option Refusal → Json
  | none => Json.null
  | some (.stampUnavailable s inv) => Json.mkObj [("kind", toJson "stamp_unavailable"),
      ("stamp", toJson (stampSpelling s)), ("invocation", toJson (invocationSpelling inv))]
  | some .other => Json.mkObj [("kind", toJson "other")]

private def writeJson (w : ApplicationWrite.Observation) : Json := Json.mkObj [
  ("granted", toJson w.granted), ("application", toJson w.application),
  ("blocked", toJson w.blocked), ("credential", toJson w.credential),
  ("bounded", toJson w.bounded), ("targets", toJson w.targets), ("limit", toJson w.limit),
  ("preview", toJson w.preview), ("digest_matches", toJson w.digestMatches)]

/-- `$caller.node_did` is the placeholder's wire spelling. -/
private def filterJson (f : Filter) : Json := Json.mkObj <| f.map fun (field, value) =>
  (field, toJson (match value with | .literal v => v | .callerNodeDid => "$caller.node_did"))

private def insertOutcomeSpelling : InsertOutcome → String
  | .inserted => "inserted" | .unchanged => "unchanged" | .abortStep => "abort_step"

def dataCasesJson : String := (Json.mkObj [
  ("writes", toJson <| dataWriteCases.map fun c =>
    let (applicable, ok, refused) := dataWriteVerdict c
    Json.mkObj [("op", toJson (opSpelling c.op)), ("in_own_sdl", toJson c.inOwnSdl),
      ("runtime_or_protected", toJson c.runtimeOrProtected),
      ("actor_is_invoker", toJson c.actorIsInvoker), ("op_granted", toJson c.opGranted),
      ("stamped_supplied_by_guest", toJson c.stampedSuppliedByGuest),
      ("invocation", toJson (invocationSpelling c.invocation)),
      ("declared_stamps", stampsJson declaredStamps),
      ("collection_fields", toJson c.collectionFields), ("write", writeJson c.write),
      ("applicable", stampsJson applicable), ("admitted", toJson ok),
      ("refusal", refusalJson refused)]),
  ("queries", toJson <| dataQueryCases.map fun c => Json.mkObj [
    ("limit", toJson c.1), ("in_own_sdl", toJson c.2.1),
    ("runtime_or_protected", toJson c.2.2.1), ("query_granted", toJson c.2.2.2),
    ("expected", toJson (dataQueryVerdict c))]),
  ("filters", toJson <| dataFilterCases.map fun f => Json.mkObj [
    ("host_node_did", toJson hostNodeDid), ("filter", filterJson f),
    ("expected", filterJson (dataFilterVerdict f))]),
  ("notes", toJson <| dataNoteCases.map fun c => Json.mkObj [
    ("existing", toJson c.1), ("new", toJson c.2.1),
    ("on_conflict", toJson (match c.2.2 with | .ignore => "ignore" | .fail => "fail")),
    ("expected", toJson (insertOutcomeSpelling (dataNoteVerdict c)))])]).compress

private def noteStamps : List (String × Stamp) :=
  [("job_row_doc_id", .jobRowDocId), ("chain_root_doc_id", .chainRootDocId)]

theorem data_regressions :
    dataWriteCases.map dataWriteVerdict =
      [ (noteStamps, true, none), (noteStamps, false, some .other)
      , (noteStamps, false, some .other), (noteStamps, false, some .other)
      , (noteStamps, false, some .other), (noteStamps, false, some .other)
      , (noteStamps, false, some (.stampUnavailable .jobRowDocId .callback))
      , (noteStamps, false, some (.stampUnavailable .jobRowDocId .toolCall))
      , ([("node_did", .nodeDid)], true, none), ([("node_did", .nodeDid)], true, none)
      , (noteStamps, false, some (.stampUnavailable .jobRowDocId .ux))
      , ([("node_did", .nodeDid)], true, none)
      , (noteStamps, false, some .other) ] ∧
    dataQueryCases.map dataQueryVerdict = [false, true, true, false, false, false, false] ∧
    dataFilterCases.map dataFilterVerdict =
      [ [("owner", .literal "did:key:host"), ("name", .literal "draft")]
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

def routeCasesJson : String := (toJson <| routeCases.map fun c => Json.mkObj [
  ("static_hook_owned", toJson c.1), ("job_bound", toJson c.2.1),
  ("selected", toJson c.2.2.1), ("admitted", toJson c.2.2.2),
  ("expected", toJson (routeVerdict c))]).compress

theorem route_regressions :
    routeCases.map routeVerdict =
      [ false, false, false, true, false, false, false, false
      , false, false, false, false, false, false, false, false ] := by
  native_decide

def casesJson : String := (Json.mkObj [("consent", consentJson), ("budgets", budgetsJson),
  ("publication", publicationJson), ("tool_calls", toolCallsJson), ("child_admission", childAdmissionJson),
  ("tool_effects", PluginEffects.casesJson),
  ("tool_effect_terminals", PluginEffects.terminalCasesJson),
  ("tool_effect_accounting", PluginEffects.accountingCasesJson),
  ("model_slots", modelSlotsJson), ("network", networkJson),
  ("call_access", callAccessJson), ("invocation", invocationJson), ("receipt", Conformance.PluginReceipt.casesJson),
  ("scratch", scratchJson),
  ("host_import", hostImportJson)]).compress
end Conformance.PluginResources
