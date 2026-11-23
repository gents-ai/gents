use super::child_scope_fixture::ChildScopeWorld;
use gents_lean_vocab::families::session_child_child_scope_repair as family;

#[tokio::test]
async fn modeled_auxiliary_child_repair_requires_signed_exact_parent_and_native_owner() {
    for case in family::cases() {
        let input = serde_json::from_value(serde_json::to_value(&case.input).unwrap()).unwrap();
        let world = ChildScopeWorld::build(&input).await;
        let observed = world.repair(&input).await;
        case.check(family::RepairOutput {
            parent_granted: observed.parent_granted,
            parent_linked: observed.parent_linked,
        });
        world.node.shutdown().await.unwrap();
    }
}
