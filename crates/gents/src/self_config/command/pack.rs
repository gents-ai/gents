use super::*;

impl ConfigCommandTool {
    pub(super) fn plugin(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(
            self.core.held_grants().pack_install || self.categories.contains("tools"),
            "plugin inspection requires the pack or tools grant"
        );
        let home = self
            .plugins
            .home()
            .context("this runtime has no plugin home")?;
        match argv.first().map(String::as_str) {
            Some("list") => {
                let parsed = ParsedArgs::parse(&argv[1..])?;
                parsed.reject_mutation_flags()?;
                anyhow::ensure!(
                    parsed.positionals.is_empty()
                        && parsed
                            .options
                            .keys()
                            .all(|name| matches!(name.as_str(), "limit" | "cursor")),
                    "plugin list accepts options.limit and options.cursor only"
                );
                let limit = parse_limit(&parsed)?;
                let cursor = parsed.one("cursor")?;
                let mut records = crate::plugin::store::list_records(home)?;
                records.sort_by_key(|record| format!("{}/{}", record.namespace, record.name));
                let mut items = records.into_iter().filter(|record| cursor.is_none_or(|cursor| format!("{}/{}", record.namespace, record.name).as_str() > cursor)).take(limit + 1).map(|record| json!({"plugin":format!("{}/{}",record.namespace,record.name),"version":record.version,"digest":record.digest,"owner_pack_coordinate":record.owner_pack_coordinate})).collect::<Vec<_>>();
                let truncated = items.len() > limit;
                items.truncate(limit);
                let next_cursor = truncated
                    .then(|| {
                        items
                            .last()
                            .and_then(|item| item["plugin"].as_str())
                            .map(ToOwned::to_owned)
                    })
                    .flatten();
                ordered! {"items": items, "next_cursor": next_cursor, "next_call": json!({"argv":["plugin","get","<PLUGIN>"]})}.pretty()
            }
            Some("get") if argv.len() == 2 => {
                let (namespace, name) = crate::plugin::store::parse_coordinate(&argv[1])?;
                let record = crate::plugin::store::read_record(home, namespace, name)?;
                ordered! {
                    "plugin": argv[1],
                    "selection": json!({"plugin":argv[1],"digest":record.digest}),
                    "next_call": json!({"argv":["tools","get"]}),
                    "guidance": "Read the target Tools document and preserve its other integration settings and plugin entries. Explicitly update integrations.plugins with the pinned selection, then read tools get and effective config get for resolved readback. Start a fresh request to call the tool; installation alone does not expose it to the current request.",
                    "installed": record,
                }.pretty()
            }
            _ => bail!("plugin supports list and get; call [\"help\",\"plugin\"]"),
        }
    }

    pub(super) async fn pack(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(
            self.core.held_grants().pack_install,
            "pack installation is not granted"
        );
        let installer = PackInstaller {
            core: self.core.clone(),
            node: self.node.clone(),
            home: self.plugins.home().map(std::path::Path::to_path_buf),
        };
        let verb = argv
            .first()
            .map(String::as_str)
            .context("pack command is required; see [\"help\",\"pack\"]")?;
        if matches!(verb, "scaffold" | "check" | "verify" | "build" | "test") {
            return self.authoring(verb, &argv[1..]).await;
        }
        match verb {
            "search" => {
                let parsed = ParsedArgs::parse(&argv[1..])?;
                anyhow::ensure!(
                    parsed.switches.is_empty() && parsed.positionals.len() <= 1,
                    "pack search takes an optional query and options.page; see [\"help\",\"pack\"]"
                );
                for name in parsed.options.keys() {
                    anyhow::ensure!(
                        matches!(name.as_str(), "query" | "page"),
                        "unknown pack search option {name:?}; use options.query and options.page"
                    );
                }
                let option_query = parsed.one("query")?;
                anyhow::ensure!(
                    option_query.is_none() || parsed.positionals.is_empty(),
                    "supply the search query in argv or options.query, not both"
                );
                let query = option_query
                    .or_else(|| parsed.positionals.first().map(String::as_str))
                    .unwrap_or("");
                let page = parsed
                    .one("page")?
                    .unwrap_or("1")
                    .parse::<u32>()
                    .context("pack search options.page must be a positive integer")?;
                anyhow::ensure!(page > 0, "pack search options.page starts at 1");
                installer.search(query, page).await
            }
            "list" => {
                let parsed = ParsedArgs::parse(&argv[1..])?;
                parsed.reject_mutation_flags()?;
                installer
                    .list(parse_limit(&parsed)?, parsed.one("cursor")?)
                    .await
            }
            "get" => {
                anyhow::ensure!(argv.len() == 2, "pack get requires exactly one PACKAGE");
                installer.get(&argv[1]).await
            }
            "preview" => {
                let operation = argv
                    .get(1)
                    .context("pack preview requires install or update")?;
                anyhow::ensure!(
                    matches!(operation.as_str(), "install" | "update"),
                    "pack preview supports install and update"
                );
                installer
                    .preview(operation, parse_pack_change(&argv[2..])?)
                    .await
            }
            "install" | "update" => {
                let change = parse_pack_change(&argv[1..])?;
                self.execution.enter_mutation();
                installer.apply(verb, change).await
            }
            "remove" => {
                anyhow::ensure!(argv.len() == 2, "pack remove requires exactly one PACKAGE");
                self.execution.enter_mutation();
                installer.remove(&argv[1]).await
            }
            other => bail!("unknown pack command {other:?}; see [\"help\",\"pack\"]"),
        }
    }
}

fn parse_pack_change(argv: &[String]) -> Result<PackInstallParams> {
    let package = argv
        .first()
        .context("pack operation requires PACKAGE")?
        .clone();
    let parsed = ParsedArgs::parse(&argv[1..])?;
    anyhow::ensure!(
        parsed.positionals.is_empty() && parsed.switches.is_empty(),
        "unexpected pack argument; see [\"help\",\"pack\"]"
    );
    for name in parsed.options.keys() {
        anyhow::ensure!(
            matches!(name.as_str(), "var" | "inference-slot" | "digest"),
            "unknown pack option --{name}; accepted: --inference-slot, --var, --digest"
        );
    }
    let pairs = |name: &str| -> Result<BTreeMap<String, String>> {
        let mut values = BTreeMap::new();
        for binding in parsed.options.get(name).into_iter().flatten() {
            let (key, value) = binding
                .split_once('=')
                .with_context(|| format!("--{name} must be NAME=VALUE"))?;
            anyhow::ensure!(
                !key.is_empty() && !value.is_empty(),
                "--{name} requires non-empty NAME and VALUE"
            );
            anyhow::ensure!(
                values.insert(key.to_owned(), value.to_owned()).is_none(),
                "duplicate --{name} name {key:?}"
            );
        }
        Ok(values)
    };
    Ok(PackInstallParams {
        package,
        variables: pairs("var")?,
        inference_slots: pairs("inference-slot")?,
        expected_digest: parsed.one("digest")?.map(ToOwned::to_owned),
    })
}
