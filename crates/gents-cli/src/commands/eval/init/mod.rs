//! `gents eval init`: an interview with a model that drafts an eval
//! definition pack for one behavior of a subject pack.
//!
//! The pure core: the subject dossier the author reads, the draft it
//! answers with, the validation that draft must pass, and the writer that
//! turns a validated draft into a definition pack on disk. Nothing reaches
//! `--out` before validation, including the loader round trip, passes.
//!
//! Around it, the command: the author runs as an ordinary request on the
//! operator's served home ([`turn::LiveTurn`]), [`interview`] alternates the
//! operator's lines with the author's replies until a draft validates.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use gents::eval::checks::CheckRegistry;

use self::contract::{first_turn, MAX_VALIDATION_ROUNDS, VALIDATION_PREFIX};
use self::dossier::Dossier;
use self::draft::parse_reply;
use self::turn::Turn;
use self::validate::{assemble, validate, Assembled, Floors};
use self::write::{case_table, commit, stage, Staged, Written};
use super::{Deps, EvalContext};
use crate::cli::EvalInitArgs;

mod contract;
mod dossier;
mod draft;
mod pilot;
mod turn;
mod validate;
mod write;

/// What one init settled before the interview: the subject, the catalog,
/// the floors and where the pack goes.
pub(crate) struct InitContext<'a> {
    /// The subject as the author reads it; also its identity (pack name,
    /// digest, behavior, slot) for assembly and the README.
    pub(crate) dossier: Dossier,
    pub(crate) registry: &'a CheckRegistry,
    pub(crate) floors: Floors,
    /// `--definition-id`, which overrides the draft's.
    pub(crate) definition_id: Option<String>,
    /// The home's identity: owner of the definition.
    pub(crate) owner: String,
    pub(crate) out: PathBuf,
    pub(crate) force: bool,
    /// The subject as the operator named it, for the printed next commands.
    pub(crate) subject: String,
    /// The subject's directory: the pilot's cell names it.
    pub(crate) subject_dir: PathBuf,
    /// The inference profile the author and the pilot run on.
    pub(crate) profile: String,
}

/// How an interview ended. The session is the turn's: an interview that
/// fails returns no outcome, and its session still names the transcript.
pub(crate) struct InitOutcome {
    /// The pack at `--out`; `None` when the operator ended the interview.
    pub(crate) written: Option<Written>,
    /// The definition that was written.
    pub(crate) assembled: Option<Assembled>,
    /// Every operator line, in order: the README's interview summary.
    pub(crate) summary: String,
    /// Drafts that entered validation.
    pub(crate) rounds: usize,
}

/// Alternate the operator's `lines` with the author's replies on `turn`
/// until a draft validates and lands at `ctx.out`, the operator ends it
/// (`/quit`, `/exit`, end of input), or [`MAX_VALIDATION_ROUNDS`] drafts
/// failed (an error carrying the last messages; nothing written).
pub(crate) async fn interview(
    turn: &mut dyn Turn,
    lines: &mut dyn Iterator<Item = String>,
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
            let Some(line) = next_line(lines) else {
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
        match checked(parsed, ctx, &summary, None).await {
            Ok((assembled, staged)) => {
                let written = commit(staged, &ctx.out, ctx.force)?;
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
                    return Err(anyhow!(
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

/// The operator's next non-blank line, trimmed; `None` at the end of input
/// or on `/quit` or `/exit`.
fn next_line(lines: &mut dyn Iterator<Item = String>) -> Option<String> {
    for line in lines {
        let line = line.trim();
        match line {
            "" => continue,
            "/quit" | "/exit" => return None,
            line => return Some(line.to_owned()),
        }
    }
    None
}

/// Validation steps 1 to 7 over a reply that carries a draft: parsed,
/// assembled, held to the contract, catalog and subject, then staged
/// through the loader round trip, its README recording `pilot`. Every
/// failure is messages for the author.
async fn checked(
    parsed: Result<Option<draft::Draft>, String>,
    ctx: &InitContext<'_>,
    summary: &str,
    pilot: Option<&write::PilotNote>,
) -> Result<(Assembled, Staged), Vec<String>> {
    let draft = match parsed {
        Ok(Some(draft)) => draft,
        Ok(None) => return Err(vec!["the reply carries no fenced json draft".to_owned()]),
        Err(message) => return Err(vec![message]),
    };
    let assembled = assemble(
        &draft,
        ctx.definition_id.as_deref(),
        &ctx.owner,
        &ctx.dossier.slot,
    )?;
    validate(&assembled, ctx.registry, &ctx.dossier, &ctx.floors)?;
    let staged = stage(&assembled, summary, &ctx.dossier, pilot).await?;
    Ok((assembled, staged))
}

/// The case table and the commands that install and run what was written.
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
    writeln!(
        out,
        "\nnext:\n  gents config apply --root {} --bind-agent-did home\n  gents eval run {} --cell baseline={}:{}",
        written.out.display(),
        definition.definition_id,
        ctx.subject,
        ctx.dossier.behavior_id
    )?;
    Ok(())
}

/// Refused before anything is read or installed: an existing `--out`
/// without `--force`, a non-terminal stdin (this command is an interview),
/// and a home no runtime serves (the author's requests need one to claim
/// them).
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
    Ok(())
}

/// `gents eval init`, on a home [`preflight`] found served.
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
        true,
    )
    .await?;
    let subject_dir = subject
        .directory()
        .ok_or_else(|| anyhow!("subject {} resolved to no directory", args.subject))?
        .to_path_buf();
    let dossier = dossier::render(&subject_dir, args.behavior.as_deref())?;
    writeln!(
        out,
        "drafting an eval of behavior {} of pack {} {} ({})",
        dossier.behavior_id, dossier.pack_name, dossier.pack_version, dossier.pack_digest
    )?;
    let profile = args.profile.clone().unwrap_or_else(|| {
        gents::default_inference_profile_id_for_behavior(&gents::default_behavior_id_for_agent(
            &ctx.owner,
        ))
    });
    install_author(&ctx.access, &ctx.owner, &profile).await?;

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
    };
    let mut turn = turn::LiveTurn {
        graphql: graphql.clone(),
        agent_did: ctx.owner.clone(),
        session_id: uuid::Uuid::new_v4().to_string(),
        timeout_secs: args.timeout_secs,
        poll_secs: args.poll_secs,
    };
    let mut lines = std::iter::from_fn(|| {
        eprint!("> ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    });
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
        // Only now does Ctrl-C stop launching rather than end the process.
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
    // Whatever the outcome, the session is the author's transcript.
    writeln!(
        out,
        "session {id}: continue it with `gents chat --session-id {id} --behavior-id {}`",
        turn::AUTHOR_BEHAVIOR,
        id = turn.session_id()
    )?;
    result
}

/// Install the built-in `eval_author` pack into the home, its `author` slot
/// bound to `profile`, through the owner `gents pack install` uses.
/// Re-applying the same documents changes nothing.
async fn install_author(access: &gents::ConfigAccess, owner: &str, profile: &str) -> Result<()> {
    use anyhow::Context as _;
    let pack = gents::pack::resolve_pack("eval_author")?;
    let config = gents::pack::load_pack_config(
        &pack.manifest,
        &gents::pack::PackInstallOptions {
            agent_did: owner.to_owned(),
        },
        &|path| pack.asset(path).map(Vec::from),
        &|_name| None,
    )
    .context("loading the eval_author pack")?;
    let requested = std::collections::BTreeMap::from([("author".to_owned(), profile.to_owned())]);
    let inference =
        gents::pack::preview_pack_inference_bindings(access, &pack.manifest, owner, &requested)
            .await?;
    let bound =
        gents::pack::bind_pack_install_config(&pack.manifest, &config, &inference.bindings)?;
    gents::pack::install_pack_documents(access, &bound)
        .await
        .context("installing the eval_author pack")?;
    Ok(())
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

    fn good_block() -> String {
        draft_block(&good().cases)
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
        }
    }

    fn lines(lines: &[&str]) -> std::vec::IntoIter<String> {
        lines
            .iter()
            .map(|line| (*line).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[tokio::test]
    async fn an_interview_ends_when_the_author_drafts_and_the_pack_is_written() {
        let registry = CheckRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        let ctx = context(&registry, root.path().join("out"));
        let mut turn = ScriptedTurn::new(["What must it get right?".to_owned(), good_block()]);
        let mut out = Vec::new();
        let outcome = interview(
            &mut turn,
            &mut lines(&["It must count items."]),
            &ctx,
            &mut out,
        )
        .await
        .unwrap();
        let printed = String::from_utf8(out).unwrap();

        let written = outcome.written.expect("the pack was written");
        assert_eq!(written.out, ctx.out);
        assert!(ctx.out.join("manifest.json").is_file());
        assert_eq!(outcome.rounds, 1);
        assert_eq!(outcome.summary, "It must count items.");
        assert_eq!(
            outcome.assembled.unwrap().definition.definition_id,
            "canary-quality"
        );
        assert!(turn.sent[0].starts_with("# Subject"), "{}", turn.sent[0]);
        assert!(turn.sent[0].contains("# Check catalog"), "{}", turn.sent[0]);
        assert!(!turn.sent[0].contains("## How to reply with a draft"));
        assert_eq!(turn.sent[1], "It must count items.");
        assert_eq!(turn.sent.len(), 2);

        assert!(printed.contains("What must it get right?"), "{printed}");
        assert!(
            printed.contains("| val-a | validation | 1 | captured_rows_count |"),
            "{printed}"
        );
        assert!(printed.contains("gents config apply --root"), "{printed}");
        assert!(
            printed.contains("gents eval run canary-quality"),
            "{printed}"
        );
    }

    #[tokio::test]
    async fn a_bad_draft_goes_back_with_the_messages_and_a_good_one_follows() {
        let registry = CheckRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        let ctx = context(&registry, root.path().join("out"));
        let mut turn = ScriptedTurn::new([bad_block(), good_block()]);
        let mut out = Vec::new();
        let outcome = interview(&mut turn, &mut lines(&[]), &ctx, &mut out)
            .await
            .unwrap();
        assert!(outcome.written.is_some());
        assert_eq!(outcome.rounds, 2);
        assert!(
            turn.sent[1].starts_with(VALIDATION_PREFIX),
            "{}",
            turn.sent[1]
        );
        assert!(turn.sent[1].contains("unknown check"), "{}", turn.sent[1]);
    }

    #[tokio::test]
    async fn a_draft_that_fails_the_round_trip_is_a_validation_round() {
        let registry = CheckRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        let ctx = context(&registry, root.path().join("out"));
        // Steps 1 to 6 pass; the loader refuses the id as a pack name.
        let renamed = format!(
            "```json\n{}\n```",
            json!({
                "definition": {"definition_id": "Canary.Quality"},
                "cases": good().cases,
            })
        );
        let mut turn = ScriptedTurn::new([renamed, good_block()]);
        let outcome = interview(&mut turn, &mut lines(&[]), &ctx, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(outcome.rounds, 2);
        assert!(
            turn.sent[1].starts_with(VALIDATION_PREFIX),
            "{}",
            turn.sent[1]
        );
        assert!(turn.sent[1].contains("Canary.Quality"), "{}", turn.sent[1]);
        assert!(outcome.written.is_some());
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
        // The first turn and two returned drafts; the third is not sent back.
        assert_eq!(turn.sent.len(), 3);
        assert!(!ctx.out.exists());
    }

    #[tokio::test]
    async fn the_author_installs_on_the_named_profile_and_again_changes_nothing() {
        let fixture = crate::commands::eval::testing::Fixture::new().await;
        let access = &fixture.ctx.access;
        let owner = fixture.ctx.owner.as_str();
        install_author(access, owner, "local").await.unwrap();
        install_author(access, owner, "local").await.unwrap();
        let (_, behavior) = access
            .transact("cli.eval.init.test_read_author", |txn| {
                Box::pin(async move {
                    gents::config_client::read_desired_state_record_in_txn(
                        txn,
                        gents::Collection::AgentBehavior,
                        owner,
                        turn::AUTHOR_BEHAVIOR,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .expect("the author behavior installed");
        assert_eq!(behavior["inference_profile_id"], "local");

        let error = install_author(access, owner, "no-such-profile")
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

    #[tokio::test]
    async fn the_operator_can_end_the_interview() {
        let registry = CheckRegistry::builtin();
        let root = tempfile::tempdir().unwrap();
        let ctx = context(&registry, root.path().join("out"));
        let mut turn = ScriptedTurn::new(["What must it get right?"]);
        let mut out = Vec::new();
        let outcome = interview(&mut turn, &mut lines(&["", "/quit"]), &ctx, &mut out)
            .await
            .unwrap();
        assert!(outcome.written.is_none());
        assert!(outcome.assembled.is_none());
        assert_eq!(outcome.rounds, 0);
        assert_eq!(turn.sent.len(), 1);
        assert!(!ctx.out.exists());
    }
}
