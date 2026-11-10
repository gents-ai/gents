use super::*;
use crate::plugin::tool::PluginTool;

impl ConfigCommandTool {
    pub(super) fn plugin(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(
            self.core.held_grants().pack_install || self.categories.contains("tools"),
            "plugin inspection requires the pack or tools grant"
        );
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
                let mut records = self.plugins.installed()?;
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
                let record = self.plugins.installed_record(&argv[1])?;
                ordered! {
                    "plugin": argv[1],
                    "selection": json!({"plugin":argv[1],"digest":record.digest}),
                    "next_call": json!({"argv":["tools","get"]}),
                    "guidance": "Read the target Tools document and preserve its other integration settings and plugin entries. Explicitly update integrations.plugins with the pinned selection, then read tools get and effective config get for resolved readback. Start a fresh request to call the tool; installation alone does not expose it to the current request.",
                    "installed": record,
                }.pretty()
            }
            _ => bail!("plugin supports build, list, get and run; call [\"help\",\"plugin\"]"),
        }
    }

    /// `plugin run`: one call of an installed plugin as a tool call of the
    /// calling agent makes it, through the plugin tool owner.
    pub(super) async fn plugin_run(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(
            self.core.held_grants().pack_install || self.categories.contains("tools"),
            "plugin run requires the pack or tools grant"
        );
        let parsed = ParsedArgs::parse(argv)?;
        let [coordinate] = parsed.positionals.as_slice() else {
            bail!("plugin run requires exactly one NAMESPACE/NAME; see [\"plugin\",\"list\"]");
        };
        anyhow::ensure!(
            parsed.switches.is_empty() && parsed.options.keys().all(|name| name == "input"),
            "plugin run takes NAMESPACE/NAME and options.input, the plugin's JSON input"
        );
        let input = match parsed.one("input")? {
            Some(raw) => {
                serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
            }
            None => Value::Null,
        };
        let (output, binding_note) =
            PluginTool::for_run(self.plugins.clone(), coordinate, &self.selected_plugins)?
                .with_agent_data(
                    self.node.clone(),
                    &self.node_did,
                    Some(self.core.agent_did()),
                )?
                .run_once(input)
                .await
                .with_context(|| format!("calling plugin {coordinate}"))?;
        ordered! {"output": output, "plugin": coordinate, "binding_note": binding_note}.pretty()
    }

    pub(super) async fn pack(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(
            self.core.held_grants().pack_install,
            "pack installation is not granted"
        );
        let installer = PackInstaller {
            core: self.core.clone(),
            node: self.node.clone(),
            actor: self.identity.clone(),
            home: self.plugins.home().map(std::path::Path::to_path_buf),
        };
        let verb = argv
            .first()
            .map(String::as_str)
            .context("pack command is required; see [\"help\",\"pack\"]")?;
        if matches!(
            verb,
            "scaffold" | "add" | "export" | "check" | "verify" | "build" | "test"
        ) {
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
                    .context("pack preview requires install, update or remove")?;
                if operation == "remove" {
                    anyhow::ensure!(argv.len() == 3, "pack preview remove takes one PACKAGE");
                    return installer.preview_remove(&argv[2]).await;
                }
                anyhow::ensure!(
                    matches!(operation.as_str(), "install" | "update"),
                    "pack preview supports install, update and remove"
                );
                let (change, _) = parse_pack_change(&argv[2..])?;
                installer.preview(operation, change).await
            }
            "install" | "update" => {
                let (change, edited) = parse_pack_change(&argv[1..])?;
                self.execution.enter_mutation();
                installer.apply(verb, change, edited.as_deref(), argv).await
            }
            "remove" => {
                let rest = argv[1..]
                    .iter()
                    .filter(|word| *word != "--with-collections")
                    .cloned()
                    .collect::<Vec<_>>();
                let with_collections = rest.len() + 1 < argv.len();
                let parsed = ParsedArgs::parse(&rest)?;
                anyhow::ensure!(
                    parsed.positionals.len() == 1
                        && parsed.switches.is_empty()
                        && parsed.options.keys().all(|name| name == "edited"),
                    "pack remove takes one PACKAGE and optionally --with-collections and options.edited (overwrite or keep)"
                );
                let edited = parsed.one("edited")?;
                self.execution.enter_mutation();
                installer
                    .remove(&parsed.positionals[0], with_collections, edited, argv)
                    .await
            }
            other => bail!("unknown pack command {other:?}; see [\"help\",\"pack\"]"),
        }
    }
}

fn parse_pack_change(argv: &[String]) -> Result<(PackChange, Option<String>)> {
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
            matches!(
                name.as_str(),
                "var" | "inference-slot" | "digest" | "edited"
            ),
            "unknown pack option --{name}; accepted: --inference-slot, --var, --digest, --edited"
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
    Ok((
        PackChange {
            package,
            variables: pairs("var")?,
            inference_slots: pairs("inference-slot")?,
            expected_digest: parsed.one("digest")?.map(ToOwned::to_owned),
        },
        parsed.one("edited")?.map(ToOwned::to_owned),
    ))
}
