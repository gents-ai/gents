use super::*;

use std::collections::BTreeMap;

#[test]
fn default_allocation_bounds_nothing() {
    // The pre-budget behaviour of every request in the repo: stamping this
    // allocation on a request must be observationally a no-op.
    let allocation = BudgetAllocation::unbounded();
    assert!(allocation.is_unbounded());
    assert!(allocation.max_turns.is_none());
    assert!(allocation.max_tool_calls.is_none());
    assert!(allocation.deadline.is_none());
    assert!(allocation.max_descendants.is_none());

    let mut bounded = BudgetAllocation::unbounded();
    bounded.max_turns = Some(0);
    assert!(!bounded.is_unbounded());
}

#[test]
fn allocation_round_trips_through_its_json_column() {
    let mut allocation = BudgetAllocation::unbounded();
    allocation.max_turns = Some(12);
    allocation.max_tool_calls_by_class = Some(BTreeMap::from([
        (ToolClass::Subagent, 2),
        (ToolClass::Cli, 5),
    ]));
    allocation.accounting = Some(CostAccounting::new(serde_json::json!({"tier": "cheap"})));

    let encoded = serde_json::to_string(&allocation).expect("encode allocation_json");
    let decoded: BudgetAllocation = serde_json::from_str(&encoded).expect("decode allocation_json");

    assert_eq!(decoded, allocation);
    // Opaque accounting rides through untouched.
    assert_eq!(
        decoded.accounting.expect("accounting").into_value(),
        serde_json::json!({"tier": "cheap"})
    );
}

#[test]
fn derivation_variants_are_tagged_on_the_wire() {
    let cases = [
        (BudgetDerivation::Root, "root"),
        (
            BudgetDerivation::EvenSplit {
                siblings: 4,
                ordinal: 0,
            },
            "even_split",
        ),
        (
            BudgetDerivation::Reserved {
                reserve_fraction_bp: 2_500,
            },
            "reserved",
        ),
        (
            BudgetDerivation::Grant {
                granted_by_budget_id: "budget-parent".to_string(),
                sequence: 1,
            },
            "grant",
        ),
    ];

    for (derivation, kind) in cases {
        let encoded = serde_json::to_value(&derivation).expect("encode derivation_json");
        assert_eq!(encoded["kind"], kind);
        let decoded: BudgetDerivation =
            serde_json::from_value(encoded).expect("decode derivation_json");
        assert_eq!(decoded, derivation);
    }
}

#[test]
fn tool_class_wire_names_match_serde() {
    for class in [
        ToolClass::Native,
        ToolClass::Subagent,
        ToolClass::BackgroundProcess,
        ToolClass::Meta,
        ToolClass::SelfConfig,
        ToolClass::Document,
        ToolClass::Introspection,
        ToolClass::GraphPipeline,
        ToolClass::Mcp,
        ToolClass::Cli,
    ] {
        let encoded = serde_json::to_value(class).expect("encode tool class");
        assert_eq!(
            encoded,
            serde_json::Value::String(class.as_str().to_string()),
            "as_str must match the serde representation for {class:?}"
        );
    }
}

#[test]
fn execution_budget_round_trips_with_its_lineage() {
    let budget = ExecutionBudget {
        budget_id: "budget-child".to_string(),
        budget_group_id: "group-1".to_string(),
        parent_budget_id: Some("budget-root".to_string()),
        depth: 1,
        allocation: BudgetAllocation {
            max_turns: Some(10),
            ..BudgetAllocation::unbounded()
        },
        derivation: BudgetDerivation::EvenSplit {
            siblings: 2,
            ordinal: 1,
        },
    };

    let encoded = serde_json::to_string(&budget).expect("encode budget");
    let decoded: ExecutionBudget = serde_json::from_str(&encoded).expect("decode budget");
    assert_eq!(decoded, budget);
}

#[test]
fn consumption_starts_at_zero() {
    // Nothing derived from an empty subtree can be nonzero, so the default
    // is the only honest starting point for a fold.
    let consumption = BudgetConsumption::default();
    assert_eq!(consumption.turns, 0);
    assert_eq!(consumption.tool_calls, 0);
    assert_eq!(consumption.total_tokens, 0);
    assert_eq!(consumption.descendants, 0);
    assert!(consumption.tool_calls_by_class.is_empty());
    assert_eq!(consumption.elapsed, std::time::Duration::ZERO);
}
