//! The document door of the data path: a `Page` document's own `data` is the
//! declaration, read from the node the call names.

use super::*;

const PAGE: &str = "page/board";

impl Fixture {
    async fn write_page(&self, page: Value) {
        let document: crate::documents::PageDocument = serde_json::from_value(page).unwrap();
        let value = serde_json::to_value(document).unwrap();
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::Page,
            add: value.clone(),
            update: value,
        }])
        .unwrap();
        self.access
            .transact_write("test.ux_data.page", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await
            .unwrap();
    }

    fn board_page(&self, enabled: bool) -> Value {
        json!({
            "page_id": "board",
            "node_did": self.node_did,
            "display_name": "Board",
            "module": "export default { id: 'board', register() {} }",
            "data": [{"surface": "board", "tools": ["list_tasks", "add_task"]}],
            "enabled": enabled,
        })
    }
}

#[test]
fn page_data_admission_matches_lean() {
    let raw = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.PageDocument:olean",
        "Proofs/Conformance/PageDocument.lean",
    )
    .unwrap();
    let cases: Vec<Value> = serde_json::from_str(&raw).unwrap();
    assert_eq!(cases.len(), 8);
    for case in cases {
        let document = case["door"] == "document";
        let first = case["first"].as_bool().unwrap();
        let enabled = case["enabled"].as_bool().unwrap();
        assert_eq!(
            page_data_call_allowed(document, first, enabled),
            case["data_call"].as_bool().unwrap(),
            "{case}"
        );
        let loadable = if document {
            let page: crate::documents::PageDocument = serde_json::from_value(json!({
                "page_id": "board", "node_did": NODE, "module": "export default {}",
                "enabled": enabled,
            }))
            .unwrap();
            crate::plugin::ux::page::listing(&page)
                .unwrap()
                .default_enabled
                == Some(true)
        } else {
            first
        };
        assert_eq!(loadable, case["loadable"].as_bool().unwrap(), "{case}");
    }
}

#[tokio::test]
async fn a_page_document_calls_only_what_it_declares_while_enabled() {
    let fixture = Fixture::new().await;
    fixture.write_page(fixture.board_page(true)).await;

    let listings = crate::plugin::ux::page::list(&fixture.access, &fixture.node_did)
        .await
        .unwrap();
    assert_eq!(listings.len(), 1);
    assert_eq!(listings[0].id, PAGE);
    assert_eq!(listings[0].default_enabled, Some(true));

    let created = fixture
        .call_as(
            PAGE,
            &fixture.node_did,
            "board",
            "add_task",
            json!({"title": "Ship the page", "lane": "doing"}),
        )
        .await
        .unwrap();
    assert_eq!(created["kind"], "create");
    let listed = fixture
        .call_as(PAGE, &fixture.node_did, "board", "list_tasks", json!({}))
        .await
        .unwrap();
    assert_eq!(
        listed["output"]["results"],
        json!([{"title": "Ship the page", "lane": "doing"}])
    );

    let undeclared = fixture
        .call_as(PAGE, &fixture.node_did, "archive", "list_tasks", json!({}))
        .await
        .unwrap_err();
    assert!(
        format!("{undeclared:#}").contains("not declared"),
        "{undeclared:#}"
    );
    let listed = surfaces(
        fixture.home.path(),
        &fixture.access,
        PAGE,
        &fixture.node_did,
    )
    .await
    .unwrap();
    assert_eq!(listed["surfaces"][0]["surface"], "board");
    assert_eq!(listed["surfaces"][0]["tools"].as_array().unwrap().len(), 2);

    fixture.write_page(fixture.board_page(false)).await;
    let disabled = fixture
        .call_as(PAGE, &fixture.node_did, "board", "list_tasks", json!({}))
        .await
        .unwrap_err();
    assert!(
        format!("{disabled:#}").contains("is disabled"),
        "{disabled:#}"
    );
    let disabled = surfaces(
        fixture.home.path(),
        &fixture.access,
        PAGE,
        &fixture.node_did,
    )
    .await
    .unwrap_err();
    assert!(
        format!("{disabled:#}").contains("is disabled"),
        "{disabled:#}"
    );

    let missing = fixture
        .call_as(
            "page/absent",
            &fixture.node_did,
            "board",
            "list_tasks",
            json!({}),
        )
        .await
        .unwrap_err();
    assert!(format!("{missing:#}").contains("no page"), "{missing:#}");
    fixture.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_page_document_is_read_under_the_callers_own_access() {
    let fixture = Fixture::new().await;
    fixture.write_page(fixture.board_page(true)).await;
    let stranger = crate::store::identity::KeyIdentity::load_or_create(
        fixture.home.path().join("stranger.key"),
        None,
    )
    .unwrap();
    use crate::store::identity::NodeIdentity as _;
    let access = ConfigAccess::local_as(
        fixture.node.clone(),
        identity::Did::new(stranger.did()).unwrap(),
    );
    let seen = crate::plugin::ux::page::list(&access, &fixture.node_did).await;
    assert!(
        seen.map_or(true, |pages| pages.is_empty()),
        "an identity with no grant sees no page"
    );
    fixture.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn a_page_document_creates_through_the_desktops_remote_access() {
    let fixture = Fixture::new().await;
    fixture.write_page(fixture.board_page(true)).await;
    let page = fixture
        .access
        .execute("{ Page(filter: {page_id: {_eq: \"board\"}}) { _docID } }")
        .await
        .unwrap();
    fixture
        .access
        .add_actor_relationship(
            "Page",
            page["data"]["Page"][0]["_docID"].as_str().unwrap(),
            "reader",
            &fixture.client_did,
        )
        .await
        .unwrap();
    let request = |tool: &str, args: Value| UxDataCall {
        ux: PAGE.into(),
        node_did: fixture.node_did.clone(),
        surface: "board".into(),
        tool: tool.into(),
        args,
    };
    let created = call(
        fixture.home.path(),
        &fixture.client_access,
        request("add_task", json!({"title": "From the desktop", "lane": "doing"})),
    )
    .await
    .unwrap();
    assert_eq!(created["kind"], "create");
    let listed = call(
        fixture.home.path(),
        &fixture.client_access,
        request("list_tasks", json!({})),
    )
    .await
    .unwrap();
    assert_eq!(
        listed["output"]["results"],
        json!([{"title": "From the desktop", "lane": "doing"}])
    );

    fixture.write_page(fixture.board_page(false)).await;
    let disabled = call(
        fixture.home.path(),
        &fixture.client_access,
        request("add_task", json!({"title": "After disable", "lane": "doing"})),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{disabled:#}").contains("is disabled"),
        "{disabled:#}"
    );
    fixture.node.shutdown().await.expect("node shutdown");
}
