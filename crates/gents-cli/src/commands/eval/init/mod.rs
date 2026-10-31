use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use gents::eval::checks::CheckRegistry;

use self::contract::{first_turn, MAX_VALIDATION_ROUNDS, VALIDATION_PREFIX};
use self::dossier::Dossier;
use self::draft::parse_reply;
use self::turn::Turn;
use self::validate::{assemble, validate, Assembled, Floors};
use self::write::{case_table, commit, stage, Staged, Written};
use super::{Deps, EvalContext};
use crate::cli::EvalInitArgs;
use crate::commands::pack::{resolve_pack_source, single_slot_agent, PackSource};

mod contract;
pub(crate) mod dossier;
pub(crate) mod draft;
mod pilot;
mod readiness;
pub(crate) mod turn;
pub(super) mod validate;
mod write;

pub(crate) struct InitContext<'a> {
    pub(crate) dossier: Dossier,
    pub(crate) registry: &'a CheckRegistry,
    pub(crate) floors: Floors,
    pub(crate) definition_id: Option<String>,
    pub(crate) owner: String,
    pub(crate) out: PathBuf,
    pub(crate) force: bool,
    pub(crate) subject: String,
    pub(crate) subject_dir: PathBuf,
    pub(crate) profile: String,
    pub(crate) scope: crate::cli::EvalScopeArgs,
}

pub(crate) struct InitOutcome {
    pub(crate) written: Option<Written>,
    pub(crate) assembled: Option<Assembled>,
    pub(crate) summary: String,
    pub(crate) rounds: usize,
}

pub(crate) async fn interview(
    turn: &mut dyn Turn,
    lines: &mut dyn Iterator<Item = std::io::Result<String>>,
    ctx: &InitContext<'_>,
    out: &mut dyn Write,
) -> Result<InitOutcome> {
    let mut said: Vec<String> = Vec::new();
    let mut rounds = 0;
    let mut reply = turn
        .send(&first_turn(
            &ctx.dossier,
            &ctx.registry.catalog(),
            &ctx.floors,
        ))
        .await?;
    loop {
        if !turn.shows_replies() {
            writeln!(out, "{reply}")?;
        }
        let parsed = parse_reply(&reply);
        if let Ok(None) = parsed {
            let Some(line) = next_line(lines).context("reading the operator's answer")? else {
                return Ok(InitOutcome {
                    written: None,
                    assembled: None,
                    summary: said.join("\n"),
                    rounds,
                });
            };
            reply = turn.send(&line).await?;
            said.push(line);
            continue;
        }
        rounds += 1;
        let summary = said.join("\n");
        match checked(parsed, ctx, ctx.definition_id.as_deref(), &summary, None).await {
            Ok((assembled, staged)) => {
                let written = commit(
                    staged,
                    &ctx.out,
                    ctx.force,
                    &crate::home_state::resolve_home_dir(ctx.scope.home.as_deref()),
                )?;
                print_written(ctx, &assembled, &written, out)?;
                return Ok(InitOutcome {
                    written: Some(written),
                    assembled: Some(assembled),
                    summary,
                    rounds,
                });
            }
            Err(messages) => {
                let listed = format!("- {}", messages.join("\n- "));
                writeln!(out, "the draft did not validate:\n{listed}")?;
                if rounds == MAX_VALIDATION_ROUNDS {
                    return Err(anyhow::anyhow!(
                        "the author's draft did not validate after {MAX_VALIDATION_ROUNDS} rounds; nothing was written; last messages:\n{listed}"
                    ));
                }
                reply = turn
                    .send(&format!("{VALIDATION_PREFIX}\n\n{listed}"))
                    .await?;
            }
        }
    }
}

fn next_line(
    lines: &mut dyn Iterator<Item = std::io::Result<String>>,
) -> std::io::Result<Option<String>> {
    for line in lines {
        let line = line?;
        match line.trim() {
            "" => continue,
            "/quit" | "/exit" => return Ok(None),
            line => return Ok(Some(line.to_owned())),
        }
    }
    Ok(None)
}

fn operator_lines(
    mut read_line: impl FnMut(&mut String) -> std::io::Result<usize>,
) -> impl Iterator<Item = std::io::Result<String>> {
    std::iter::from_fn(move || {
        eprint!("> ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        match read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => Some(Ok(line)),
            Err(error) => Some(Err(error)),
        }
    })
}

async fn checked(
    parsed: Result<Option<draft::Draft>, String>,
    ctx: &InitContext<'_>,
    definition_id: Option<&str>,
    summary: &str,
    pilot: Option<&write::PilotNote>,
) -> Result<(Assembled, Staged), Vec<String>> {
    let draft = match parsed {
        Ok(Some(draft)) => draft,
        Ok(None) => return Err(vec!["the reply carries no fenced json draft".to_owned()]),
        Err(message) => return Err(vec![message]),
    };
    let assembled = assemble(&draft, definition_id, &ctx.owner, &ctx.dossier.slot)?;
    validate(&assembled, ctx.registry, &ctx.dossier, &ctx.floors)?;
    let staged = stage(&assembled, summary, &ctx.dossier, pilot).await?;
    Ok((assembled, staged))
}

fn print_written(
    ctx: &InitContext<'_>,
    assembled: &Assembled,
    written: &Written,
    out: &mut dyn Write,
) -> Result<()> {
    let definition = &assembled.definition;
    writeln!(
        out,
        "wrote eval definition {} as pack {} at {}\n",
        definition.definition_id,
        written.pack_name,
        written.out.display()
    )?;
    write!(out, "{}", case_table(definition))?;
    let scope = scope_flags(&ctx.scope);
    writeln!(
        out,
        "\nnext:\n  gents config apply --root {} --bind-node-did home{scope}\n  gents eval run {} --cell baseline={}:{} --profile baseline={}{scope}",
        written.out.display(),
        definition.definition_id,
        ctx.subject,
        ctx.dossier.agent_id,
        ctx.profile,
    )?;
    Ok(())
}

fn scope_flags(scope: &crate::cli::EvalScopeArgs) -> String {
    let mut flags = String::new();
    if let Some(home) = &scope.home {
        flags.push_str(&format!(" --home {}", home.display()));
    }
    if let Some(graphql) = scope
        .graphql
        .as_deref()
        .map(str::trim)
        .filter(|graphql| !graphql.is_empty())
    {
        flags.push_str(&format!(" --graphql {graphql}"));
    }
    flags
}

pub(crate) async fn preflight(args: &EvalInitArgs) -> Result<()> {
    anyhow::ensure!(
        args.force || args.out.symlink_metadata().is_err(),
        "{} already exists; pass --force to replace it",
        args.out.display()
    );
    let home_dir = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let graphql = match args
        .scope
        .graphql
        .as_deref()
        .map(str::trim)
        .filter(|graphql| !graphql.is_empty())
    {
        Some(graphql) => Some(graphql.to_owned()),
        None => crate::read_runtime_state(&home_dir)?.map(|state| state.graphql),
    };
    let served = match graphql {
        Some(graphql) => crate::graphql_access::graphql_endpoint_available(&graphql).await,
        None => false,
    };
    anyhow::ensure!(
        served,
        "start `gents server` for this home and retry: the author runs on a served home"
    );
    write::recover_output(&args.out, &home_dir)?;
    anyhow::ensure!(
        args.force || args.out.symlink_metadata().is_err(),
        "{} already exists; pass --force to replace it",
        args.out.display()
    );
    Ok(())
}

pub(crate) async fn run(
    ctx: &EvalContext,
    args: &EvalInitArgs,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    let gents::ConfigAccess::Graphql(graphql) = &ctx.access else {
        anyhow::bail!(
            "start `gents server` for this home and retry: the author runs on a served home"
        );
    };
    crate::request_helpers::ensure_local_request_signer(args.scope.home.as_deref(), &ctx.owner)?;
    // Held to the end: a materialized cache entry stays locked while read.
    let subject = crate::commands::pack::resolve_subject_pack(
        &ctx.home_dir,
        &args.subject,
        args.registry.as_deref(),
    )
    .await?;
    let subject_dir = subject.directory().to_path_buf();
    let author = resolve_pack_source(&args.author, args.registry.as_deref(), &ctx.home_dir).await?;
    let (author_slot, author_agent) = single_slot_agent(author.manifest())?;
    write::refuse_subject_overlap(&args.out, &subject_dir)?;
    let dossier = dossier::render(&subject_dir, args.agent.as_deref())?;
    writeln!(
        out,
        "drafting an eval of agent {} of pack {} {} ({})",
        dossier.agent_id, dossier.pack_name, dossier.pack_version, dossier.pack_digest
    )?;
    let profile = args.profile.clone().unwrap_or_else(|| {
        gents::default_inference_profile_id_for_agent(&gents::default_agent_id_for_node(&ctx.owner))
    });
    install_pack_slot(&ctx.access, &ctx.owner, &author, &author_slot, &profile).await?;
    readiness::wait_for_author(
        &ctx.access,
        &ctx.owner,
        &author_agent,
        std::time::Duration::from_secs(args.timeout_secs),
        std::time::Duration::from_secs(args.poll_secs).max(std::time::Duration::from_millis(1)),
    )
    .await?;

    let init = InitContext {
        dossier,
        registry: deps.registry,
        floors: Floors {
            validation_min: args.validation_min,
        },
        definition_id: args.definition_id.clone(),
        owner: ctx.owner.clone(),
        out: args.out.clone(),
        force: args.force,
        subject: args.subject.clone(),
        subject_dir,
        profile,
        scope: args.scope.clone(),
    };
    let mut turn = turn::LiveTurn {
        graphql: graphql.clone(),
        node_did: ctx.owner.clone(),
        agent_id: author_agent.clone(),
        session_id: uuid::Uuid::new_v4().to_string(),
        timeout_secs: args.timeout_secs,
        poll_secs: args.poll_secs,
        quiet: false,
    };
    let mut lines = operator_lines(|line| std::io::stdin().read_line(line));
    let result = async {
        let outcome = interview(&mut turn, &mut lines, &init, out).await?;
        if outcome.written.is_none() {
            writeln!(out, "the interview ended; nothing was written")?;
            return Ok(());
        }
        writeln!(out, "the draft validated in round {}", outcome.rounds)?;
        if !args.pilot {
            return Ok(());
        }
        let count = outcome
            .assembled
            .as_ref()
            .map_or(0, |assembled| assembled.definition.cases.len());
        let question = format!("Pilot {count} cases, one trial each, on {}?", init.profile);
        let approved = args.yes || crate::interactive_backend::confirm(&question, true).await;
        let deps = Deps {
            executor: deps.executor,
            registry: deps.registry,
            cancel: super::cancel_on_ctrl_c(
                "interrupt: the pilot stops launching; the pack stays as written",
            ),
            options: deps.options.clone(),
        };
        let piloted = pilot::pilot(
            ctx,
            &deps,
            &init,
            &mut turn,
            &outcome,
            &mut |_| approved,
            out,
        )
        .await?;
        writeln!(
            out,
            "piloted in {}{}",
            piloted.run_ids.join(", "),
            if piloted.revised {
                "; the author revised the pack from the evidence"
            } else {
                "; the pack stays as written"
            }
        )?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    writeln!(
        out,
        "session {id}: continue it with `gents chat --session-id {id} --agent-id {}`",
        author_agent,
        id = turn.session_id()
    )?;
    result
}

pub(crate) async fn install_pack_slot(
    access: &gents::ConfigAccess,
    owner: &str,
    pack: &PackSource,
    slot: &str,
    profile: &str,
) -> Result<gents::store::config_client::DesiredStateApplyCounts> {
    let manifest = pack.manifest();
    let config = pack_config(pack, owner)?;
    let requested = std::collections::BTreeMap::from([(slot.to_owned(), profile.to_owned())]);
    anyhow::ensure!(manifest.metadata.dependencies.is_empty(),
        "eval author pack dependencies must be installed through `gents pack install` on the runtime host");
    let prepared = gents::pack::prepare_document_pack_install(
        access,
        owner,
        manifest,
        &config,
        &requested,
        &[],
        gents::pack::InstalledVia::Operator,
        None,
        Some(pack.archive()),
        &|_| None,
    )
    .await?;
    let bound = prepared.config;
    let identity = gents::pack::PackIdentity::new(manifest, pack.digest(), Vec::new());
    gents::pack::install_pack_documents(
        access,
        owner,
        &identity,
        &bound,
        gents::pack::DriftPolicy::Overwrite,
    )
    .await
    .map(|report| report.applied)
    .with_context(|| format!("installing the {} pack", manifest.name))
}

pub(crate) fn pack_config(pack: &PackSource, owner: &str) -> Result<gents::documents::PackConfig> {
    gents::pack::load_pack_config(
        pack.manifest(),
        &gents::pack::PackInstallOptions {
            node_did: owner.to_owned(),
        },
        &|path| pack.asset(path).map(Vec::from),
        &|_name| None,
    )
    .with_context(|| format!("loading the {} pack", pack.manifest().name))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::turn::ScriptedTurn;
    use super::validate::tests::{dossier, good, OWNER};
    use super::*;

    pub(crate) fn draft_block(cases: &[serde_json::Value]) -> String {
        format!(
            "```json\n{}\n```",
            json!({
                "definition": {"definition_id": "canary-quality", "title": "Canary quality"},
                "cases": cases,
            })
        )
    }

    fn bad_block() -> String {
        let mut cases = good().cases;
        cases[0]["stages"][0]["checks"][0]["check"] = json!("no_such");
        draft_block(&cases)
    }

    fn context<'a>(registry: &'a CheckRegistry, out: PathBuf) -> InitContext<'a> {
        let mut subject = dossier();
        subject.text = "# Subject\n\n## Identity\n\n- pack: eval_canary 1.0.0\n".into();
        InitContext {
            dossier: subject,
            registry,
            floors: Floors { validation_min: 1 },
            definition_id: None,
            owner: OWNER.into(),
            out,
            force: false,
            subject: "./canary".into(),
            subject_dir: PathBuf::from("./canary"),
            profile: "local".into(),
            scope: crate::cli::EvalScopeArgs::default(),
        }
    }

    fn lines(lines: &[&str]) -> std::vec::IntoIter<std::io::Result<String>> {
        lines
            .iter()
            .map(|line| Ok((*line).to_owned()))
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[tokio::test]
    async fn runtime_captures_and_long_piped_answers_survive_pack_round_trip() {
        let registry = CheckRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        let ctx = context(&registry, root.path().join("out"));
        let mut cases = good().cases;
        for case in &mut cases {
            case["stages"][0]["capture"] = json!([{
                "kind": "documents", "name": "items", "collection": "AgentRequest",
                "filter": {"node_did": {"_eq": OWNER}},
                "fields": ["_docID", "request_id", "lifecycle_state"]
            }]);
        }
        let answer = "x".repeat(8192);
        let input = format!("{answer}\n");
        let mut input = input.as_bytes();
        let mut lines = operator_lines(|line| std::io::BufRead::read_line(&mut input, line));
        let mut turn =
            ScriptedTurn::new(["What must it get right?".to_owned(), draft_block(&cases)]);
        let outcome = interview(&mut turn, &mut lines, &ctx, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(outcome.summary, answer);
        assert_eq!(turn.sent[1], answer);
        assert_eq!(outcome.rounds, 1);
        let definition = outcome.assembled.unwrap().definition;
        assert!(definition.cases.iter().all(|case| {
            matches!(&case.stages[0].capture[0], gents::documents::EvalCapture::Documents {
                collection, fields, ..
            } if collection == "AgentRequest" && fields.iter().any(|field| field == "request_id"))
        }));
        assert!(outcome.written.unwrap().out.join("manifest.json").is_file());
    }

    #[tokio::test]
    async fn a_served_home_accepts_piped_interview_input() {
        let root = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let graphql = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new().route(
            "/",
            axum::routing::post(|| async { axum::Json(json!({"data": {"__typename": "Query"}})) }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let out = root.path().join("out").display().to_string();
        let crate::cli::EvalCommand::Init(args) = crate::commands::eval::testing::eval_command(&[
            "init",
            "./subject",
            "--out",
            &out,
            "--graphql",
            &graphql,
        ]) else {
            panic!("not init")
        };
        preflight(&args).await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn three_bad_drafts_end_with_nothing_written() {
        let registry = CheckRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        let ctx = context(&registry, root.path().join("out"));
        let mut turn = ScriptedTurn::new([bad_block(), bad_block(), bad_block()]);
        let error = interview(&mut turn, &mut lines(&[]), &ctx, &mut Vec::new())
            .await
            .err()
            .expect("three failed drafts are an error");
        let message = format!("{error:#}");
        assert!(message.contains("after 3 rounds"), "{message}");
        assert!(message.contains("unknown check"), "{message}");
        assert_eq!(turn.sent.len(), 3);
        assert!(!ctx.out.exists());
    }

    #[tokio::test]
    async fn the_author_installs_on_the_named_profile_and_again_changes_nothing() {
        let fixture = crate::commands::eval::testing::Fixture::new().await;
        let access = &fixture.ctx.access;
        let owner = fixture.ctx.owner.as_str();
        let read = |ids: [(gents::Collection, &'static str); 3]| async move {
            let mut documents = Vec::new();
            for (collection, id) in ids {
                let found = access
                    .transact_write("cli.eval.init.test_read_author", |txn| {
                        Box::pin(async move {
                            gents::store::config_client::read_desired_state_record_in_txn(
                                txn, collection, owner, id,
                            )
                            .await
                        })
                    })
                    .await
                    .unwrap()
                    .unwrap_or_else(|| panic!("{} {id} installed", collection.graphql_type()));
                documents.push(found);
            }
            documents
        };
        let installed = || {
            read([
                (gents::Collection::Agent, "fixture-author"),
                (gents::Collection::AgentContext, "fixture-author-context"),
                (gents::Collection::Tools, "fixture-author-tools"),
            ])
        };
        let author = crate::commands::pack::test_support::fixture_pack_source(
            "slot_fixture",
            &fixture.ctx.home_dir,
        );
        let (slot, agent_id) = single_slot_agent(author.manifest()).unwrap();
        assert_eq!(
            (slot.as_str(), agent_id.as_str()),
            ("author", "fixture-author")
        );
        let first_counts = install_pack_slot(access, owner, &author, &slot, "local")
            .await
            .unwrap();
        assert_eq!(first_counts.get(gents::Collection::Agent), 1);
        let first = installed().await;
        install_pack_slot(access, owner, &author, &slot, "local")
            .await
            .unwrap();
        assert_eq!(installed().await, first);
        let agent = &first[0].1;
        assert_eq!(agent["inference_profile_id"], "local");

        let error = install_pack_slot(access, owner, &author, &slot, "no-such-profile")
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("no-such-profile"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn an_existing_out_is_refused_before_anything_else() {
        let root = tempfile::tempdir().unwrap();
        let out = root.path().display().to_string();
        let crate::cli::EvalCommand::Init(args) =
            crate::commands::eval::testing::eval_command(&["init", "./subject", "--out", &out])
        else {
            panic!("not init");
        };
        let error = preflight(&args).await.unwrap_err().to_string();
        assert!(error.contains("--force"), "{error}");
    }

    const SMOKE_SUBJECT: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../gents/tests/fixtures/packs/documents_fixture"
    );
    const SMOKE_BEHAVIOR: &str = "fixture-worker";

    #[test]
    fn the_live_smokes_subject_shows_collections_to_capture() {
        let dossier =
            dossier::render(std::path::Path::new(SMOKE_SUBJECT), Some(SMOKE_BEHAVIOR)).unwrap();
        assert!(!dossier.collections.is_empty(), "{}", dossier.text);
    }

    /// A live smoke test: the author runs as a real request on a served
    /// home, `gents eval init <subject> --agent fixture-worker`-style, the
    /// operator answering every question with "draft". Needs `GENTS_EVAL_INIT_HOME`
    /// pointing at a home `gents server` already serves, with a backend the
    /// resolved profile can reach. It asserts only that a pack was written
    /// and validated; everything else about the draft is the author's.
    #[tokio::test]
    #[ignore = "needs a served home with GENTS_EVAL_INIT_HOME and a real backend"]
    async fn a_live_author_drafts_a_pack_that_validates() {
        let home_dir = PathBuf::from(
            std::env::var("GENTS_EVAL_INIT_HOME")
                .expect("GENTS_EVAL_INIT_HOME names a home gents server already serves"),
        );
        let state = crate::home_state::read_runtime_state(&home_dir)
            .unwrap()
            .expect("a runtime state written by `gents server`");
        let graphql = crate::home_graphql_endpoint(&home_dir, state.graphql.clone());
        let access = gents::ConfigAccess::Graphql(graphql.clone());
        let owner = state.node_did.clone();

        let subject_dir = PathBuf::from(SMOKE_SUBJECT);
        let dossier = dossier::render(&subject_dir, Some(SMOKE_BEHAVIOR)).unwrap();
        let registry = CheckRegistry::builtin();
        let profile = gents::default_inference_profile_id_for_agent(
            &gents::default_agent_id_for_node(&owner),
        );
        let author = resolve_pack_source("gents/eval_author", None, &home_dir)
            .await
            .expect("gents/eval_author is in the home's store or the registry");
        let (slot, author_agent) = single_slot_agent(author.manifest()).unwrap();
        install_pack_slot(&access, &owner, &author, &slot, &profile)
            .await
            .unwrap();

        let root = tempfile::tempdir().unwrap();
        let ctx = InitContext {
            dossier,
            registry: &registry,
            floors: Floors { validation_min: 1 },
            definition_id: None,
            owner: owner.clone(),
            out: root.path().join("out"),
            force: false,
            subject: SMOKE_SUBJECT.into(),
            subject_dir,
            profile,
            scope: crate::cli::EvalScopeArgs {
                home: Some(home_dir.clone()),
                graphql: None,
            },
        };
        let mut turn = turn::LiveTurn {
            graphql,
            node_did: owner,
            agent_id: author_agent,
            session_id: uuid::Uuid::new_v4().to_string(),
            timeout_secs: 300,
            poll_secs: 1,
            quiet: false,
        };
        let mut lines = lines(&["draft", "draft", "draft"]);
        let outcome = interview(&mut turn, &mut lines, &ctx, &mut Vec::new())
            .await
            .unwrap();

        let written = outcome.written.expect("the author's draft validated");
        assert!(written.out.join("manifest.json").is_file());
        assert!(outcome.assembled.is_some());
    }
}
