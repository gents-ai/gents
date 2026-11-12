/-! Whether a tool definition carries `eager_input_streaming` (`k5t`). Only Bedrock
and Vertex read the catalog flag of the model, and only on their own default
endpoint: a custom base URL never streams tool input eagerly, and every other
provider never does. -/

namespace EagerStreaming

inductive Provider where
  | firstParty
  | bedrock
  | vertex
  | foundry
  | anthropicAws
  | anthropicGoogleCloud
  | mantle
  | gateway
  deriving DecidableEq, Repr

def enabled (provider : Provider) (defaultEndpoint bedrock vertex : Bool) : Bool :=
  match provider, defaultEndpoint with
  | .bedrock, true => bedrock
  | .vertex, true => vertex
  | _, _ => false

theorem a_custom_endpoint_never_streams_eagerly (provider : Provider) (bedrock vertex : Bool) :
    enabled provider false bedrock vertex = false := by
  cases provider <;> rfl

theorem bedrock_reads_only_its_own_flag (bedrock vertex : Bool) :
    enabled .bedrock true bedrock vertex = bedrock := rfl

theorem vertex_reads_only_its_own_flag (bedrock vertex : Bool) :
    enabled .vertex true bedrock vertex = vertex := rfl

theorem other_providers_never_stream_eagerly (provider : Provider) (bedrock vertex : Bool)
    (h : provider ≠ .bedrock ∧ provider ≠ .vertex) :
    enabled provider true bedrock vertex = false := by
  cases provider <;> first | rfl | exact (h.1 rfl).elim | exact (h.2 rfl).elim

end EagerStreaming
