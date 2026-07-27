use gents_desktop_core::local_runtime::runtime_status_url;

mod operations_holds;
pub(crate) mod support;

#[test]
fn peer_status_url_accepts_bare_host_and_graphql_endpoint() {
    assert_eq!(
        runtime_status_url("127.0.0.1:9181").expect("bare host should normalize"),
        "http://127.0.0.1:9181/status"
    );
    assert_eq!(
        runtime_status_url("http://127.0.0.1:9181/api/v0/graphql")
            .expect("graphql endpoint should normalize"),
        "http://127.0.0.1:9181/status"
    );
}
