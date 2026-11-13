import Proofs.Conformance.Contracts.Json.ClientRuntime

/-! The canonical output wire every canonical-output family shares: segments,
message envelopes, reconstructed native messages, projection observations and
views as the JSON values native translators decode. -/

namespace Conformance.CanonicalOutputWire

open Lean Conformance.Contracts

def parsed (raw : String) : Json :=
  match Json.parse raw with
  | .ok value => value
  | .error error => panic! error

def segment (value : CanonicalOutput.Segment) : Json := parsed (canonicalSegmentJson value)

def segments (values : List CanonicalOutput.Segment) : Json :=
  Json.arr (values.map segment).toArray

def message (value : CanonicalOutput.MessageEnvelope) : Json := parsed (canonicalMessageJson value)

def messages (values : List CanonicalOutput.MessageEnvelope) : Json :=
  Json.arr (values.map message).toArray

def reconstructed (value : CanonicalOutput.ReconstructedMessage) : Json :=
  parsed (reconstructedMessageJson value)

def reconstructedList (values : List CanonicalOutput.ReconstructedMessage) : Json :=
  Json.arr (values.map reconstructed).toArray

def payloadSpec (value : CanonicalOutput.PayloadSpec) : Json := parsed (payloadSpecJson value)

def streams (value : CanonicalOutput.Streams) : Json := parsed (streamsJson value)

def observation (value : StreamingResponse.Observation) : Json :=
  parsed (outputObservationJson value)

def view (value : StreamingResponse.View) : Json := parsed (outputViewJson value)

end Conformance.CanonicalOutputWire
