import Proofs.Tools.SelfConfig.CloneReceipt
import Proofs.Conformance.Family

namespace SelfConfig.CloneReceipt

deriving instance Lean.ToJson for Report

attribute [conformance_function] report overrideReported

end SelfConfig.CloneReceipt

namespace Conformance.CloneReceipt

open Lean

structure PromptInput where
  prompt : String
  deriving ToJson

@[conformance_family]
def promptMention : Family PromptInput Bool where
  inputs := [
      "Use config to inspect Tools.", "Use `config`.", "CONFIG is unavailable.",
      "Use self_config to inspect Tools.", "Use `SELF_CONFIG`.", "config",
      "Configure yourself.", "Check the configuration.", "self_configure", "configurator",
      "reconfig", "", "config_tool", "Ask the config-tool."
    ].map fun prompt => { prompt }
  model := fun i => SelfConfig.CloneReceipt.mentionsConfig i.prompt

end Conformance.CloneReceipt
