import Proofs.Session.CanonicalOutput.Message

/-! A transcript read reconstructs each message from every segment delivered
before that read. An earlier read never fixes what a later read sees: a payload
segment replicated after its source's closure, and after another message was
read, is observed by the next read. -/

namespace CanonicalOutput.LateDelivery

open CanonicalOutput

inductive Step where
  | deliver (segment : Segment)
  | read (message : MessageEnvelope)

def run : List Segment → List Step → List (Except MessageError ReconstructedMessage)
  | _, [] => []
  | records, .deliver segment :: rest => run (deliver records segment) rest
  | records, .read message :: rest => reconstructMessage records [] message :: run records rest

theorem read_observes_every_prior_delivery (records : List Segment) (segment : Segment)
    (message : MessageEnvelope) (rest : List Step) :
    run records (.deliver segment :: .read message :: rest) =
      reconstructMessage (deliver records segment) [] message ::
        run (deliver records segment) rest := rfl

def textDeclaration : Declaration := ⟨0, 0, .text, none, none⟩

/-- One flush of an authored text source: the first flush declares stream 0,
later flushes continue it, and the last one also closes the source. -/
def authoredSegment (id request key generation : Nat) (parts : List String)
    (ordinal : Nat) : Segment :=
  let part := parts.getD ordinal ""
  { id
    coordinate := ⟨request, .authored key⟩
    writer := .request generation
    flush := some ⟨ordinal,
      [⟨0, part.utf8ByteSize, if ordinal = 0 then some textDeclaration else none⟩],
      part.toUTF8.toList⟩
    close := if ordinal + 1 = parts.length then
        some (.closed .complete parts.length [(parts.map String.utf8ByteSize).sum])
      else none
    createdAt := 0 }

def authoredUserMessage (id request key generation closeId sequence : Nat) :
    MessageEnvelope :=
  { header :=
      { id, session := 0, request := some request, origin := none
        refs := [⟨closeId, 0⟩], outcome := .complete, role := .user
        publication := .requestExecution generation }
    key := toString key
    sequence
    nativeId := none
    blocks := [.text ⟨⟨closeId, 0⟩, .full⟩]
    createdAt := 0 }

def scanGap : List Step :=
  [ .deliver (authoredSegment 1 0 1 0 ["first"] 0)
  , .deliver (authoredSegment 3 0 2 0 ["hel", "lo"] 1)
  , .read (authoredUserMessage 10 0 1 0 1 1)
  , .deliver (authoredSegment 2 0 2 0 ["hel", "lo"] 0)
  , .read (authoredUserMessage 11 0 2 0 3 2) ]

theorem reused_scan_observes_payload_segments_replicated_after_their_close :
    (run [] scanGap).map (·.toOption.map (·.blocks)) =
      [some [.text "first".toUTF8.toList], some [.text "hello".toUTF8.toList]] := by
  native_decide

end CanonicalOutput.LateDelivery
