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

/// Alternate the operator's `lines` with the author's replies on `turn`
/// until a draft validates and lands at `ctx.out`, the operator ends it
/// (`/quit`, `/exit`, end of input), or [`MAX_VALIDATION_ROUNDS`] drafts
/// failed (an error carrying the last messages; nothing written).
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
    use std::io::IsTerminal;
    anyhow::ensure!(
        args.force || args.out.symlink_metadata().is_err(),
        "{} already exists; pass --force to replace it",
        args.out.display()
    );
    anyhow::ensure!(
        std::io::stdin().is_terminal(),
        "gents eval init is an interview; run it in a terminal"
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
) -> Result<gents::config_client::DesiredStateApplyCounts> {
    let manifest = pack.manifest();
    let config = pack_config(pack, owner)?;
    let requested = std::collections::BTreeMap::from([(slot.to_owned(), profile.to_owned())]);
    let inference =
        gents::pack::preview_pack_inference_bindings(access, manifest, owner, &requested).await?;
    let bound = gents::pack::bind_pack_install_config(manifest, &config, &inference.bindings)?;
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

pub(crate) fn pack_config(
    pack: &PackSource,
    owner: &str,
) -> Result<gents::document_config::PackConfig> {
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

