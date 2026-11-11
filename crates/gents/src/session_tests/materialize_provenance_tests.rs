use gents_session::request::lifecycle::materialize::*;

    #[tokio::test]
    async fn pending_receipt_lookup_uses_the_selected_agent() {
        use gents_toolcalls::call::lifecycle::admission_fixture::{
            published_admission_with_owner, PublishedAdmissionOptions,
        };

        let (admission, parent) = published_admission_with_owner(PublishedAdmissionOptions {
            name: "pending-receipt-agent".into(),
            ..Default::default()
        })
        .await
        .unwrap();
        let access = admission.tool.access().unwrap();
        let receipt = defra_node::QueryResponse::success(serde_json::json!({
            "create_AgentRequest": [{}]
        }));
        let escaped = escape_graphql_string(&parent.request().request_id);
        let resolved = resolve_created_agent_request_doc_id(
            &access,
            &receipt,
            "create_AgentRequest",
            &escaped,
            "lookup failed",
            "missing receipt",
        )
        .await
        .unwrap();
        assert_eq!(resolved, parent.request().doc_id);
        assert!(resolve_created_agent_request_doc_id(
            &gents_store::config_client::ConfigAccess::local(admission.node.clone()),
            &receipt,
            "create_AgentRequest",
            &escaped,
            "lookup failed",
            "missing receipt",
        )
        .await
        .is_err());
        admission.node.shutdown().await.unwrap();
    }
