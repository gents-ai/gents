import Lean

namespace Runtime.StartupDegraded
open Lean

inductive Reason where
  | agentDisabled | backendNotConfigured | backendDisabled
  | backendTemporarilyUnavailable | credentialsRequired
  | runtimeConfigurationInvalid | subscriptionNotEntitled | inferenceProfileInvalid
  | toolConfigurationInvalid | toolSurfaceUnavailable | executorStartFailed
  deriving ToJson

/-- Configuration availability may defer an Agent while the node starts.
An unusable tool surface is a failed admission, rather than deferred availability. -/
def allowsStartup : Reason → Bool
  | .agentDisabled | .backendNotConfigured | .backendDisabled
  | .backendTemporarilyUnavailable | .credentialsRequired => true
  | .runtimeConfigurationInvalid | .subscriptionNotEntitled | .inferenceProfileInvalid
  | .toolConfigurationInvalid | .toolSurfaceUnavailable | .executorStartFailed => false

end Runtime.StartupDegraded
