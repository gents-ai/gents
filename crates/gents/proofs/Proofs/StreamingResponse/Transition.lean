import Proofs.StreamingResponse.State

namespace StreamingResponse

open CanonicalOutput

/-- Observation changes only by immutable fact delivery or by an owner fact
changing. Canonical execution, not this projection, authorizes those facts. -/
inductive Transition : Observation → Observation → Prop
  | deliverSegment (observation : Observation) (record : Segment) :
      Transition observation
        { observation with records := CanonicalOutput.deliver observation.records record }
  | deliverMessage (observation : Observation) (message : MessageEnvelope) :
      Transition observation
        { observation with
            messages := if message ∈ observation.messages then observation.messages
              else observation.messages ++ [message] }
  | observeOwner (observation : Observation) (owner : OwnerLiveness) :
      Transition observation { observation with owner := owner }
  | selectMessage (observation : Observation) (id : DocId) :
      Transition observation
        { observation with target := { observation.target with messageId := some id } }
  | observeTerminal (observation : Observation) (selection : TerminalSelection) :
      Transition observation
        { observation with requestTerminal := true, terminalSelection := some selection }

inductive Trace : Observation → Observation → Prop
  | refl (observation) : Trace observation observation
  | step {before middle after} : Transition before middle → Trace middle after →
      Trace before after

end StreamingResponse
