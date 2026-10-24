import Proofs.SessionHydration.FileInventoryOrder

namespace SessionHydration.FileInventory.Order.Examples

open CanonicalOutput

def admission : Request := ⟨"inventory", "peer", "reader", "node", "session", 1⟩

def tool : Tool := ⟨90, 700, ⟨"reader", "node", "session"⟩, true, .completed⟩

def call (id sequence : Nat) (selectedTool : Tool := tool) : MessageEnvelope :=
  let blocks := [MessageBlock.toolCall selectedTool.id "call" none "write"
    ⟨⟨1000, 0⟩, .full⟩ none none]
  { header := ⟨id, 1, some selectedTool.request, none, blocks.flatMap blockRefs,
      .complete, .assistant, .requestExecution 1⟩
  , key := "call-" ++ toString id, sequence, nativeId := none, blocks, createdAt := 0 }

def terminalRecords (selectedTool : Tool) : List Segment :=
  [ ⟨3000 + selectedTool.id, ⟨selectedTool.request, .tool selectedTool.id⟩,
      .tool selectedTool.id,
      some ⟨0, [⟨0, 0, some ⟨0, 0, .toolOutput, none, none⟩⟩], []⟩, none, 0⟩
  , ⟨4000 + selectedTool.id, ⟨selectedTool.request, .tool selectedTool.id⟩,
      .tool selectedTool.id, none, some (.closed .complete 1 [0]), 0⟩ ]

def delivered (id sequence : Nat) (selectedTool : Tool := tool) : MessageEnvelope :=
  let blocks := [MessageBlock.toolResult selectedTool.id "call" none
    [.text ⟨⟨4000 + selectedTool.id, 0⟩, .full⟩]]
  { header := ⟨id, 1, some selectedTool.request, none, blocks.flatMap blockRefs, .complete, .user, .toolDelivery selectedTool.id⟩
  , key := "delivery-" ++ toString id, sequence, nativeId := none
  , blocks, createdAt := 0 }

def snapshot (headers : List MessageEnvelope) : Snapshot :=
  { admission
  , documents := headers.map fun message => ⟨.agentMessage, message.header.id⟩
  , tools := [⟨tool, .unknown⟩] }

def input (messages : List MessageEnvelope) : ClosureInput :=
  { request := admission, bases := [], roots := [], requirements := [], messages
  , segments := terminalRecords tool ++ terminalRecords { tool with id := 2, request := 701 }
  , access := [], deniedHeaders := [], deniedSegments := [] }

theorem invocation_result_pair_has_canonical_positions :
    window (snapshot [call 10 3, delivered 11 5]) (input [call 10 3, delivered 11 5]) tool =
      some ⟨90, 3, 5⟩ := by native_decide

theorem missing_result_does_not_supply_a_window :
    window (snapshot [call 10 3]) (input [call 10 3]) tool = none := by native_decide

theorem unselected_later_result_does_not_extend_a_cut :
    window (snapshot [call 10 3]) (input [call 10 3, delivered 11 5]) tool = none := by native_decide

theorem wrong_physical_request_does_not_supply_a_window :
    let result := delivered 11 5
    let result := { result with header := { result.header with request := some 701 } }
    window (snapshot [call 10 3, result]) (input [call 10 3, result]) tool = none := by native_decide

theorem wrong_physical_tool_does_not_supply_a_window :
    window (snapshot [call 10 3, delivered 11 5]) (input [call 10 3, delivered 11 5])
      { tool with id := 91 } = none := by native_decide

theorem conflicting_session_position_does_not_supply_a_window :
    window (snapshot [call 10 3, delivered 11 3]) (input [call 10 3, delivered 11 3]) tool =
      none := by native_decide

theorem result_before_admission_does_not_supply_a_window :
    window (snapshot [call 10 5, delivered 11 3]) (input [call 10 5, delivered 11 3]) tool =
      none := by native_decide

theorem a_selected_fork_preserves_its_canonical_window :
    let first := call 10 3
    let last := delivered 11 5
    let forkedFirst := { first with header := forkHeader first.header 20 2 }
    let forkedLast := { last with header := forkHeader last.header 21 2 }
    let request := { admission with nativeSession := 2 }
    let selected := { snapshot [forkedFirst, forkedLast] with admission := request }
    let closure := { input [first, last, forkedFirst, forkedLast] with request }
    window selected closure tool = some ⟨90, 3, 5⟩ := by native_decide

theorem a_fork_cut_before_delivery_stays_unresolved :
    let first := call 10 3
    let last := delivered 11 5
    let forked := { first with header := forkHeader first.header 20 2 }
    let request := { admission with nativeSession := 2 }
    let selected := { snapshot [forked] with admission := request }
    let closure := { input [first, last, forked] with request }
    window selected closure tool = none := by native_decide

theorem physical_id_order_does_not_determine_edit_order :
    resolve [⟨2, 5, 7⟩, ⟨90, 1, 3⟩, ⟨1, 8, 10⟩] = some [90, 2, 1] := by native_decide

theorem overlapping_calls_do_not_supply_an_order :
    resolve [⟨90, 1, 7⟩, ⟨2, 5, 8⟩] = none := by native_decide

theorem calls_from_one_admission_do_not_supply_an_order :
    resolve [⟨90, 1, 3⟩, ⟨2, 1, 4⟩] = none := by native_decide

theorem duplicate_physical_tools_do_not_supply_an_order :
    resolve [⟨90, 1, 3⟩, ⟨90, 5, 7⟩] = none := by native_decide

theorem a_reversed_window_does_not_supply_an_order :
    resolve [⟨90, 3, 1⟩] = none := by native_decide

def captured (selectedTool : Tool) : ToolFact :=
  ⟨selectedTool, .observed
    ⟨900 + selectedTool.id, 800 + selectedTool.id, selectedTool.id, selectedTool.request,
      selectedTool.scope, "node", [⟨"workspace", "file.rs", .file "before", .file "after"⟩]⟩⟩

def secondTool : Tool := { tool with id := 2, request := 701 }

def sequentialHeaders : List MessageEnvelope :=
  [call 12 6 secondTool, delivered 13 8 secondTool, delivered 11 4, call 10 1]

def capturedSnapshot : Snapshot :=
  let initial := { snapshot sequentialHeaders with tools := [captured secondTool, captured tool] }
  { initial with ordering := initial.tools.map fun fact => window initial (input sequentialHeaders) fact.tool }

theorem physical_page_order_preserves_canonical_resource_ordinals :
    capturedSnapshot.entries.map (fun entry => (entry.tool, entry.order)) =
      [(2, .ordered 1 2), (90, .ordered 0 2)] := by native_decide

theorem changing_only_ordering_invalidates_the_receipt :
    (VerifiedReceipt.metadata capturedSnapshot).proves
      { capturedSnapshot with ordering := [] } = false := by native_decide

theorem interleaved_physical_inventory_uses_one_resource_and_canonical_session :
    comparison capturedSnapshot (input sequentialHeaders) "node" "workspace" "file.rs" =
      some [90, 2] := by native_decide

theorem another_resource_cannot_borrow_the_order :
    comparison capturedSnapshot (input sequentialHeaders) "node" "workspace" "other.rs" =
      none := by native_decide

theorem another_source_cannot_borrow_the_order :
    comparison capturedSnapshot (input sequentialHeaders) "other-node" "workspace" "file.rs" =
      none := by native_decide

theorem missing_intermediate_delivery_keeps_the_comparison_unresolved :
    let documents := capturedSnapshot.documents.filter (fun key => key.id != 11)
    let selected := { capturedSnapshot with documents }
    comparison selected (input sequentialHeaders) "node" "workspace" "file.rs" = none := by
  native_decide

def forkInput (includeDelivery : Bool) : Snapshot × ClosureInput :=
  let first := call 10 3
  let last := delivered 11 5
  let forkedFirst := { first with header := forkHeader first.header 20 2 }
  let forkedLast := { last with header := forkHeader last.header 21 2 }
  let request := { admission with nativeSession := 2 }
  let selected := if includeDelivery then [forkedFirst, forkedLast] else [forkedFirst]
  ({ snapshot selected with admission := request },
    { input (first :: last :: selected) with request })

def wrongStreamInput (headers : List MessageEnvelope) : ClosureInput :=
  { input headers with segments := (terminalRecords tool).map fun segment =>
      { segment with flush := segment.flush.map fun flush =>
          { flush with runs := flush.runs.map fun run =>
              { run with declaration := run.declaration.map fun declaration =>
                  { declaration with kind := .text } } } } }

def windowCases : List (String × Snapshot × ClosureInput × Tool) :=
  let valid := [call 10 3, delivered 11 5]
  let wrong := delivered 11 5
  let wrong := { wrong with header := { wrong.header with request := some 701 } }
  let completeFork := forkInput true
  let cutFork := forkInput false
  [ ("wrong_terminal_stream_kind", snapshot valid, wrongStreamInput valid, tool)
  , ("authored_background_receipt", snapshot valid,
      { input valid with segments := (terminalRecords tool).map fun segment =>
        { segment with coordinate := ⟨tool.request, .authored 11⟩ } }, tool)
  , ("missing_terminal_output", snapshot valid, { input valid with segments := [] }, tool)
  , ("canonical_invocation_result", snapshot valid, input valid, tool)
  , ("missing_delivery", snapshot [call 10 3], input [call 10 3], tool)
  , ("delivery_beyond_selected_cut", snapshot [call 10 3], input valid, tool)
  , ("wrong_request", snapshot [call 10 3, wrong], input [call 10 3, wrong], tool)
  , ("unselected_tool", snapshot valid, input valid, { tool with id := 91 })
  , ("conflicting_sequence", snapshot [call 10 3, delivered 11 3],
      input [call 10 3, delivered 11 3], tool)
  , ("reversed_sequence", snapshot [call 10 5, delivered 11 3],
      input [call 10 5, delivered 11 3], tool)
  , ("forked_pair", completeFork.1, completeFork.2, tool)
  , ("fork_cut_before_delivery", cutFork.1, cutFork.2, tool)
  , ("foreign_closure", snapshot valid,
      { input valid with request := { admission with requester := "other-reader" } }, tool) ]

def resolutionCases : List (String × List Window) :=
  [ ("empty", [])
  , ("singleton", [⟨90, 1, 3⟩])
  , ("reversed_physical_ids", [⟨2, 5, 7⟩, ⟨90, 1, 3⟩, ⟨1, 8, 10⟩])
  , ("overlap", [⟨90, 1, 7⟩, ⟨2, 5, 8⟩])
  , ("shared_admission", [⟨90, 1, 3⟩, ⟨2, 1, 4⟩])
  , ("duplicate_tool", [⟨90, 1, 3⟩, ⟨90, 5, 7⟩])
  , ("reversed_window", [⟨90, 3, 1⟩]) ]

def comparisonCases : List (String × Snapshot × ClosureInput × String × String × String) :=
  let documents := capturedSnapshot.documents.filter (fun key => key.id != 11)
  let receiptClosure := { input sequentialHeaders with segments :=
    ((terminalRecords tool).map fun segment =>
      { segment with coordinate := ⟨tool.request, .authored 11⟩ }) ++ terminalRecords secondTool }
  let receiptSnapshot := { capturedSnapshot with ordering := (capturedSnapshot.tools.map fun fact =>
      window capturedSnapshot receiptClosure fact.tool) }
  [ ("background_receipt_resource", receiptSnapshot, receiptClosure, "node", "workspace", "file.rs")
  , ("sequential_resource", capturedSnapshot, input sequentialHeaders, "node", "workspace", "file.rs")
  , ("other_resource", capturedSnapshot, input sequentialHeaders, "node", "workspace", "other.rs")
  , ("other_source", capturedSnapshot, input sequentialHeaders, "other-node", "workspace", "file.rs")
  , ("missing_intermediate_delivery", { capturedSnapshot with documents },
      input sequentialHeaders, "node", "workspace", "file.rs") ]

def entryCases : List (String × List Entry) :=
  let entries := capturedSnapshot.entries
  [ ("canonical_ordinals_in_physical_page_order", entries)
  , ("repeated_ordinal", entries.map fun entry => { entry with order := .ordered 0 2 })
  , ("inconsistent_resource_count", entries.map fun entry =>
      if entry.tool == 90 then { entry with order := .ordered 0 3 } else entry)
  , ("mixed_resolved_and_unknown", entries.map fun entry =>
      if entry.tool == 90 then { entry with order := .unresolved } else entry)
  , ("out_of_range_ordinal", entries.map fun entry => { entry with order := .ordered 2 2 })
  , ("unknown_order_is_explicit", entries.map fun entry => { entry with order := .unresolved }) ]

theorem ordinal_representation_checks :
    entryCases.map (fun value => coherentEntries value.2) =
      [true, false, false, false, false, true] := by native_decide

theorem authored_background_receipt_cannot_order_a_terminal_tool :
    let headers := [call 10 3, delivered 11 5]
    let receipt := (terminalRecords tool).map fun segment =>
      { segment with coordinate := ⟨tool.request, .authored 11⟩ }
    window (snapshot headers) { input headers with segments := receipt } tool = none := by
  native_decide

theorem missing_terminal_output_cannot_order_a_terminal_tool :
    let headers := [call 10 3, delivered 11 5]
    window (snapshot headers) { input headers with segments := [] } tool = none := by
  native_decide

theorem non_tool_stream_cannot_supply_a_terminal_window :
    let headers := [call 10 3, delivered 11 5]
    window (snapshot headers) (wrongStreamInput headers) tool = none := by
  native_decide

end SessionHydration.FileInventory.Order.Examples
