import Proofs.CanonicalOutput.Delegation

namespace CanonicalOutput.MessageCases

def agrees {ε α : Type} [DecidableEq ε] [DecidableEq α] :
    Except ε α → Except ε α → Bool
  | .ok left, .ok right => decide (left = right)
  | .error left, .error right => decide (left = right)
  | _, _ => false

def segment (secret : UInt8 := 65) (args : List UInt8 := [123, 125]) : Segment :=
  { id := 100, coordinate := ⟨10, .provider 0 0 0⟩, writer := .request 7
    flush := some ⟨0, [⟨0, 1, some { block := 0, part := 0, kind := .text }⟩,
      ⟨1, args.length, some { block := 1, part := 0, kind := .arguments, tool := some ⟨"call", none, "child"⟩ }⟩], secret :: args⟩
    close := some (.closed .complete 1 [1, args.length]), createdAt := 5 }

def text : PayloadSpec := ⟨⟨100, 0⟩, .full⟩
def args : PayloadSpec := ⟨⟨100, 1⟩, .full⟩
def call : MessageBlock PayloadSpec := .toolCall 300 "call" none "child" args (some "sig") none

def message (blocks : List (MessageBlock PayloadSpec) := [.text text, call]) : MessageEnvelope :=
  { header :=
      { id := 200, session := 1, request := some 10, origin := none
        refs := blocks.flatMap blockRefs, outcome := .complete, role := .assistant
        publication := .requestExecution 7 }
    key := "turn", sequence := 0, nativeId := some "provider-message", blocks := blocks
    createdAt := 6 }

def intent : ToolIntent := ⟨300, "call", "child", ⟨100, 1⟩⟩

theorem native_structure_and_metadata_survive :
    agrees (reconstructMessage [segment] [] message)
      (.ok ⟨.assistant, some "provider-message", [.text [65],
        .toolCall 300 "call" none "child" [123, 125] (some "sig") none]⟩) = true := by
  native_decide

theorem reordered_provider_blocks_rejected :
    agrees (reconstructMessage [segment] [] (message [call, .text text]))
      (.error .nativePosition) = true := by native_decide

theorem provider_position_cannot_move_between_blocks :
    let moved := message [.text text]
    let moved := { moved with blocks := [.text text], header :=
      { moved.header with refs := [text.reference] } }
    agrees (reconstructMessage [segment] [] moved) (.ok
      ⟨.assistant, some "provider-message", [.text [65]]⟩) = true := by native_decide

def reasoningSegment : Segment :=
  { id := 120, coordinate := ⟨10, .provider 0 1 0⟩, writer := .request 7
    flush := some ⟨0,
      [⟨0, 1, some { block := 0, part := 0, kind := .reasoning }⟩,
       ⟨1, 1, some { block := 1, part := 0, kind := .reasoning }⟩], [97, 98]⟩
    close := some (.closed .complete 1 [1, 1]), createdAt := 5 }

def reasoningA : PayloadSpec := ⟨⟨120, 0⟩, .full⟩
def reasoningB : PayloadSpec := ⟨⟨120, 1⟩, .full⟩

def reasoningMessage (blocks : List (MessageBlock PayloadSpec)) : MessageEnvelope :=
  { header :=
      { id := 220, session := 1, request := some 10, origin := none
      , refs := blocks.flatMap blockRefs, outcome := .complete, role := .assistant
      , publication := .requestExecution 7 }
  , key := "reasoning", sequence := 1, nativeId := none, blocks, createdAt := 6 }

theorem streams_cannot_be_regrouped_into_one_reasoning_block :
    let regrouped := reasoningMessage
      [.reasoning none [.text reasoningA none, .text reasoningB none]]
    agrees (reconstructMessage [reasoningSegment] [] regrouped)
      (.error .nativePosition) = true := by native_decide

theorem original_reasoning_block_boundaries_are_accepted :
    let exact := reasoningMessage
      [.reasoning none [.text reasoningA none], .reasoning none [.text reasoningB none]]
    agrees (reconstructMessage [reasoningSegment] [] exact)
      (.ok ⟨.assistant, none,
        [.reasoning none [.text [97] none], .reasoning none [.text [98] none]]⟩) = true := by
  native_decide

theorem incomplete_argument_json_not_repaired :
    agrees (reconstructMessage [segment 65 [123]] [] message)
      (.error .invalidJson) = true := by native_decide

theorem recovery_can_omit_incomplete_json :
    let retained := { segment 65 [123] with close := some (.closed .«partial» 1 [1, 1]) }
    let recovered := message [.text text]
    let recoveredHeader := { recovered.header with
      outcome := .«partial», publication := .requestRecovery 8 }
    let recovered := { recovered with header := recoveredHeader, nativeId := none }
    agrees (reconstructMessage [retained] [] recovered)
      (.ok ⟨.assistant, none, [.text [65]]⟩) = true := by native_decide

theorem delegated_input_contains_only_arguments :
    agrees (prepareDelegatedInput [segment] [] message intent)
      (.ok ⟨⟨100, 1⟩, "{}"⟩) = true := by native_decide

theorem changing_other_stream_does_not_change_delegated_bytes :
    agrees (prepareDelegatedInput [segment 66] [] message intent)
      (prepareDelegatedInput [segment 65] [] message intent) = true := by native_decide

theorem partial_publication_cannot_delegate :
    let partialMessage := { message with header := { message.header with outcome := .«partial» } }
    agrees (prepareDelegatedInput [segment] [] partialMessage intent)
      (.error .invalidPublication) = true := by native_decide

theorem unknown_call_cannot_delegate :
    agrees (prepareDelegatedInput [segment] [] message { intent with call := 999 })
      (.error .unknownCall) = true := by native_decide

theorem provider_id_mismatch_rejected :
    let wrong := .toolCall 300 "other" none "child" args (some "sig") none
    agrees (reconstructMessage [segment] [] (message [.text text, wrong]))
      (.error .metadataMismatch) = true := by native_decide

theorem provider_name_mismatch_rejected :
    let wrong := .toolCall 300 "call" none "other" args (some "sig") none
    agrees (reconstructMessage [segment] [] (message [.text text, wrong]))
      (.error .metadataMismatch) = true := by native_decide

theorem provider_call_id_mismatch_rejected :
    let wrong := .toolCall 300 "call" (some "other") "child" args (some "sig") none
    agrees (reconstructMessage [segment] [] (message [.text text, wrong]))
      (.error .metadataMismatch) = true := by native_decide

def mediaSegment : Segment :=
  { id := 110, coordinate := ⟨10, .provider 0 0 0⟩, writer := .request 7
    flush := some ⟨0, [⟨0, 1, some
      { block := 0, part := 0, kind := .media, mediaKind := some .audio }⟩], [97]⟩
    close := some (.closed .complete 1 [1]), createdAt := 5 }

def mediaSpec : PayloadSpec := ⟨⟨110, 0⟩, .full⟩

theorem media_kind_mismatch_rejected :
    let image : Media PayloadSpec := { kind := .image, data := .base64 mediaSpec }
    agrees (reconstructMessage [mediaSegment] [] (message [.media image]))
      (.error .metadataMismatch) = true := by native_decide

theorem delegation_rejects_wrong_request_scope :
    let wrong := { message with header := { message.header with request := some 11 } }
    agrees (prepareDelegatedInput [segment] [] wrong intent)
      (.error (.message .wrongSource)) = true := by native_decide

theorem delegation_rejects_wrong_generation :
    let wrong := { message with header :=
      { message.header with publication := .requestExecution 8 } }
    agrees (prepareDelegatedInput [segment] [] wrong intent)
      (.error (.message .wrongSource)) = true := by native_decide

theorem local_call_cannot_receive_delegated_projection :
    receiveDelegatedInput 1 1 7 ⟨300, 1, 1, 7, ⟨⟨100, 1⟩, "{}"⟩⟩ = none := by
  rfl

theorem presentation_preserves_recorded_normalization :
    agrees (present [97, 13, 10, 98]
      (.composed [.range 0 1, .literal [10], .range 3 4]))
      (.ok [97, 10, 98]) = true := by native_decide

theorem presentation_can_select_no_bytes :
    agrees (present [97] (.composed [])) (.ok []) = true := by native_decide

theorem invalid_presentation_range_rejected :
    agrees (present [97] (.composed [.range 0 2])) (.error .invalidRange) = true := by
  native_decide

theorem range_cannot_split_utf8_codepoint :
    agrees (present [195, 169] (.composed [.range 0 1])) (.error .invalidUtf8) = true := by
  native_decide

theorem argument_presentation_cannot_rewrite_canonical_json :
    agrees (resolveMessagePayload [segment] [] [.arguments] true
      { args with presentation := .composed [.literal [123, 125]] })
      (.error .invalidPresentation) = true := by native_decide

theorem system_message_has_one_text_and_no_native_id :
    let bad := { message with header := { message.header with role := .system } }
    agrees (reconstructMessage [segment] [] bad) (.error .invalidRole) = true := by native_decide

theorem payload_free_assistant_is_structurally_valid :
    let empty := message []
    agrees (reconstructMessage [] [] empty)
      (.ok ⟨.assistant, some "provider-message", []⟩) = true := by native_decide

end CanonicalOutput.MessageCases
