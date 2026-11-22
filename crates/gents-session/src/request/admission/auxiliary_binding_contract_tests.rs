use super::child_scope_fixture::ChildScopeWorld;
use gents_lean_vocab::families::session_child_child_scope_binding as family;

#[tokio::test]
async fn modeled_request_child_bindings_use_real_node_and_agent_signatures() {
    for case in family::cases() {
        let world = ChildScopeWorld::build(&case.input).await;
        let allowed = world.bound(&case.input).await;
        case.check(family::BindingOutput { allowed });
        world.node.shutdown().await.unwrap();
    }
}
