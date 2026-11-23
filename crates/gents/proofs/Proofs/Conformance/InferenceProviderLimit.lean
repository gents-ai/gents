import Proofs.Inference.InferenceCall.ProviderLimit
import Proofs.Conformance.Family

namespace Conformance.InferenceProviderLimit
open InferenceCall.ProviderLimit

attribute [conformance_vocabulary] Verdict.toDefraDB

deriving instance Lean.ToJson for Verdict
deriving instance Lean.ToJson for Statement

/-- One attempt's report: `reportedAt` is a usage exhaustion's reset or a
throttle's wake, and `statement` what a usage exhaustion states of its limit.
Times are seconds. -/
structure VerdictInput where
  now : Nat
  maxWait : Option Nat
  usageExhausted : Bool
  reportedAt : Option Nat
  statement : Statement
  deriving Lean.ToJson

structure VerdictOutput where
  verdict : Verdict
  resetsAt : Option Nat
  statement : Statement
  deriving Lean.ToJson

def VerdictInput.report (i : VerdictInput) : Report :=
  if i.usageExhausted then .usageExhausted i.reportedAt i.statement else .throttled i.reportedAt

/-- The statements a case carries: none, and a full one. -/
def statements : List Statement :=
  [{}, { windowMinutes := some 1440, tokensUsed := some 691791, tokensLimit := some 600000 }]

@[conformance_family]
def verdicts : Conformance.Family VerdictInput VerdictOutput where
  inputs :=
    ([none, some 300, some 900].flatMap fun maxWait =>
      [none, some 1000, some 1299, some 1300, some 1301, some 2000, some 100000].flatMap
        fun reportedAt =>
          [false, true].flatMap fun usageExhausted =>
            statements.map fun statement =>
              { now := 1000, maxWait, usageExhausted, reportedAt, statement })
  model := fun i =>
    let recorded := verdict i.now i.maxWait i.report
    { verdict := recorded.verdict, resetsAt := recorded.resetsAt,
      statement := recorded.statement }

end Conformance.InferenceProviderLimit
