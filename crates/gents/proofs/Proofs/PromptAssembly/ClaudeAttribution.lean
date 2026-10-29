import Proofs.PromptAssembly.ClaudeMap

/-!
# Claude Code attribution block

The first system block of a Claude Code request attributes it to a client build
(Claude Code 2.1.311 `J6o`, `chunk-1twhepc1.js`). It travels as `system[0]` text,
never carries `cache_control`, and changes on every first-party request
(`cc_prev_req`, `cc_prompt_id`, the prompt and turn indices) while Claude Code
replays thinking across those requests. The provider therefore keeps this block
out of the prompt prefix it caches and binds thinking signatures to.

The `cc_version` fingerprint (SHA-256 over selected UTF-16 units of the first
user text) and the request-scoped values are computed natively; this module
fixes the field order and every inclusion gate. Claude Code's
`CLAUDE_CODE_ATTRIBUTION_HEADER` opt-out is a local user setting with no Gents
counterpart, so the block is always present.
-/

namespace PromptAssembly.ClaudeAttribution

open PromptAssembly.ClaudeMap

/-- Claude Code `Te()` provider kinds. -/
inductive Provider where
  | firstParty
  | vertex
  | bedrock
  | anthropicAws
  | anthropicGoogleCloud
  | foundry
  | mantle
  | gateway
  deriving DecidableEq, Repr

structure Fields where
  version : String
  fingerprint : String
  entrypoint : String
  provider : Provider
  /-- Claude Code `xi()`: the base URL is unset or `api.anthropic.com`. -/
  firstPartyBaseUrl : Bool
  /-- `cron` for scheduled, looping and auto-continued work (`Wlr`). -/
  workload : Option String := none
  subagent : Bool := false
  previousRequestId : Option String := none
  promptId : Option String := none
  turnOrigin : Option String := none
  promptIndex : Option Nat := none
  turnIndex : Option Nat := none
  /-- `1` behind an authenticated gateway, `env` behind an unvouched custom base
  URL; decided by the gateway owner from its credential and host verdict. -/
  gateway : Option String := none
  /-- Tagged project id and the settings layer it came from. -/
  project : Option (String × String) := none
  deriving DecidableEq, Repr

def firstPartyBase (fields : Fields) : Bool :=
  fields.provider == .firstParty && fields.firstPartyBaseUrl

/-- `^req_[A-Za-z0-9_-]{1,36}$` -/
def validRequestId (id : String) : Bool :=
  match id.toList with
  | 'r' :: 'e' :: 'q' :: '_' :: rest =>
      decide (1 ≤ rest.length) && decide (rest.length ≤ 36) &&
        rest.all (fun c => c.isAlphanum || c == '_' || c == '-')
  | _ => false

def hexDigit (c : Char) : Bool :=
  c.isDigit || ('a' ≤ c && c ≤ 'f') || ('A' ≤ c && c ≤ 'F')

/-- `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$` with the
case-insensitive flag. -/
def validUuid (id : String) : Bool :=
  let groups := id.splitOn "-"
  groups.map String.length == [8, 4, 4, 4, 12] && groups.all (·.toList.all hexDigit)

def lowerAscii (c : Char) : Bool := 'a' ≤ c && c ≤ 'z'

/-- `^[a-z][a-z_]{0,31}$` -/
def validTurnOrigin (origin : String) : Bool :=
  match origin.toList with
  | c :: rest => lowerAscii c && decide (rest.length ≤ 31) &&
      rest.all (fun c => lowerAscii c || c == '_')
  | [] => false

/-- Claude Code `g`: an integer in `[floor, 10^7]`. -/
def validIndex (floor value : Nat) : Bool :=
  decide (floor ≤ value) && decide (value ≤ 10000000)

def base58 (c : Char) : Bool :=
  ('1' ≤ c && c ≤ '9') || ('A' ≤ c && c ≤ 'H') || ('J' ≤ c && c ≤ 'N') ||
    ('P' ≤ c && c ≤ 'Z') || ('a' ≤ c && c ≤ 'k') || ('m' ≤ c && c ≤ 'z')

/-- `X6o`: `^claude_proj_(?:staging_|local_)?01[1-9A-HJ-NP-Za-km-z]{22}$` -/
def validProjectId (id : String) : Bool :=
  let stem := ["claude_proj_staging_", "claude_proj_local_", "claude_proj_"].find?
    (·.isPrefixOf id)
  match stem.map (fun stem => (id.drop stem.length).toList) with
  | some ('0' :: '1' :: rest) => rest.length == 22 && rest.all base58
  | _ => false

/-- Settings layers a project attribution may come from (`H`). -/
def projectSources : List String :=
  ["harness", "user", "project", "local", "flag", "managed", "admin_default"]

/-- The optional fields in emission order; `none` is an omitted field. -/
def slots (fields : Fields) : List (Option String) :=
  let base := firstPartyBase fields
  [ if base || fields.provider == .vertex then some " cch=00000;" else none
  , fields.workload.bind fun workload =>
      if workload.isEmpty then none else some (" cc_workload=" ++ workload ++ ";")
  , if fields.subagent then some " cc_is_subagent=true;" else none
  , fields.previousRequestId.bind fun id =>
      if base && validRequestId id then some (" cc_prev_req=" ++ id ++ ";") else none
  , fields.promptId.bind fun id =>
      if base && validUuid id then some (" cc_prompt_id=" ++ id ++ ";") else none
  , fields.turnOrigin.bind fun origin =>
      if base && validTurnOrigin origin then some (" cc_turn_origin=" ++ origin ++ ";")
      else none
  , match fields.promptIndex, fields.turnIndex with
    | some prompt, some turn =>
        if base && validIndex 0 prompt && validIndex 1 turn then
          some (" cc_prompt_index=" ++ toString prompt ++ "; cc_turn_index=" ++
            toString turn ++ ";")
        else none
    | _, _ => none
  , fields.gateway.map fun mark => " cc_gateway=" ++ mark ++ ";"
  , fields.project.bind fun (id, source) =>
      if base && validProjectId id && projectSources.contains source then
        some (" cc_project=" ++ id ++ "; cc_project_src=" ++ source ++ ";")
      else none ]

def body (fields : Fields) : String :=
  " cc_version=" ++ fields.version ++ "." ++ fields.fingerprint ++ "; cc_entrypoint=" ++
    fields.entrypoint ++ ";" ++ String.join (slots fields).reduceOption

def header (fields : Fields) : String := attributionPrefix ++ body fields

theorem header_starts_with_prefix (fields : Fields) :
    header fields = attributionPrefix ++ body fields := rfl

theorem slots_length (fields : Fields) : (slots fields).length = 9 := by
  simp [slots]

theorem cch_iff (fields : Fields) :
    (slots fields).head? = some (some " cch=00000;") ↔
      firstPartyBase fields = true ∨ fields.provider = .vertex := by
  by_cases h : (firstPartyBase fields || fields.provider == .vertex) = true <;>
    simp_all [slots]

/-- Request-scoped fields and the project attribution are first-party only. -/
theorem request_fields_require_first_party (fields : Fields)
    (h : firstPartyBase fields = false) :
    (slots fields)[3]? = some none ∧ (slots fields)[4]? = some none ∧
      (slots fields)[5]? = some none ∧ (slots fields)[6]? = some none ∧
      (slots fields)[8]? = some none := by
  refine ⟨?_, ?_, ?_, ?_, ?_⟩ <;> simp only [slots, h, Bool.false_and, if_false]
  · cases fields.previousRequestId <;> simp
  · cases fields.promptId <;> simp
  · cases fields.turnOrigin <;> simp
  · cases fields.promptIndex <;> cases fields.turnIndex <;> simp
  · cases fields.project <;> simp

end PromptAssembly.ClaudeAttribution
