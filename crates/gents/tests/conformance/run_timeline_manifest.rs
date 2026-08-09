use std::collections::BTreeMap;

use crate::lean_vocab_test::lean_run_timeline_manifest_cases;
use gents::run_timeline_manifest::{
    freeze_timeline_manifest_with_declared_edges, TimelineCoverageGap, TimelineCoverageGapKind,
    TimelineDeclaredExactEdge, TimelineExpectedSlot, TimelineManifestStatus,
    TimelineObservedSource, TimelineOmissionReason, TimelineRootCandidate, TimelineRootSelector,
    TimelineSlotRequirement, TimelineSourceClass, TimelineSourceDecision, TimelineSourceSlot,
    RUN_TIMELINE_MANIFEST_VERSION,
};
use gents::{DocumentVersionRef, SignedDocumentVersionRef};

struct Scenario {
    selector: TimelineRootSelector,
    candidates: Vec<TimelineRootCandidate>,
    expected: Vec<TimelineExpectedSlot>,
    observed: Vec<TimelineObservedSource>,
    decisions: Vec<TimelineSourceDecision>,
    coverage_gaps: Vec<TimelineCoverageGap>,
    declared_edges: Vec<TimelineDeclaredExactEdge>,
}

fn exact(doc_id: usize, cid: usize) -> SignedDocumentVersionRef {
    SignedDocumentVersionRef {
        version: DocumentVersionRef {
            doc_id: doc_id.to_string(),
            composite_commit_cid: cid.to_string(),
        },
        signer_did: "7".to_string(),
    }
}

fn unsigned(doc_id: usize, cid: usize) -> SignedDocumentVersionRef {
    SignedDocumentVersionRef {
        signer_did: String::new(),
        ..exact(doc_id, cid)
    }
}

fn source_slot(source_class: TimelineSourceClass) -> TimelineSourceSlot {
    TimelineSourceSlot::new(source_class, 0)
}

fn collection_version_id(collection: &str) -> &'static str {
    match collection {
        "AgentRequest" => "100",
        "AgentResponseOutcome" => "200",
        "InferenceCall" => "300",
        "RenderedRequest" => "400",
        "AgentMessage" => "600",
        "AgentResponse" => "150",
        "CompactionEntry" => "500",
        other => panic!("unmapped test collection {other}"),
    }
}

fn collection_contract_id(collection: &str) -> usize {
    match collection {
        "AgentRequest" => 10,
        "AgentResponse" => 15,
        "AgentResponseOutcome" => 20,
        "InferenceCall" => 30,
        "RenderedRequest" => 40,
        "AgentMessage" => 60,
        "CompactionEntry" => 50,
        other => panic!("unmapped test collection {other}"),
    }
}

fn observed_source(
    slot: &TimelineSourceSlot,
    collection: &str,
    exact: SignedDocumentVersionRef,
) -> TimelineObservedSource {
    TimelineObservedSource {
        slot: slot.clone(),
        collection: collection.to_string(),
        collection_version_id: collection_version_id(collection).to_string(),
        exact,
    }
}

fn include(
    slot: &TimelineSourceSlot,
    collection: &str,
    exact: SignedDocumentVersionRef,
) -> TimelineSourceDecision {
    TimelineSourceDecision::Include {
        slot: slot.clone(),
        collection: collection.to_string(),
        collection_version_id: collection_version_id(collection).to_string(),
        exact,
    }
}

fn omit(
    slot: &TimelineSourceSlot,
    collection: &str,
    reason: TimelineOmissionReason,
) -> TimelineSourceDecision {
    TimelineSourceDecision::Omit {
        slot: slot.clone(),
        collection: collection.to_string(),
        reason,
    }
}

fn base_scenario() -> Scenario {
    let request_slot = source_slot(TimelineSourceClass::Request);
    let outcome_slot = source_slot(TimelineSourceClass::ResponseOutcome);
    let call_slot = source_slot(TimelineSourceClass::InferenceCall);
    let render_slot = source_slot(TimelineSourceClass::RenderedRequest);
    let request = exact(101, 1001);
    let outcome = exact(201, 2001);
    let call = exact(301, 3001);
    let render = exact(401, 4001);
    Scenario {
        selector: TimelineRootSelector::Exact(request.clone()),
        candidates: vec![TimelineRootCandidate {
            request_id: "1".to_string(),
            exact: request.clone(),
            current_head_count: 1,
        }],
        expected: vec![
            TimelineExpectedSlot {
                slot: request_slot.clone(),
                requirement: TimelineSlotRequirement::Required,
            },
            TimelineExpectedSlot {
                slot: outcome_slot.clone(),
                requirement: TimelineSlotRequirement::Required,
            },
            TimelineExpectedSlot {
                slot: call_slot.clone(),
                requirement: TimelineSlotRequirement::Required,
            },
            TimelineExpectedSlot {
                slot: render_slot.clone(),
                requirement: TimelineSlotRequirement::Optional,
            },
        ],
        observed: vec![
            observed_source(&request_slot, "AgentRequest", request.clone()),
            observed_source(&outcome_slot, "AgentResponseOutcome", outcome.clone()),
            observed_source(&call_slot, "InferenceCall", call.clone()),
            observed_source(&render_slot, "RenderedRequest", render.clone()),
        ],
        decisions: vec![
            include(&request_slot, "AgentRequest", request),
            include(&outcome_slot, "AgentResponseOutcome", outcome),
            include(&call_slot, "InferenceCall", call),
            include(&render_slot, "RenderedRequest", render),
        ],
        coverage_gaps: Vec::new(),
        declared_edges: Vec::new(),
    }
}

fn scenario(name: &str) -> Scenario {
    let mut scenario = base_scenario();
    let request_slot = source_slot(TimelineSourceClass::Request);
    let outcome_slot = source_slot(TimelineSourceClass::ResponseOutcome);
    let render_slot = source_slot(TimelineSourceClass::RenderedRequest);
    match name {
        "exact_root_selected" | "exact_sources_frozen" => {}
        "nested_provenance_edge_frozen"
        | "nested_provenance_schema_rebind_rejected"
        | "nested_provenance_signer_rebind_rejected" => {
            let message_slot = source_slot(TimelineSourceClass::Message);
            let message = exact(601, 6001);
            scenario.expected.insert(
                1,
                TimelineExpectedSlot {
                    slot: message_slot.clone(),
                    requirement: TimelineSlotRequirement::Required,
                },
            );
            scenario.observed.insert(
                1,
                observed_source(&message_slot, "AgentMessage", message.clone()),
            );
            scenario
                .decisions
                .insert(1, include(&message_slot, "AgentMessage", message.clone()));
            let mut edge = TimelineDeclaredExactEdge {
                collection: "AgentMessage".to_string(),
                collection_version_id: "600".to_string(),
                exact: message,
            };
            if name == "nested_provenance_schema_rebind_rejected" {
                edge.collection_version_id = "499".to_string();
            } else if name == "nested_provenance_signer_rebind_rejected" {
                edge.exact.signer_did = "8".to_string();
            }
            scenario.declared_edges.push(edge);
        }
        "missing_nested_provenance_edge_rejected" => {
            scenario.declared_edges.push(TimelineDeclaredExactEdge {
                collection: "AgentMessage".to_string(),
                collection_version_id: "600".to_string(),
                exact: exact(601, 6001),
            });
        }
        "unique_logical_root_selected" => {
            scenario.selector = TimelineRootSelector::LogicalRequestId("1".to_string());
        }
        "missing_logical_root_rejected" => {
            scenario.selector = TimelineRootSelector::LogicalRequestId("1".to_string());
            scenario.candidates.clear();
        }
        "ambiguous_logical_root_rejected" => {
            scenario.selector = TimelineRootSelector::LogicalRequestId("1".to_string());
            scenario.candidates.push(TimelineRootCandidate {
                request_id: "1".to_string(),
                exact: exact(102, 1002),
                current_head_count: 1,
            });
        }
        "exact_root_wrong_cid_rejected" => {
            scenario.selector = TimelineRootSelector::Exact(exact(101, 1999));
        }
        "exact_root_multiple_heads_selected" => scenario.candidates[0].current_head_count = 2,
        "unsigned_root_rejected" => {
            scenario.selector = TimelineRootSelector::LogicalRequestId("1".to_string());
            scenario.candidates[0].exact = unsigned(101, 1001);
        }
        "source_cid_rebind_rejected" => {
            scenario.observed[3].exact = exact(401, 4999);
        }
        "source_doc_rebind_rejected" => {
            scenario.observed[3].exact = exact(499, 4001);
        }
        "source_signer_rebind_rejected" => {
            scenario.observed[3].exact.signer_did = "8".to_string();
        }
        "source_collection_rebind_rejected" => {
            scenario.observed[3].collection = "Collection49".to_string();
        }
        "source_schema_version_rebind_rejected" => {
            scenario.observed[3].collection_version_id = "499".to_string();
        }
        "unsigned_source_rejected" => {
            scenario.observed[3].exact = unsigned(401, 4001);
            scenario.decisions[3] = include(&render_slot, "RenderedRequest", unsigned(401, 4001));
        }
        "duplicate_observed_slot_rejected" => {
            scenario.observed.push(observed_source(
                &render_slot,
                "RenderedRequest",
                exact(401, 4001),
            ));
        }
        "undeclared_source_decision_rejected" => {
            let slot = source_slot(TimelineSourceClass::Compaction);
            scenario.decisions.push(omit(
                &slot,
                "CompactionEntry",
                TimelineOmissionReason::NotApplicable,
            ));
        }
        "undeclared_observed_source_rejected" => {
            let slot = source_slot(TimelineSourceClass::Compaction);
            scenario
                .observed
                .push(observed_source(&slot, "CompactionEntry", exact(501, 5001)));
        }
        "reversed_decision_input_emits_canonical_order" => scenario.decisions.reverse(),
        "duplicate_expected_slot_rejected" => {
            scenario.expected.push(TimelineExpectedSlot {
                slot: render_slot,
                requirement: TimelineSlotRequirement::Optional,
            });
        }
        "noncanonical_expected_order_rejected" => scenario.expected.reverse(),
        "optional_unsent_render_explicitly_omitted" => {
            scenario.observed.pop();
            scenario.decisions[3] = omit(
                &render_slot,
                "RenderedRequest",
                TimelineOmissionReason::NotProduced,
            );
        }
        "optional_redacted_render_explicitly_omitted" => {
            scenario.decisions[3] = omit(
                &render_slot,
                "RenderedRequest",
                TimelineOmissionReason::Redacted,
            );
        }
        "optional_live_projection_explicitly_omitted" => {
            let live_slot = source_slot(TimelineSourceClass::ResponseLive);
            scenario.expected.insert(
                1,
                TimelineExpectedSlot {
                    slot: live_slot.clone(),
                    requirement: TimelineSlotRequirement::Optional,
                },
            );
            scenario.decisions.insert(
                1,
                omit(
                    &live_slot,
                    "AgentResponse",
                    TimelineOmissionReason::ProjectionExcluded,
                ),
            );
        }
        "required_source_omission_rejected" => {
            scenario.decisions[1] = omit(
                &outcome_slot,
                "AgentResponseOutcome",
                TimelineOmissionReason::LegacyUnavailable,
            );
        }
        "missing_decision_rejected" => {
            scenario.decisions.pop();
        }
        "canonical_coverage_gaps_are_partial_exact" => {
            scenario.coverage_gaps = vec![
                TimelineCoverageGap {
                    kind: TimelineCoverageGapKind::OpenLogicalExtent,
                    source_class: TimelineSourceClass::Message,
                    collection: "60".into(),
                    scope_id: "1".into(),
                },
                TimelineCoverageGap {
                    kind: TimelineCoverageGapKind::OpenSessionExtent,
                    source_class: TimelineSourceClass::SessionProjection,
                    collection: "70".into(),
                    scope_id: "2".into(),
                },
                TimelineCoverageGap {
                    kind: TimelineCoverageGapKind::NonAtomicObservation,
                    source_class: TimelineSourceClass::ResponseOutcome,
                    collection: "20".into(),
                    scope_id: "1".into(),
                },
                TimelineCoverageGap {
                    kind: TimelineCoverageGapKind::RemoteSignatureUnverified,
                    source_class: TimelineSourceClass::RenderedRequest,
                    collection: "40".into(),
                    scope_id: "1".into(),
                },
            ];
        }
        "duplicate_coverage_gap_rejected" => {
            let gap = TimelineCoverageGap {
                kind: TimelineCoverageGapKind::OpenLogicalExtent,
                source_class: TimelineSourceClass::Message,
                collection: "60".into(),
                scope_id: "1".into(),
            };
            scenario.coverage_gaps = vec![gap.clone(), gap];
        }
        "noncanonical_coverage_gap_order_rejected" => {
            scenario.coverage_gaps = vec![
                TimelineCoverageGap {
                    kind: TimelineCoverageGapKind::RemoteSignatureUnverified,
                    source_class: TimelineSourceClass::RenderedRequest,
                    collection: "40".into(),
                    scope_id: "1".into(),
                },
                TimelineCoverageGap {
                    kind: TimelineCoverageGapKind::OpenLogicalExtent,
                    source_class: TimelineSourceClass::Message,
                    collection: "60".into(),
                    scope_id: "1".into(),
                },
            ];
        }
        "remote_signature_gap_is_partial_exact" => {
            scenario.coverage_gaps = vec![TimelineCoverageGap {
                kind: TimelineCoverageGapKind::RemoteSignatureUnverified,
                source_class: TimelineSourceClass::RenderedRequest,
                collection: "40".into(),
                scope_id: "1".into(),
            }];
        }
        other => panic!("unmapped Lean run-timeline manifest case {other}"),
    }
    let _ = request_slot;
    scenario
}

pub(crate) fn generated_cases_pin_exact_roots_membership_order_and_omissions() {
    let cases = lean_run_timeline_manifest_cases()
        .iter()
        .map(|case| (case.name.as_str(), case))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(cases.len(), 33);

    for case in cases.values() {
        let scenario = scenario(&case.name);
        let result = freeze_timeline_manifest_with_declared_edges(
            &scenario.selector,
            &scenario.candidates,
            &scenario.expected,
            &scenario.observed,
            &scenario.decisions,
            &scenario.coverage_gaps,
            &scenario.declared_edges,
        );
        assert_eq!(
            result.is_ok(),
            case.disposition == "accepted",
            "production manifest disposition drifted from Lean for {}: {result:?}",
            case.name
        );
        if let Ok(manifest) = result {
            assert_eq!(
                manifest.root.version.doc_id.parse::<usize>().ok(),
                case.root_doc_id,
                "case {}",
                case.name
            );
            assert_eq!(
                manifest
                    .root
                    .version
                    .composite_commit_cid
                    .parse::<usize>()
                    .ok(),
                case.root_cid,
                "case {}",
                case.name
            );
            assert_eq!(
                manifest.sources.len(),
                case.included_slots,
                "case {}",
                case.name
            );
            assert_eq!(
                manifest.omissions.len(),
                case.omitted_slots,
                "case {}",
                case.name
            );
            assert_eq!(
                manifest.manifest_version as usize,
                case.manifest_version.unwrap()
            );
            assert_eq!(manifest.coverage_gaps.len(), case.coverage_gap_count);
            assert_eq!(
                manifest.status,
                if case.manifest_status.as_deref() == Some("verified_exact") {
                    TimelineManifestStatus::VerifiedExact
                } else {
                    TimelineManifestStatus::PartialExact
                }
            );
            assert_eq!(
                manifest.sources.len() + manifest.omissions.len(),
                case.expected_slots,
                "case {}",
                case.name
            );
            assert_eq!(
                manifest
                    .sources
                    .iter()
                    .map(|source| collection_contract_id(&source.collection))
                    .collect::<Vec<_>>(),
                case.ordered_collections,
                "case {}",
                case.name
            );
            assert_eq!(
                manifest
                    .sources
                    .iter()
                    .map(|source| source.collection_version_id.parse::<usize>().unwrap())
                    .collect::<Vec<_>>(),
                case.ordered_collection_version_ids,
                "case {}",
                case.name
            );
        }
    }

    for name in [
        "exact_root_selected",
        "unique_logical_root_selected",
        "exact_sources_frozen",
        "nested_provenance_edge_frozen",
        "exact_root_multiple_heads_selected",
        "reversed_decision_input_emits_canonical_order",
        "optional_unsent_render_explicitly_omitted",
        "optional_redacted_render_explicitly_omitted",
        "optional_live_projection_explicitly_omitted",
    ] {
        let case = cases[name];
        assert_eq!(case.disposition, "accepted", "case {name}");
        assert!(case.exact_membership, "case {name}");
        assert!(case.complete_coverage, "case {name}");
        assert!(case.canonical_order, "case {name}");
        assert_eq!(case.root_doc_id, Some(101), "case {name}");
        assert_eq!(case.root_cid, Some(1001), "case {name}");
    }

    for name in [
        "missing_logical_root_rejected",
        "ambiguous_logical_root_rejected",
        "exact_root_wrong_cid_rejected",
        "unsigned_root_rejected",
        "missing_nested_provenance_edge_rejected",
        "nested_provenance_schema_rebind_rejected",
        "nested_provenance_signer_rebind_rejected",
        "source_cid_rebind_rejected",
        "source_doc_rebind_rejected",
        "source_signer_rebind_rejected",
        "source_collection_rebind_rejected",
        "source_schema_version_rebind_rejected",
        "unsigned_source_rejected",
        "duplicate_observed_slot_rejected",
        "undeclared_source_decision_rejected",
        "undeclared_observed_source_rejected",
        "duplicate_expected_slot_rejected",
        "noncanonical_expected_order_rejected",
        "required_source_omission_rejected",
        "missing_decision_rejected",
        "duplicate_coverage_gap_rejected",
        "noncanonical_coverage_gap_order_rejected",
    ] {
        assert_eq!(cases[name].disposition, "rejected", "case {name}");
    }

    let canonical = cases["reversed_decision_input_emits_canonical_order"];
    assert_eq!(
        canonical.ordered_source_classes,
        [
            "request",
            "response_outcome",
            "inference_call",
            "rendered_request"
        ]
    );
    assert_eq!(canonical.ordered_doc_ids, [101, 201, 301, 401]);
    assert_eq!(canonical.ordered_cids, [1001, 2001, 3001, 4001]);
    assert_eq!(canonical.ordered_collections, [10, 20, 30, 40]);
    assert_eq!(
        canonical.ordered_collection_version_ids,
        [100, 200, 300, 400]
    );

    let omitted = cases["optional_unsent_render_explicitly_omitted"];
    assert_eq!(omitted.expected_slots, 4);
    assert_eq!(omitted.included_slots, 3);
    assert_eq!(omitted.omitted_slots, 1);
    for name in [
        "optional_unsent_render_explicitly_omitted",
        "optional_redacted_render_explicitly_omitted",
        "optional_live_projection_explicitly_omitted",
    ] {
        assert_eq!(
            cases[name].manifest_status.as_deref(),
            Some("partial_exact"),
            "case {name}"
        );
    }
    assert_eq!(
        cases["exact_sources_frozen"].manifest_status.as_deref(),
        Some("verified_exact")
    );

    let partial = cases["canonical_coverage_gaps_are_partial_exact"];
    assert_eq!(
        partial.manifest_version,
        Some(RUN_TIMELINE_MANIFEST_VERSION as usize)
    );
    assert_eq!(partial.manifest_status.as_deref(), Some("partial_exact"));
    assert_eq!(partial.coverage_gap_count, 4);
    assert!(partial.canonical_gaps);
    assert_eq!(
        partial.ordered_coverage_gap_kinds,
        [
            "open_logical_extent",
            "open_session_extent",
            "non_atomic_observation",
            "remote_signature_unverified"
        ]
    );

    assert_eq!(cases["exact_root_selected"].selector, "exact");
    assert_eq!(cases["unique_logical_root_selected"].selector, "logical");
    assert_eq!(
        cases["ambiguous_logical_root_rejected"].visible_logical_roots,
        2
    );
}
