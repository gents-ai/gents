use super::*;

pub(super) fn mailbox_entries(policy: crate::mailbox::MailboxNotificationPolicy) -> Result<Value> {
    policy.validate()?;
    let mut declaration = crate::mailbox::canonical_mailbox_write_decl();
    declaration.notification = Some(policy);
    Ok(json!([crate::document_config::SurfaceToolDecl::Create(
        declaration
    )]))
}

pub(super) fn surface_patch(argv: &[String]) -> Result<SelfConfigPatch> {
    let mut parsed = ParsedArgs::parse(argv)?;
    if let Some(policy) = parsed.one("mailbox")? {
        let policy: crate::mailbox::MailboxNotificationPolicy = serde_json::from_str(policy)
            .context("--mailbox requires a notification policy JSON object")?;
        let entries = mailbox_entries(policy)?;
        parsed.options.remove("mailbox");
        parsed
            .options
            .entry("set".into())
            .or_default()
            .push(format!("entries={entries}"));
    }
    parse_patch_args(parsed, SelfConfigTarget::DatastoreToolSurface)
}

#[test]
fn mailbox_option_uses_canonical_declaration_and_rejects_ambiguous_patches() {
    let policy =
        json!({"identity":{"mode":"condition","key":"host-health"},"kind":"flag","action":"ack"});
    let args = vec![
        "--mailbox".into(),
        policy.to_string(),
        "--set".into(),
        "enabled=true".into(),
    ];
    let patch = surface_patch(&args).unwrap();
    let mut canonical = crate::mailbox::canonical_mailbox_write_decl();
    canonical.notification = Some(serde_json::from_value(policy).unwrap());
    assert!(patch.contains(&(
        "entries".into(),
        Some(json!([crate::document_config::SurfaceToolDecl::Create(
            canonical
        )]))
    )));
    for extra in [vec!["--set", "entries=[]"], vec!["--clear", "entries"]] {
        let mut conflicting = args.clone();
        conflicting.extend(extra.into_iter().map(String::from));
        assert!(surface_patch(&conflicting).is_err());
    }
    assert!(surface_patch(&["--mailbox".into(), "{}".into()]).is_err());
    let mut duplicate = args.clone();
    duplicate.extend(args);
    assert!(surface_patch(&duplicate).is_err());
}

impl ConfigCommandTool {
    pub(super) async fn datastore(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("tools")?;
        let preview = argv.first().is_some_and(|arg| arg == "preview");
        let argv = if preview { &argv[1..] } else { argv };
        let verb = argv
            .first()
            .map(String::as_str)
            .context("see [\"help\",\"datastore\"]")?;
        let listing = !preview && (verb == "list" || verb == "get" && argv.len() == 1);
        anyhow::ensure!(
            !listing,
            "surface IDs are listed in an agent's Tools: read {{\"argv\":[\"tools\",\"get\"],\"options\":{{\"agent\":\"AGENT_ID\"}}}}, then [\"datastore\",\"get\",SURFACE_ID] for one of its datastore.datastore_tool_surface_ids"
        );
        let id = required_resource_id(argv.get(1), "SURFACE_ID")?;
        let target = SelfConfigTarget::DatastoreToolSurface;
        if verb == "get" && !preview {
            anyhow::ensure!(argv.len() == 2, "datastore get accepts only SURFACE_ID");
            return self.exact_read(target, id).await;
        }
        anyhow::ensure!(
            matches!(verb, "create" | "edit"),
            "see [\"help\",\"datastore\"]; expected create or edit"
        );
        let mut request = ApplyRequest::new(target, surface_patch(&argv[2..])?);
        let surface_id = id.clone();
        request.resolve_unique = Box::new(move |_| Ok(surface_id.clone()));
        request.allow_create = verb == "create";
        request.require_create = verb == "create";
        let owner = self.node_did.clone();
        request.on_create = Box::new(move |id, doc| {
            doc.insert("surface_id".into(), json!(id));
            doc.insert("node_did".into(), json!(owner));
            Ok(())
        });
        self.patch(
            &self.core,
            if preview { "preview" } else { "edit" },
            request,
        )
        .await
    }
}
