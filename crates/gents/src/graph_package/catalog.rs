use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::graph_pipeline::{EntryBinding, ResultContract};
pub use crate::pack::PackageExternalDependency;
use crate::pack::{bundled_pack_asset, PackInstallOptions, BUNDLED_GRAPH_PACKAGE_NAMES};

/// Graph packages use the common manifest and canonical configuration loader.
pub type GraphPackageManifest = crate::pack::PackManifest;
pub type PackageCapabilityTemplate = crate::graph_pipeline::StageCapability;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphPackageCatalogEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub package_digest: String,
    pub compiler_version: String,
    pub external_dependencies: Vec<PackageExternalDependency>,
    pub entries: Vec<EntryBinding>,
    pub results: Vec<ResultContract>,
    pub capabilities: Vec<PackageCapabilityTemplate>,
}

#[derive(Clone, Debug)]
pub struct BundledGraphPackage {
    pub manifest: GraphPackageManifest,
    pub config: crate::document_config::PackConfig,
    pub package_digest: String,
    asset_paths: Vec<String>,
}

impl BundledGraphPackage {
    pub fn asset(&self, path: &str) -> Result<&'static [u8]> {
        anyhow::ensure!(
            self.asset_paths.iter().any(|declared| declared == path),
            "asset {path:?} is not declared by package {}",
            self.manifest.name
        );
        bundled_pack_asset(&self.manifest.name, path)
            .with_context(|| format!("bundled asset {path:?} is missing"))
    }

    pub fn asset_text(&self, path: &str) -> Result<&'static str> {
        std::str::from_utf8(self.asset(path)?)
            .with_context(|| format!("bundled asset {path:?} is not UTF-8"))
    }

    pub fn catalog_entry(&self) -> GraphPackageCatalogEntry {
        GraphPackageCatalogEntry {
            name: self.manifest.name.clone(),
            version: self.manifest.version.clone(),
            description: self.manifest.description.clone(),
            package_digest: self.package_digest.clone(),
            compiler_version: crate::graph_pipeline::COMPILER_VERSION.to_owned(),
            external_dependencies: self.manifest.external_dependencies.clone(),
            entries: self
                .config
                .graph_intents
                .iter()
                .flat_map(|intent| intent.entries.clone())
                .collect(),
            results: self
                .config
                .graph_intents
                .iter()
                .flat_map(|intent| intent.results.clone())
                .collect(),
            capabilities: self.config.graph_capabilities.clone(),
        }
    }
}

pub(crate) fn digest_assets(package_name: &str, paths: &[String]) -> Result<String> {
    let mut hasher = Sha256::new();
    for path in paths {
        let bytes = bundled_pack_asset(package_name, path)
            .with_context(|| format!("bundled package references missing asset {path:?}"))?;
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub(crate) fn load_package(
    distribution: &crate::pack::ResolvedPack,
    options: &PackInstallOptions,
    environment: &dyn Fn(&str) -> Option<String>,
) -> Result<BundledGraphPackage> {
    crate::pack::validate_pack_manifest(&distribution.manifest)?;
    anyhow::ensure!(
        distribution.manifest.metadata.kind == crate::pack::PackKind::Graph,
        "pack is not a graph"
    );
    let manifest = distribution.manifest.clone();
    let config = crate::pack::load_pack_config(
        &manifest,
        options,
        &|path| Ok(distribution.asset(path)?.to_vec()),
        environment,
    )?;
    let mut asset_paths = manifest.metadata.assets.clone();
    asset_paths.push("manifest.json".to_owned());
    asset_paths.sort();
    asset_paths.dedup();
    let package_digest = digest_assets(&manifest.name, &asset_paths)?;
    anyhow::ensure!(
        package_digest == distribution.digest,
        "graph distribution digest changed after resolution"
    );
    // Capabilities reference the same owned Task documents as ordinary packs.
    // Port/topology/caller/schema checks remain in the compiler and publication
    // owner; this loader never constructs behavior/model/tool overrides.
    for surface in &config.datastore_tool_surfaces {
        for entry in surface.entries.iter().flatten() {
            entry
                .validate()
                .with_context(|| format!("invalid datastore surface {}", surface.surface_id))?;
        }
    }
    for capability in &config.graph_capabilities {
        anyhow::ensure!(
            capability.agent_did == options.agent_did,
            "foreign graph capability owner"
        );
        anyhow::ensure!(
            config
                .tasks
                .iter()
                .filter(|task| task.agent_did == capability.agent_did
                    && task.task_id == capability.task_id)
                .count()
                == 1,
            "graph capability {} must reference exactly one owned task {}",
            capability.capability_id,
            capability.task_id
        );
    }
    Ok(BundledGraphPackage {
        manifest,
        config,
        package_digest,
        asset_paths,
    })
}

pub fn load_bundled_graph_package(
    name: &str,
    options: &PackInstallOptions,
) -> Result<BundledGraphPackage> {
    anyhow::ensure!(
        BUNDLED_GRAPH_PACKAGE_NAMES.contains(&name),
        "unknown bundled graph package {name:?}"
    );
    load_package(&crate::pack::resolve_pack(name)?, options, &|name| {
        std::env::var(name).ok()
    })
}

pub fn load_resolved_graph_package(
    distribution: &crate::pack::ResolvedPack,
    options: &PackInstallOptions,
) -> Result<BundledGraphPackage> {
    load_package(distribution, options, &|name| std::env::var(name).ok())
}

pub fn graph_package_catalog(
    options: &PackInstallOptions,
) -> Result<Vec<GraphPackageCatalogEntry>> {
    BUNDLED_GRAPH_PACKAGE_NAMES
        .iter()
        .map(|name| Ok(load_bundled_graph_package(name, options)?.catalog_entry()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::{RemoteToolStyle, Task, Tools};
    use crate::document_config::{SurfaceToolDecl, WriteToolFieldFill};
    use crate::graph_pipeline::{compile_graph, CompilerPolicy, StageCapability};
    use crate::tool_surface::{BashMode, FileToolMode};
    use crate::toolset::{CommandExecutionMode, CommandNetworkMode};

    fn options() -> PackInstallOptions {
        PackInstallOptions {
            agent_did: "did:key:fixture".to_owned(),
        }
    }
    fn fixture_package(name: &str) -> Result<BundledGraphPackage> {
        load_package(
            &crate::pack::resolve_pack(name)?,
            &options(),
            &|variable| match variable {
                "GENTS_REVIEW_MODEL" | "GENTS_WEB_RESEARCH_MODEL" => {
                    Some("selected-model".to_owned())
                }
                "GENTS_REVIEW_ENDPOINT" | "GENTS_WEB_RESEARCH_ENDPOINT" => {
                    Some("http://inference.example/v1".to_owned())
                }
                _ => None,
            },
        )
    }
    fn task<'a>(package: &'a BundledGraphPackage, capability: &StageCapability) -> &'a Task {
        let rows = package
            .config
            .tasks
            .iter()
            .filter(|task| {
                task.agent_did == capability.agent_did && task.task_id == capability.task_id
            })
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        rows[0]
    }
    fn tools<'a>(package: &'a BundledGraphPackage, capability: &StageCapability) -> &'a Tools {
        let task = task(package, capability);
        let behavior = package
            .config
            .agent_behaviors
            .iter()
            .find(|row| row.agent_did == task.agent_did && row.behavior_id == task.behavior_id)
            .unwrap();
        let context = package
            .config
            .contexts
            .iter()
            .find(|row| {
                row.agent_did == task.agent_did
                    && Some(&row.context_id) == behavior.context_id.as_ref()
            })
            .unwrap();
        package
            .config
            .tools
            .iter()
            .find(|row| {
                row.agent_did == task.agent_did && Some(&row.tools_id) == context.tools_id.as_ref()
            })
            .unwrap()
    }
    fn surface_entries<'a>(package: &'a BundledGraphPackage, id: &str) -> &'a [SurfaceToolDecl] {
        package
            .config
            .datastore_tool_surfaces
            .iter()
            .find(|row| row.agent_did == options().agent_did && row.surface_id == id)
            .unwrap()
            .entries
            .as_deref()
            .unwrap()
    }
    fn assert_no_host_tools(tools: &Tools) {
        let host = tools.host.clone().unwrap_or_default();
        assert_eq!(host.files.unwrap_or_default().mode, FileToolMode::Off);
        let bash = host.bash.unwrap_or_default();
        assert_eq!(bash.mode, BashMode::Off);
        assert_eq!(bash.execution_mode, None);
        assert_eq!(bash.network_mode, None);
        assert!(!bash.background_enabled);
        assert!(host.cli.is_empty());
        assert!(tools
            .integrations
            .as_ref()
            .and_then(|group| group.lsp.as_ref())
            .is_none());
    }
    #[test]
    fn bundled_catalog_is_read_only_complete_and_compiler_valid() {
        let package = fixture_package("code_review").unwrap();
        let catalog = BUNDLED_GRAPH_PACKAGE_NAMES
            .iter()
            .map(|name| fixture_package(name).unwrap().catalog_entry())
            .collect::<Vec<_>>();
        assert!(catalog.len() >= 2);
        assert!(package.package_digest.starts_with("sha256:"));
        for path in &package.asset_paths {
            assert!(!package.asset(path).unwrap().is_empty(), "{path}");
        }
        let plan = compile_graph(
            &package.config.graph_intents[0],
            &package.config.graph_capabilities,
            "did:key:fixture",
            &CompilerPolicy::default(),
        )
        .unwrap();
        assert_eq!(plan.nodes.len(), 4);
        assert_eq!(plan.results.len(), 2);
        assert_eq!(plan.entries[0].name, "review");
    }

    #[test]
    fn bundled_tools_use_explicit_goal_authority() {
        for package_name in BUNDLED_GRAPH_PACKAGE_NAMES {
            let package = fixture_package(package_name).unwrap();
            for capability in &package.config.graph_capabilities {
                let builtins = tools(&package, capability)
                    .built_ins
                    .clone()
                    .unwrap_or_default();
                assert!(!builtins.enable_goal_creation.unwrap_or(false));
            }
        }
    }

    #[test]
    fn code_review_tasks_have_bounded_goals_without_creation_authority() {
        let package = fixture_package("code_review").unwrap();
        assert_eq!(package.config.graph_capabilities.len(), 4);
        for capability in &package.config.graph_capabilities {
            let task = task(&package, capability);
            let objective = task.goal_objective_template.as_deref().unwrap();
            let budget = task.goal_token_budget.unwrap();
            assert!(!objective.trim().is_empty());
            assert!(budget > 0);
            crate::goal::validate_task_goal_declaration(Some(objective), Some(budget)).unwrap();
            let builtins = tools(&package, capability).built_ins.as_ref().unwrap();
            assert!(builtins.enable_goal_tools.unwrap_or(false));
            assert!(!builtins.enable_goal_creation.unwrap_or(false));
        }
    }

    #[test]
    fn web_deep_research_package_is_complete_and_compiler_valid() {
        let package = fixture_package("web_deep_research").unwrap();
        assert!(package.package_digest.starts_with("sha256:"));
        assert_eq!(package.manifest.external_dependencies.len(), 1);
        let dependency = &package.manifest.external_dependencies[0];
        assert_eq!(dependency.service_id, "web-research-mcp");
        assert_eq!(dependency.install_command, "./scripts/stack install-mcp");
        for path in &package.asset_paths {
            assert!(!package.asset(path).unwrap().is_empty(), "{path}");
        }
        let capabilities = package
            .config
            .graph_capabilities
            .iter()
            .map(|template| {
                let mut capability = template.clone();
                capability.allowed_callers = vec!["did:key:fixture".to_owned()];
                capability
            })
            .collect::<Vec<_>>();
        let plan = compile_graph(
            &package.config.graph_intents[0],
            &capabilities,
            "did:key:fixture",
            &CompilerPolicy::default(),
        )
        .unwrap();
        assert_eq!(plan.nodes.len(), 4);
        assert_eq!(plan.results.len(), 6);
        assert_eq!(plan.entries[0].name, "research");
        for capability in &package.config.graph_capabilities {
            let selection = tools(&package, capability);
            assert_no_host_tools(selection);
            let builtins = selection.built_ins.clone().unwrap_or_default();
            assert!(!builtins.enable_memory.unwrap_or(false));
            assert!(!builtins.enable_session_history_tool.unwrap_or(false));
            assert!(!builtins.enable_context_budget.unwrap_or(false));
            let subagents = selection.subagents.clone().unwrap_or_default();
            assert!(!subagents.spawn_enabled.unwrap_or(false));
            assert!(!subagents.steering_enabled.unwrap_or(false));
            assert!(!subagents.background_enabled.unwrap_or(false));
            assert!(!selection
                .self_config
                .clone()
                .unwrap_or_default()
                .enable_self_config
                .unwrap_or(false));
            let datastore = selection.datastore.as_ref().unwrap();
            assert!(!datastore.enable_defra_query.unwrap_or(false));
            let (uses_remote, surface_id) = match capability.capability_id.as_str() {
                "research-plan" => (true, "research-plan-writes"),
                "research-investigate" => (true, "research-investigate-writes"),
                "research-adjudicate" => (false, "research-adjudicate-io"),
                "research-report" => (false, "research-report-io"),
                other => panic!("unexpected capability {other}"),
            };
            let remote = selection.remote.clone().unwrap_or_default();
            if uses_remote {
                assert_eq!(remote.services.len(), 1);
                let service = &remote.services[0];
                assert_eq!(service.mcp_service_id, "web-research-mcp");
                assert!(service.required);
                assert_eq!(service.style, RemoteToolStyle::Discovery);
                assert!(!service.tool_names.is_empty());
                assert!(service.tool_names.iter().all(|name| !name.contains('*')));
            } else {
                assert!(remote.services.is_empty());
            }
            assert_eq!(
                datastore.datastore_tool_surface_ids.as_deref(),
                Some(&[surface_id.to_owned()][..])
            );
            assert_eq!(capability.workspace_authority, None);
        }
    }

    #[test]
    fn web_deep_research_handoffs_are_typed_and_correlation_scoped() {
        let package = fixture_package("web_deep_research").unwrap();
        for (asset, fields) in [
            (
                "tasks/research_plan_task/prompt.md",
                &[
                    "question",
                    "scope",
                    "freshness",
                    "audience",
                    "output_requirements",
                    "investigator_count",
                ][..],
            ),
            (
                "tasks/research_investigate_task/prompt.md",
                &[
                    "assignment_id",
                    "question",
                    "lens",
                    "instructions",
                    "query_plan",
                    "source_requirements",
                    "freshness",
                ][..],
            ),
            (
                "tasks/research_report_task/prompt.md",
                &[
                    "title",
                    "thesis",
                    "outline",
                    "synthesis",
                    "unresolved_questions",
                ][..],
            ),
        ] {
            let prompt = package.asset_text(asset).unwrap();
            for field in fields {
                assert!(
                    prompt.contains(&format!("{{{{ doc.{field} }}}}")),
                    "{asset} does not interpolate typed carrier field {field}"
                );
            }
        }
        let adjudicate_prompt = package
            .asset_text("tasks/research_adjudicate_task/prompt.md")
            .unwrap();
        assert!(adjudicate_prompt.contains("{{ group.correlation_value }}"));
        assert!(adjudicate_prompt.contains("{{ group.count }}"));
        assert!(!adjudicate_prompt.contains("event.group_size"));

        let expected_surface_tools = [
            (
                "research-plan-writes",
                vec!["write_research_assignment", "write_research_plan"],
            ),
            (
                "research-investigate-writes",
                vec![
                    "write_research_source",
                    "write_research_claim",
                    "write_research_evidence",
                    "write_research_investigation",
                ],
            ),
            (
                "research-adjudicate-io",
                vec![
                    "read_research_investigation",
                    "read_research_source",
                    "read_research_claim",
                    "read_research_evidence",
                    "write_research_claim_verdict",
                    "write_research_draft",
                ],
            ),
            (
                "research-report-io",
                vec![
                    "read_report_research_source",
                    "read_report_research_evidence",
                    "read_report_claim_verdict",
                    "write_research_result",
                ],
            ),
        ];
        for (asset, expected_tools) in expected_surface_tools {
            let surface = surface_entries(&package, asset);
            assert_eq!(
                surface
                    .iter()
                    .map(SurfaceToolDecl::tool_name)
                    .collect::<Vec<_>>(),
                expected_tools,
                "unexpected authority in {asset}"
            );
            for entry in surface {
                let fields = match entry {
                    SurfaceToolDecl::Create(entry) => &entry.fields,
                    SurfaceToolDecl::Query(entry) => &entry.filter_fields,
                };
                let run_id = fields
                    .iter()
                    .find(|field| field.name == "run_id")
                    .unwrap_or_else(|| {
                        panic!("{} has no correlation filter/fill", entry.tool_name())
                    });
                assert!(!run_id.required, "{}", entry.tool_name());
                assert_eq!(
                    run_id.fill,
                    Some(WriteToolFieldFill::Correlation),
                    "{}",
                    entry.tool_name()
                );
            }
        }

        let investigator = surface_entries(&package, "research-investigate-writes");
        for (tool_name, minimum_writes) in [
            ("write_research_source", 2),
            ("write_research_claim", 6),
            ("write_research_evidence", 6),
        ] {
            let SurfaceToolDecl::Create(decl) = investigator
                .iter()
                .find(|entry| entry.tool_name() == tool_name)
                .unwrap_or_else(|| panic!("missing {tool_name}"))
            else {
                panic!("{tool_name} must be a create tool");
            };
            assert_eq!(
                decl.output_obligation
                    .as_ref()
                    .map(|obligation| obligation.minimum_writes),
                Some(minimum_writes),
                "{tool_name} must enforce its investigator minimum"
            );
        }
        let SurfaceToolDecl::Create(evidence_write) = investigator
            .iter()
            .find(|entry| entry.tool_name() == "write_research_evidence")
            .unwrap()
        else {
            unreachable!()
        };
        assert!(evidence_write
            .fields
            .iter()
            .all(|field| !matches!(field.name.as_str(), "fetch_id" | "content_hash")));
        let evidence_schema = package
            .asset_text("schemas/research_evidence.graphql")
            .unwrap();
        assert!(!evidence_schema.contains("fetch_id"));
        assert!(!evidence_schema.contains("content_hash"));

        let all_assets = package
            .asset_paths
            .iter()
            .map(|path| package.asset_text(path).unwrap())
            .collect::<String>();
        assert!(!all_assets.contains("evidence_json"));
        for typed_field in [
            "verified_quote",
            "quote_verified",
            "evidence_id",
            "source_id",
            "fetch_id",
            "content_hash",
            "matched_query",
            "retrieval_queries",
            "search_engines",
            "candidate_relevance_score",
            "content_relevance_score",
            "extraction_method",
            "content_integrity_verified",
            "evidence_shortfall",
            "search_degradation",
            "retrieval_failures",
            "relationship",
            "evidence_summary",
        ] {
            assert!(all_assets.contains(typed_field), "missing {typed_field}");
        }
    }

    #[test]
    fn code_review_scan_writes_use_the_trigger_area_id() {
        let package = fixture_package("code_review").unwrap();
        let surface = surface_entries(&package, "review-scan-writes");
        for tool_name in ["write_candidate_finding", "write_scan_result"] {
            let entry = surface
                .iter()
                .find(|entry| entry.tool_name() == tool_name)
                .unwrap();
            let SurfaceToolDecl::Create(entry) = entry else {
                panic!("{tool_name} must be a create tool");
            };
            let area_id = entry
                .fields
                .iter()
                .find(|field| field.name == "area_id")
                .unwrap();
            assert!(!area_id.required, "{tool_name}");
            assert_eq!(
                area_id.fill,
                Some(WriteToolFieldFill::SourceField("area_id".to_owned())),
                "{tool_name}"
            );
        }
    }

    #[test]
    fn code_review_evidence_handoff_is_compact_and_correlation_scoped() {
        let package = fixture_package("code_review").unwrap();
        let surface = surface_entries(&package, "review-recon-writes");
        let SurfaceToolDecl::Create(entry) = &surface[0] else {
            panic!("review recon writer must be a create tool");
        };
        let repository_path = entry
            .fields
            .iter()
            .find(|field| field.name == "repository_path")
            .unwrap();
        assert!(!repository_path.required);
        assert_eq!(
            repository_path.fill,
            Some(WriteToolFieldFill::SourceField(
                "repository_path".to_owned()
            ))
        );
        let evidence_id = entry
            .fields
            .iter()
            .find(|field| field.name == "evidence_id")
            .unwrap();
        assert!(!evidence_id.required);
        assert_eq!(
            evidence_id.fill,
            Some(WriteToolFieldFill::SourceField("evidence_id".to_owned()))
        );
        assert!(entry.fields.iter().all(|field| field.name != "evidence"));
        let expected_total = entry
            .fields
            .iter()
            .find(|field| field.name == "expected_total")
            .unwrap();
        assert!(expected_total.required);
        assert_eq!(expected_total.fill, None);
        let recon_prompt = package
            .asset_text("tasks/review_recon_task/prompt.md")
            .unwrap();
        assert!(recon_prompt.contains("{{ doc.evidence_summary }}"));
        assert!(!recon_prompt.contains("{{ doc.evidence }}"));
        let scan_prompt = package
            .asset_text("tasks/review_scan_task/prompt.md")
            .unwrap();
        assert!(!scan_prompt.contains("{{ doc.evidence }}"));

        let scan_surface = surface_entries(&package, "review-scan-writes");
        let manifest_tool = scan_surface
            .iter()
            .find(|entry| entry.tool_name() == "read_review_evidence_manifest")
            .unwrap();
        let SurfaceToolDecl::Query(manifest_tool) = manifest_tool else {
            panic!("review evidence manifest must be a query tool");
        };
        assert_eq!(manifest_tool.collection, "CodeReviewEvidenceManifest");
        assert_eq!(manifest_tool.filter_fields.len(), 1);
        assert_eq!(manifest_tool.filter_fields[0].name, "evidence_id");
        assert_eq!(
            manifest_tool.filter_fields[0].fill,
            Some(WriteToolFieldFill::SourceField("evidence_id".to_owned()))
        );

        let page_tool = scan_surface
            .iter()
            .find(|entry| entry.tool_name() == "read_review_evidence_page")
            .unwrap();
        let SurfaceToolDecl::Query(page_tool) = page_tool else {
            panic!("review evidence page must be a query tool");
        };
        assert_eq!(page_tool.collection, "CodeReviewEvidencePage");
        assert_eq!(page_tool.filter_fields.len(), 2);
        assert_eq!(page_tool.filter_fields[0].name, "evidence_id");
        assert_eq!(
            page_tool.filter_fields[0].fill,
            Some(WriteToolFieldFill::SourceField("evidence_id".to_owned()))
        );
        assert_eq!(page_tool.filter_fields[1].name, "page_index");
        assert!(page_tool.filter_fields[1].required);
        assert_eq!(page_tool.filter_fields[1].fill, None);
        let expected_chunk_fields = (0..16)
            .map(|chunk| format!("evidence_chunk_{chunk}"))
            .collect::<Vec<_>>();
        assert!(expected_chunk_fields
            .iter()
            .all(|field| page_tool.fields.contains(field)));

        let manifest_schema = package
            .asset_text("schemas/evidence_manifest.graphql")
            .unwrap();
        assert!(manifest_schema.starts_with("type CodeReviewEvidenceManifest {"));
        let page_schema = package.asset_text("schemas/evidence_page.graphql").unwrap();
        assert!(page_schema.starts_with("type CodeReviewEvidencePage {"));
        assert!(page_schema.contains("page_key: String @index(unique: true) @immutable"));
        assert!(page_schema.contains("evidence_chunk_15: String @immutable"));

        assert!(scan_prompt.contains("read_review_evidence_manifest"));
        assert!(scan_prompt.contains("read_review_evidence_page"));
        assert!(scan_prompt.contains("page_count - 1"));
        assert!(scan_prompt.contains("evidence_chunk_15"));
        assert!(!scan_prompt.contains("read_review_evidence_0"));
    }

    #[test]
    fn code_review_stages_use_task_specific_least_privilege_tools() {
        let package = fixture_package("code_review").unwrap();
        for capability_id in ["review-recon", "review-scan"] {
            let capability = package
                .config
                .graph_capabilities
                .iter()
                .find(|row| row.capability_id == capability_id)
                .unwrap();
            let selection = tools(&package, capability);
            assert_no_host_tools(selection);
            assert!(!selection
                .built_ins
                .clone()
                .unwrap_or_default()
                .enable_context_budget
                .unwrap_or(false));
        }
        let capability = package
            .config
            .graph_capabilities
            .iter()
            .find(|row| row.capability_id == "review-verify")
            .unwrap();
        let selection = tools(&package, capability);
        let host = selection.host.as_ref().unwrap();
        assert_eq!(host.files.as_ref().unwrap().mode, FileToolMode::ReadOnly);
        let bash = host.bash.as_ref().unwrap();
        assert_eq!(bash.mode, BashMode::Unrestricted);
        assert_eq!(
            bash.execution_mode,
            Some(CommandExecutionMode::ArtifactWrite)
        );
        assert_eq!(bash.network_mode, Some(CommandNetworkMode::Disabled));
        assert!(bash.background_enabled);
        assert!(host.cli.is_empty());
        assert!(selection
            .integrations
            .as_ref()
            .and_then(|group| group.lsp.as_ref())
            .is_none());
        assert!(selection
            .built_ins
            .as_ref()
            .unwrap()
            .enable_context_budget
            .unwrap_or(false));
    }

    #[test]
    fn code_review_stages_are_durable_goal_controlled() {
        let package = fixture_package("code_review").unwrap();
        for capability in &package.config.graph_capabilities {
            let task = task(&package, capability);
            assert!(task
                .goal_objective_template
                .as_deref()
                .is_some_and(|objective| !objective.trim().is_empty()));
            assert!(task.goal_token_budget.is_some_and(|budget| budget > 0));
            let builtins = tools(&package, capability).built_ins.as_ref().unwrap();
            assert!(builtins.enable_goal_tools.unwrap_or(false));
            assert!(!builtins.enable_goal_creation.unwrap_or(false));
            assert!(task.prompt_template.contains("`update_goal`"));
            assert!(task.prompt_template.contains("`status=\"complete\"`"));
        }
    }
}
