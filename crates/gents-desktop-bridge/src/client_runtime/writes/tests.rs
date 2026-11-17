use super::*;

#[test]
fn chat_write_fence_matches_lean() {
    use gents_protocol::runtime_connection::ConnectionRefusalCause;
    for case in gents_lean_vocab::families::chat_readiness_cases_write_fence::cases::<
        ConnectionRefusalCause,
    >() {
        let connection = case.input.route.as_ref().map(|route| RuntimeConnection {
            node_did: if route.binding_matches {
                "did:key:runtime"
            } else {
                "did:key:other"
            }
            .into(),
            label: "Runtime".into(),
            graphql: if route.operator_endpoint {
                "http://localhost:1234/api/v0/graphql"
            } else {
                ""
            }
            .into(),
            local_node_home: None,
            access_request: None,
            access_state: None,
        });
        case.check(
            gents_lean_vocab::families::chat_readiness_cases_write_fence::FenceOutput {
                refusal: chat_route_refusal("did:key:runtime", connection.as_ref())
                    .map(|refusal| refusal.cause()),
            },
        );
    }
}
