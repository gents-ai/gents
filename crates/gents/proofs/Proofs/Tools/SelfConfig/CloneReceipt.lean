/-! What a clone's receipt reports about what the clone did not copy. -/

namespace SelfConfig.CloneReceipt

structure Report where
  selfConfigNotCopied : Bool
  promptWarning : Bool
  deriving DecidableEq, Repr

/-- Decision (maintainer, 2026-11-17, gents#4067 B4): a clone never copies
the source's `tools.self_config`, because copying it would widen what a new
Agent may do by default. The receipt names the dropped group with the
command that grants it back, and warns when the cloned prompt names the tool
the clone lost. Fresh creation copies nothing, so it reports nothing. -/
def report (isClone sourceHasSelfConfig promptMentionsConfig : Bool) : Report :=
  let dropped := isClone && sourceHasSelfConfig
  { selfConfigNotCopied := dropped, promptWarning := dropped && promptMentionsConfig }

/-- A requested override is reported when it changed the source's value and
never reverted: the operator chose it. -/
def overrideReported (requested changed : Bool) : Bool := requested && changed

def words (prompt : String) : List String :=
  (prompt.split fun c => !(c.isAlphanum || c == '_')).filter (!·.isEmpty)

/-- The prompt names `config` (or `self_config`) as a whole word, in any
ASCII case; `configure` and `configuration` are other words. -/
def mentionsConfig (prompt : String) : Bool :=
  (words prompt).any fun word => word.toLower == "config" || word.toLower == "self_config"

theorem fresh_creation_reports_nothing (sourceHasSelfConfig promptMentionsConfig : Bool) :
    report false sourceHasSelfConfig promptMentionsConfig = ⟨false, false⟩ := by
  simp [report]

theorem a_warning_names_a_dropped_grant (isClone source mentions : Bool)
    (h : (report isClone source mentions).promptWarning = true) :
    (report isClone source mentions).selfConfigNotCopied = true := by
  cases isClone <;> cases source <;> cases mentions <;> simp_all [report]

theorem an_unrequested_change_is_not_reported (changed : Bool) :
    overrideReported false changed = false := rfl

end SelfConfig.CloneReceipt
