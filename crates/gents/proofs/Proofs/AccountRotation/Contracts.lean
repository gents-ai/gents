import Proofs.AccountRotation.Cases
import Proofs.Conformance.ContractTypes

namespace AccountRotation.Contracts

open Conformance.Contracts

def familyName : Family → String
  | .opus => "opus"
  | .sonnet => "sonnet"
  | .other => "other"

/-- `anthropic-ratelimit-unified-representative-claim` spellings. -/
def claimName : Claim → String
  | .fiveHour => "five_hour"
  | .sevenDay => "seven_day"
  | .sevenDayOpus => "seven_day_opus"
  | .sevenDaySonnet => "seven_day_sonnet"
  | .unknown => "unknown"

def severityName : Severity → String
  | .warning => "allowed_warning"
  | .rejected => "rejected"

def overageName : Overage → String
  | .absent => "absent"
  | .allowed => "allowed"
  | .warning => "allowed_warning"
  | .rejected => "rejected"

def boolJson (value : Bool) : String := if value then "true" else "false"

def optionalNatJson : Option Nat → String
  | none => "null"
  | some value => toString value

def limitJson (limit : Limit) : String :=
  "{\"claim\":" ++ jsonString (claimName limit.claim)
    ++ ",\"severity\":" ++ jsonString (severityName limit.severity)
    ++ ",\"reset_at\":" ++ toString limit.resetAt ++ "}"

def accountJson (account : Account) : String :=
  "{\"enabled\":" ++ boolJson account.enabled
    ++ ",\"limits\":" ++ jsonArray (account.limits.map limitJson)
    ++ ",\"overage\":" ++ jsonString (overageName account.overage)
    ++ ",\"blocked_until\":" ++ toString account.blockedUntil ++ "}"

def SelectCase.toJson (c : SelectCase) : String :=
  "{\"name\":" ++ jsonString c.name
    ++ ",\"allow_overage\":" ++ boolJson c.policy.allowOverage
    ++ ",\"now\":" ++ toString caseNow
    ++ ",\"family\":" ++ jsonString (familyName c.family)
    ++ ",\"pool\":" ++ jsonArray (c.pool.map accountJson)
    ++ ",\"current\":" ++ optionalNatJson c.current
    ++ ",\"cursor\":" ++ toString c.cursor
    ++ ",\"expected\":" ++ optionalNatJson c.expected ++ "}"

def AttemptCase.toJson (c : AttemptCase) : String :=
  "{\"name\":" ++ jsonString c.name
    ++ ",\"allow_overage\":" ++ boolJson c.policy.allowOverage
    ++ ",\"now\":" ++ toString caseNow
    ++ ",\"family\":" ++ jsonString (familyName c.family)
    ++ ",\"pool\":" ++ jsonArray (c.pool.map accountJson)
    ++ ",\"current\":" ++ optionalNatJson c.current
    ++ ",\"cursor\":" ++ toString c.cursor
    ++ ",\"rejects\":" ++ jsonArray (c.rejects.map fun (account, resetAt) =>
        "{\"account\":" ++ toString account ++ ",\"reset_at\":" ++ toString resetAt ++ "}")
    ++ ",\"expected\":" ++ jsonArray (c.expected.map toString) ++ "}"

def ObserveCase.toJson (c : ObserveCase) : String :=
  "{\"name\":" ++ jsonString c.name
    ++ ",\"family\":" ++ jsonString (familyName c.family)
    ++ ",\"account\":" ++ accountJson c.account
    ++ ",\"reported\":" ++ jsonArray (c.reported.map limitJson)
    ++ ",\"overage\":" ++ jsonString (overageName c.overage)
    ++ ",\"expected\":" ++ accountJson c.expected ++ "}"

def selectCasesJson : String := jsonArray (selectCases.map SelectCase.toJson)

def attemptCasesJson : String := jsonArray (attemptCases.map AttemptCase.toJson)

def observeCasesJson : String := jsonArray (observeCases.map ObserveCase.toJson)

end AccountRotation.Contracts
