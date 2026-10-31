use clap::Parser;

use gents::eval::checks::CheckRegistry;
use tokio_util::sync::CancellationToken;

use super::testing::{
    accepted_job, delete_definition, optimization, optimization_command, optimization_with,
    proposer_file,
};
use super::{
    ensure_tool_less, execute, pack_config, proposer_agent_id, subject_preamble, JobTarget,
};
use crate::cli::Cli;
use crate::commands::eval::testing::{deps, eval, executor, Fixture, DEFINITION};
use crate::commands::eval::UNCALIBRATED_BANNER;

#[tokio::test]
async fn run_refuses_without_a_proposer_and_an_unknown_proposer_is_a_usage_error() {
    let fixture = Fixture::new().await;
    let pack = fixture.pack_arg();
    let error = optimization(&fixture, &["run", DEFINITION, "--subject", pack.as_str()])
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "optimization run needs a proposer: pass --proposer scripted:<file> or --proposer agent:<pack>[:<agent>]"
    );
    let usage = match Cli::try_parse_from([
        "gents",
        "optimization",
        "run",
        DEFINITION,
        "--subject",
        "monitor",
        "--proposer",
        "llm",
    ]) {
        Ok(_) => panic!("an unknown proposer must not parse"),
        Err(error) => error,
    };
    assert!(
        usage
            .to_string()
            .contains("pass --proposer scripted:<file> or --proposer agent:<pack>[:<agent>]"),
        "{usage}"
    );
    assert_eq!(usage.exit_code(), 2);
}

#[test]
fn a_slot_with_several_behaviors_needs_the_behavior_named() {
    let slot = gents::pack::PackInferenceSlot {
        name: "proposer".to_owned(),
        description: String::new(),
        agents: vec!["terse".to_owned(), "verbose".to_owned()],
        optional: false,
    };
    let error = proposer_agent_id("p", &slot, None).unwrap_err();
    assert_eq!(
        error.to_string(),
        "pack p declares 2 inference-slot agents; pass --proposer agent:p:<agent> with one of [\"terse\", \"verbose\"]"
    );
    assert_eq!(
        proposer_agent_id("p", &slot, Some("verbose")).unwrap(),
        "verbose"
    );
    let unknown = proposer_agent_id("p", &slot, Some("other")).unwrap_err();
    assert!(
        unknown.to_string().contains("no inference-slot agent"),
        "{unknown}"
    );
    let one = gents::pack::PackInferenceSlot {
        agents: vec!["terse".to_owned()],
        optional: false,
        ..slot
    };
    assert_eq!(proposer_agent_id("p", &one, None).unwrap(), "terse");
}

#[test]
fn the_subject_preamble_is_the_dossier_naming_the_subject_tools() {
    let pipeline = crate::commands::pack::test_support::fixture_dir("documents_fixture");
    let preamble = subject_preamble(&pipeline, "fixture-worker", &JobTarget::Context).unwrap();
    assert!(
        preamble.starts_with("# Subject\n\n## Identity"),
        "{preamble}"
    );
    assert_eq!(preamble.matches("# Subject").count(), 1, "{preamble}");
    assert!(preamble.contains("type FixtureJob"), "{preamble}");
    assert!(
        preamble.ends_with(
            "The instruction you will rewrite is this agent's system prompt. \
             Every later turn carries the current instruction and the training feedback."
        ),
        "{preamble}"
    );
    let task =
        subject_preamble(&pipeline, "fixture-worker", &JobTarget::Task("plan".into())).unwrap();
    assert!(
        task.contains("the prompt template of its task \"plan\""),
        "{task}"
    );
    assert!(task.contains("every {{ variable }}"), "{task}");
    assert!(
        task.contains("use no {{ variable }} the current template does not already use"),
        "{task}"
    );
    assert!(
        task.contains("node.node_did, node.agent_id, ctx.now"),
        "{task}"
    );
}

#[tokio::test]
async fn a_proposer_behavior_with_tools_is_refused_before_anything_runs() {
    let fixture = Fixture::new().await;
    let pack = fixture.pack_arg();
    let proposer = format!(
        "agent:{}",
        crate::commands::pack::test_support::fixture_dir("documents_fixture").display()
    );
    let error = optimization(
        &fixture,
        &[
            "run",
            DEFINITION,
            "--subject",
            pack.as_str(),
            "--proposer",
            proposer.as_str(),
        ],
    )
    .await
    .unwrap_err();
    let message = error.to_string();
    assert!(
        message.starts_with("proposer agent fixture-worker has tools [")
            && message.ends_with("]; a proposer must be tool-less"),
        "{error:#}"
    );
    assert!(message.contains("\"read_file\""), "{message}");
}

#[test]
fn cli_tools_a_target_outside_the_pack_or_skills_are_refused() {
    use serde_json::json;
    const OWNER: &str = "did:key:z6MkproposerOwner";
    let home = tempfile::tempdir().unwrap();
    let pack =
        crate::commands::pack::test_support::fixture_pack_source("slot_fixture", home.path());
    let base = pack_config(&pack, OWNER).unwrap();
    ensure_tool_less(&base, OWNER, "fixture-author").unwrap();

    let mut cli = base.clone();
    cli.tools[0]
        .host
        .as_mut()
        .unwrap()
        .cli
        .push(serde_json::from_value(json!({"name": "rg"})).unwrap());
    let mut delegated = base.clone();
    delegated.tools[0].agents =
        Some(serde_json::from_value(json!({"enabled": true, "target_ids": ["helper"]})).unwrap());
    delegated.agent_targets.push(
        serde_json::from_value(json!({
            "target_id": "helper",
            "node_did": OWNER,
            "target_node_did": OWNER,
            "agent_id": "home-default",
            "name": "helper",
        }))
        .unwrap(),
    );
    let mut sessions = base.clone();
    sessions.tools[0]
        .built_ins
        .as_mut()
        .unwrap()
        .enable_session_history_tool = Some(true);
    let mut skills = base.clone();
    skills.contexts[0].skill_ids = vec!["review".to_owned()];
    for (config, offending) in [
        (cli, "\"rg\""),
        (delegated, "\"agent_new\""),
        (sessions, "\"sessions\""),
        (skills, "has skills [\"review\"]"),
    ] {
        let error = ensure_tool_less(&config, OWNER, "fixture-author")
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("proposer agent fixture-author has ")
                && error.contains(offending)
                && error.ends_with("; a proposer must be tool-less"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn a_behavior_proposer_needs_a_served_home() {
    let fixture = Fixture::new().await;
    let pack = fixture.pack_arg();
    let proposer = format!(
        "agent:{}",
        crate::commands::pack::test_support::fixture_dir("slot_fixture").display()
    );
    let error = optimization(
        &fixture,
        &[
            "run",
            DEFINITION,
            "--subject",
            pack.as_str(),
            "--proposer",
            proposer.as_str(),
        ],
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("start `gents server`"),
        "{error:#}"
    );
}

#[tokio::test]
async fn a_proposer_pack_missing_offline_fails_in_one_sentence() {
    let fixture = Fixture::new().await;
    let pack = fixture.pack_arg();
    let error = optimization(
        &fixture,
        &[
            "run",
            DEFINITION,
            "--subject",
            pack.as_str(),
            "--proposer",
            "agent:absent_proposer",
            "--registry",
            "http://127.0.0.1:9",
        ],
    )
    .await
    .unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.starts_with("gents/absent_proposer is not in the pack store of ")
            && message.contains("could not be reached")
            && !message.contains('\n'),
        "{message}"
    );
}

#[tokio::test]
async fn a_refused_job_is_refused_before_the_proposer_is_built() {
    let fixture = Fixture::new().await;
    let pack = fixture.pack_arg();
    let proposer = format!(
        "agent:{}",
        crate::commands::pack::test_support::fixture_dir("slot_fixture").display()
    );
    std::fs::create_dir_all(&fixture.ctx.home_dir).unwrap();
    let policy = fixture.ctx.home_dir.join("policy.json");
    std::fs::write(
        &policy,
        serde_json::to_vec(&gents::optimization::PolicyV2::uncalibrated()).unwrap(),
    )
    .unwrap();
    let policy = policy.display().to_string();
    for (flag, value, refusal) in [
        (
            "--job-id",
            "a/b",
            r#"job_id "a/b" must be one ordinary path component"#,
        ),
        (
            "--policy",
            policy.as_str(),
            "policy max_rounds 3 does not match the budget's max_rounds 2; the Bonferroni divisor must be the number of candidates the job may try",
        ),
    ] {
        let error = optimization(
            &fixture,
            &[
                "run",
                DEFINITION,
                "--subject",
                pack.as_str(),
                "--proposer",
                proposer.as_str(),
                "--rounds",
                "2",
                flag,
                value,
            ],
        )
        .await
        .unwrap_err();
        assert_eq!(format!("{error:#}"), refusal);
    }
}

#[tokio::test]
async fn a_scripted_job_runs_to_ready_to_promote_and_show_recomputes_its_decisions() {
    let fixture = Fixture::new().await;
    let output = accepted_job(&fixture, "job-1").await;
    assert!(output.lines().any(|line| line == "frozen"), "{output}");
    assert!(output.contains("finalized ready_to_promote"), "{output}");
    assert!(
        output.contains("job job-1 state ready_to_promote"),
        "{output}"
    );

    assert!(output.contains(UNCALIBRATED_BANNER), "{output}");

    let shown = optimization(&fixture, &["show", "job-1"]).await.unwrap();
    assert_eq!(shown.lines().next(), Some(UNCALIBRATED_BANNER), "{shown}");
    assert!(shown.contains("state ready_to_promote"), "{shown}");
    assert!(shown.contains("checkpoint round 1 pack "), "{shown}");
    let accepted = shown
        .lines()
        .find(|line| line.contains("journaled accept"))
        .unwrap_or_else(|| panic!("{shown}"));
    assert!(accepted.contains("recomputed accept"), "{accepted}");
    assert!(!shown.contains("MISMATCH"), "{shown}");

    let json = optimization(&fixture, &["show", "job-1", "--json"])
        .await
        .unwrap();
    assert!(!json.contains(UNCALIBRATED_BANNER), "{json}");
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(json["state"]["state"], "ready_to_promote");
    assert_eq!(json["job"]["job_id"], "job-1");

    delete_definition(&fixture).await;
    let shown = optimization(&fixture, &["show", "job-1"]).await.unwrap();
    assert!(
        shown.contains("the eval definition changed since the job froze it"),
        "{shown}"
    );
    let accepted = shown
        .lines()
        .find(|line| line.contains("journaled accept"))
        .unwrap_or_else(|| panic!("{shown}"));
    assert!(
        accepted.contains("recomputed not recomputable: definition changed or runs invalidated"),
        "{accepted}"
    );
    assert!(!shown.contains("MISMATCH"), "{shown}");
}

#[tokio::test]
async fn policy_defaults_is_sized_to_rounds_and_a_policy_file_is_taken_as_written() {
    use crate::commands::eval::testing::VALIDATION_CASES;

    let fixture = Fixture::new().await;
    let pack = fixture.pack_arg();
    let proposer = proposer_file(&fixture);
    let scripted = executor(&VALIDATION_CASES);
    let registry = CheckRegistry::builtin();
    fn argv<'a>(
        pack: &'a str,
        proposer: &'a str,
        job_id: &'a str,
        policy: &'a str,
    ) -> Vec<&'a str> {
        vec![
            "run",
            DEFINITION,
            "--subject",
            pack,
            "--profile",
            "local",
            "--proposer",
            proposer,
            "--rounds",
            "2",
            "--policy",
            policy,
            "--job-id",
            job_id,
        ]
    }
    let output = optimization_with(
        &fixture,
        &argv(&pack, &proposer, "defaults-2", "defaults"),
        &deps(&scripted, &registry, CancellationToken::new()),
    )
    .await
    .unwrap();
    assert!(output.contains("finalized "), "{output}");
    assert!(
        output.contains("job defaults-2 state ready_to_promote"),
        "{output}"
    );
    assert!(
        output.contains(UNCALIBRATED_BANNER),
        "the defaults sized to --rounds are still uncalibrated: {output}"
    );

    let path = fixture.ctx.home_dir.join("policy.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&gents::optimization::PolicyV2::uncalibrated()).unwrap(),
    )
    .unwrap();
    let path = path.display().to_string();
    let error = optimization_with(
        &fixture,
        &argv(&pack, &proposer, "file-2", &path),
        &deps(&scripted, &registry, CancellationToken::new()),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "policy max_rounds 3 does not match the budget's max_rounds 2; the Bonferroni divisor must be the number of candidates the job may try"
    );
    assert_eq!(format!("{error:#}"), error.to_string());

    std::fs::write(
        &path,
        serde_json::to_vec(&gents::optimization::PolicyV2 {
            max_rounds: 2,
            alpha_ppm: 40_000,
            ..gents::optimization::PolicyV2::uncalibrated()
        })
        .unwrap(),
    )
    .unwrap();
    let output = optimization_with(
        &fixture,
        &argv(&pack, &proposer, "calibrated-2", &path),
        &deps(&scripted, &registry, CancellationToken::new()),
    )
    .await
    .unwrap();
    assert!(output.contains("job calibrated-2 state "), "{output}");
    assert!(!output.contains(UNCALIBRATED_BANNER), "{output}");
}

#[tokio::test]
async fn a_job_id_that_is_not_one_path_component_is_refused_verbatim() {
    let fixture = Fixture::new().await;
    let pack = fixture.pack_arg();
    let proposer = super::testing::proposer_file(&fixture);
    let error = optimization(
        &fixture,
        &[
            "run",
            DEFINITION,
            "--subject",
            pack.as_str(),
            "--profile",
            "local",
            "--proposer",
            proposer.as_str(),
            "--job-id",
            "a/b",
        ],
    )
    .await
    .unwrap_err();
    let refusal = r#"job_id "a/b" must be one ordinary path component"#;
    assert_eq!(error.to_string(), refusal);
    assert_eq!(
        format!("{error:#}"),
        refusal,
        "a refusal carries no context"
    );
    let wrapped = anyhow::Error::from(gents::optimization::JobRefused(refusal.to_owned()))
        .context("driving the job");
    assert_eq!(format!("{:#}", super::surface_refusal(wrapped)), refusal);
}

#[tokio::test]
async fn an_interrupted_job_says_how_to_resume_and_the_same_command_finishes_it() {
    use crate::commands::eval::testing::VALIDATION_CASES;

    let fixture = Fixture::new().await;
    let pack = fixture.pack_arg();
    let proposer = proposer_file(&fixture);
    let registry = CheckRegistry::builtin();
    let argv = |job_id: &'static str, json: bool| {
        let mut argv = vec![
            "run".to_owned(),
            DEFINITION.to_owned(),
            "--subject".to_owned(),
            pack.clone(),
            "--profile".to_owned(),
            "local".to_owned(),
            "--proposer".to_owned(),
            proposer.clone(),
            "--job-id".to_owned(),
            job_id.to_owned(),
        ];
        if json {
            argv.push("--json".to_owned());
        }
        argv
    };
    let interrupted = || {
        let token = CancellationToken::new();
        token.cancel();
        token
    };
    fn refs(argv: &[String]) -> Vec<&str> {
        argv.iter().map(String::as_str).collect()
    }

    let stopped = executor(&[]);
    let mut out = Vec::new();
    let error = execute(
        &fixture.ctx,
        optimization_command(&refs(&argv("job-stop", false))),
        &deps(&stopped, &registry, interrupted()),
        &mut out,
    )
    .await
    .unwrap_err();
    assert_eq!(
        format!("{error:#}"),
        "job job-stop stopped before it finished; run the same command with --job-id job-stop to resume it"
    );
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("job job-stop state running "), "{output}");
    assert!(!output.contains("stopped before it finished"), "{output}");
    let shown = optimization(&fixture, &["show", "job-stop"]).await.unwrap();
    assert!(shown.contains("job job-stop state running "), "{shown}");

    let scripted = executor(&VALIDATION_CASES);
    let output = optimization_with(
        &fixture,
        &refs(&argv("job-stop", false)),
        &deps(&scripted, &registry, CancellationToken::new()),
    )
    .await
    .unwrap();
    assert!(
        output.contains("job job-stop state ready_to_promote"),
        "{output}"
    );
    assert!(!output.contains("stopped before it finished"), "{output}");
    let journal =
        gents::optimization::load_job(&fixture.ctx.access, &fixture.ctx.owner, "job-stop")
            .await
            .unwrap()
            .expect("the job")
            .journal;
    assert_eq!(
        journal
            .iter()
            .filter(|entry| matches!(entry, gents::optimization::JournalEntry::Frozen))
            .count(),
        1,
        "the resume froze nothing again: {journal:?}"
    );

    let mut out = Vec::new();
    let error = execute(
        &fixture.ctx,
        optimization_command(&refs(&argv("job-json", true))),
        &deps(&stopped, &registry, interrupted()),
        &mut out,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("job job-json stopped before it finished"),
        "{error:#}"
    );
    let output = String::from_utf8(out).unwrap();
    let json: serde_json::Value = serde_json::from_str(&output)
        .unwrap_or_else(|error| panic!("stdout is not pure JSON ({error}): {output}"));
    assert_eq!(json["state"]["state"], "running");
    assert_eq!(json["job"]["job_id"], "job-json");
}

#[tokio::test]
async fn a_failure_before_the_freeze_does_not_say_resume() {
    let fixture = Fixture::new().await;
    let subject = format!("{}:no-such-agent", fixture.pack_arg());
    let proposer = proposer_file(&fixture);
    let error = optimization(
        &fixture,
        &[
            "run",
            DEFINITION,
            "--subject",
            subject.as_str(),
            "--profile",
            "local",
            "--proposer",
            proposer.as_str(),
            "--job-id",
            "pre-freeze",
        ],
    )
    .await
    .unwrap_err();
    assert!(!format!("{error:#}").contains("resume"), "{error:#}");
    assert!(
        gents::optimization::load_job(&fixture.ctx.access, &fixture.ctx.owner, "pre-freeze")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_script_with_fewer_proposals_than_rounds_is_refused_before_the_freeze() {
    let fixture = Fixture::new().await;
    let pack = fixture.pack_arg();
    let proposer = proposer_file(&fixture);
    let error = optimization(
        &fixture,
        &[
            "run",
            DEFINITION,
            "--subject",
            pack.as_str(),
            "--profile",
            "local",
            "--proposer",
            proposer.as_str(),
            "--rounds",
            "4",
            "--job-id",
            "short-script",
        ],
    )
    .await
    .unwrap_err();
    let path = proposer.trim_start_matches("scripted:");
    assert_eq!(
        error.to_string(),
        format!("proposer script {path} holds 3 proposals, fewer than --rounds 4")
    );
    assert!(!fixture.ctx.jobs_dir().join("short-script").exists());
    assert!(
        gents::optimization::load_job(&fixture.ctx.access, &fixture.ctx.owner, "short-script")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn promote_refuses_a_wrong_digest_then_promotes_and_revert_closes_the_job() {
    let fixture = Fixture::new().await;
    accepted_job(&fixture, "job-1").await;
    let view = gents::optimization::show(&fixture.ctx.access, &fixture.ctx.owner, "job-1")
        .await
        .unwrap();
    let digest = view
        .checkpoint
        .expect("an accepted job retains a checkpoint")
        .pack_digest;

    let wrong = optimization(&fixture, &["promote", "job-1", "--digest", "sha256:wrong"])
        .await
        .unwrap_err();
    assert!(wrong.to_string().starts_with("wrong_digest: "), "{wrong:#}");

    let promoted = optimization(&fixture, &["promote", "job-1", "--digest", digest.as_str()])
        .await
        .unwrap();
    assert_eq!(
        promoted.lines().next(),
        Some(UNCALIBRATED_BANNER),
        "{promoted}"
    );
    assert!(
        promoted
            .lines()
            .nth(1)
            .is_some_and(|line| line.starts_with("promoted job job-1: the target now digests to ")),
        "{promoted}"
    );
    let target = gents::optimization::show(&fixture.ctx.access, &fixture.ctx.owner, "job-1")
        .await
        .unwrap()
        .job
        .journal
        .iter()
        .rev()
        .find_map(|entry| match entry {
            gents::optimization::JournalEntry::Promoted { target_digest, .. } => {
                Some(target_digest.clone())
            }
            _ => None,
        })
        .expect("a promotion is journaled");
    assert!(
        promoted.contains(&format!("--digest {target}")),
        "{promoted}"
    );

    let again = optimization(&fixture, &["promote", "job-1", "--digest", digest.as_str()])
        .await
        .unwrap_err();
    assert!(again.to_string().starts_with("not_ready: "), "{again:#}");

    let wrong = optimization(&fixture, &["revert", "job-1", "--digest", "sha256:wrong"])
        .await
        .unwrap_err();
    assert!(wrong.to_string().starts_with("wrong_digest: "), "{wrong:#}");
    let reverted = optimization(&fixture, &["revert", "job-1", "--digest", target.as_str()])
        .await
        .unwrap();
    assert_eq!(
        reverted.trim_end(),
        "reverted job job-1; Reverted is final, and a further promotion is a new job"
    );
    let shown = optimization(&fixture, &["show", "job-1"]).await.unwrap();
    assert!(shown.contains("state reverted"), "{shown}");
}

#[tokio::test]
async fn rm_keeps_running_and_ready_jobs_and_removes_a_reverted_one() {
    let fixture = Fixture::new().await;
    let pack = fixture.pack_arg();
    let proposer = proposer_file(&fixture);
    let scripted = executor(&[]);
    let registry = CheckRegistry::builtin();
    let interrupted = CancellationToken::new();
    interrupted.cancel();
    optimization_with(
        &fixture,
        &[
            "run",
            DEFINITION,
            "--subject",
            pack.as_str(),
            "--profile",
            "local",
            "--proposer",
            proposer.as_str(),
            "--job-id",
            "job-running",
        ],
        &deps(&scripted, &registry, interrupted),
    )
    .await
    .unwrap_err();
    let running_dir = fixture.ctx.jobs_dir().join("job-running");
    assert!(
        running_dir.is_dir(),
        "freeze copied the baseline into the job directory"
    );
    let refused = optimization(&fixture, &["rm", "job-running"])
        .await
        .unwrap_err();
    assert_eq!(
        refused.to_string(),
        "job job-running is running: only a job that is nothing_to_promote, exhausted, failed, stale, promoted or reverted is removed without --force"
    );
    let forced = optimization(&fixture, &["rm", "job-running", "--force"])
        .await
        .unwrap();
    assert!(forced.contains("bytes reclaimed"), "{forced}");
    assert!(!running_dir.exists());

    accepted_job(&fixture, "job-1").await;
    let kept = optimization(&fixture, &["rm", "job-1"]).await.unwrap_err();
    assert!(
        kept.to_string()
            .starts_with("job job-1 is ready_to_promote: "),
        "{kept:#}"
    );

    let held = gents::optimization::held_runs(&fixture.ctx.access, &fixture.ctx.owner)
        .await
        .unwrap();
    let (run_id, _) = held
        .iter()
        .find(|(_, (job, _))| job == "job-1")
        .expect("the ready job holds its runs");
    let refused = eval(&fixture, &["rm", run_id.as_str()]).await.unwrap_err();
    assert_eq!(
        refused.to_string(),
        format!(
            "run {run_id} is evidence of optimization job job-1, which is ready_to_promote; pass --force to delete its directory anyway"
        )
    );

    let view = gents::optimization::show(&fixture.ctx.access, &fixture.ctx.owner, "job-1")
        .await
        .unwrap();
    let digest = view.checkpoint.expect("a retained checkpoint").pack_digest;
    let promotion = gents::optimization::promote(
        &fixture.ctx.access,
        &fixture.ctx.owner,
        "job-1",
        &digest,
        &fixture.ctx.owner,
    )
    .await
    .unwrap();
    gents::optimization::revert(
        &fixture.ctx.access,
        &fixture.ctx.owner,
        "job-1",
        &promotion.target_digest,
        &fixture.ctx.owner,
    )
    .await
    .unwrap();
    let removed = optimization(&fixture, &["rm", "job-1"]).await.unwrap();
    assert!(removed.starts_with("removed "), "{removed}");
    assert!(!fixture.ctx.jobs_dir().join("job-1").exists());
    let shown = optimization(&fixture, &["show", "job-1"]).await.unwrap();
    assert!(
        shown.contains("state reverted"),
        "the documents stay: {shown}"
    );
}

#[tokio::test]
async fn rm_force_of_a_ready_job_says_a_later_promote_will_refuse() {
    let fixture = Fixture::new().await;
    accepted_job(&fixture, "job-1").await;
    let digest = gents::optimization::show(&fixture.ctx.access, &fixture.ctx.owner, "job-1")
        .await
        .unwrap()
        .checkpoint
        .expect("an accepted job retains a checkpoint")
        .pack_digest;
    let removed = optimization(&fixture, &["rm", "job-1", "--force"])
        .await
        .unwrap();
    let dir = fixture.ctx.jobs_dir().join("job-1");
    assert!(
        removed.lines().any(|line| line.contains(&format!(
            "; a later `gents optimization promote job-1` will refuse: the retained checkpoint pack under {} is gone",
            dir.display()
        ))),
        "{removed}"
    );
    let refused = optimization(&fixture, &["promote", "job-1", "--digest", digest.as_str()])
        .await
        .unwrap_err();
    assert!(
        refused.to_string().starts_with("rebuild_mismatch: "),
        "{refused:#}"
    );
}

#[test]
fn run_and_rm_help_say_what_absence_and_force_mean() {
    use clap::CommandFactory;

    let mut cli = Cli::command();
    let optimization = cli
        .find_subcommand_mut("optimization")
        .expect("optimization");
    let run = optimization
        .find_subcommand_mut("run")
        .expect("run")
        .render_long_help()
        .to_string();
    assert!(!run.contains(&u64::MAX.to_string()), "{run}");
    assert!(run.contains("unlimited when absent"), "{run}");
    let rm = optimization
        .find_subcommand_mut("rm")
        .expect("rm")
        .render_long_help()
        .to_string();
    assert!(
        rm.contains("a job waiting to be promoted can no longer be promoted"),
        "{rm}"
    );

    let command =
        super::testing::optimization_command(&["run", DEFINITION, "--subject", "monitor"]);
    let crate::cli::OptimizationCommand::Run(args) = command else {
        panic!("not run");
    };
    assert_eq!(args.max_tokens, None);
}

#[tokio::test]
async fn rm_refuses_a_job_directory_that_is_not_under_this_home() {
    let fixture = Fixture::new().await;
    accepted_job(&fixture, "job-1").await;
    let node = fixture
        .ctx
        .access
        .local_node()
        .expect("the fixture is embedded");
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(elsewhere.path().join("eval").join("jobs")).unwrap();
    let other_home = crate::commands::eval::EvalContext {
        access: gents::ConfigAccess::local(node.clone()),
        home_dir: elsewhere.path().to_path_buf(),
        owner: fixture.ctx.owner.clone(),
        runtime: fixture.ctx.runtime.clone(),
    };
    for argv in [&["rm", "job-1"][..], &["rm", "job-1", "--force"][..]] {
        let crate::cli::OptimizationCommand::Rm(args) = super::testing::optimization_command(argv)
        else {
            panic!("not rm");
        };
        let error = super::rm(&other_home, &args, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "job job-1: its directory is not under this home",
            "{argv:?}"
        );
        assert!(fixture.ctx.jobs_dir().join("job-1").is_dir(), "{argv:?}");
    }
}

#[tokio::test]
async fn rm_refuses_a_job_id_that_is_not_one_path_component() {
    let fixture = Fixture::new().await;
    let outside = fixture.ctx.home_dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    for job_id in ["../outside", "a/b", ".."] {
        let error = optimization(&fixture, &["rm", job_id, "--force"])
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("job_id {job_id:?} must be one ordinary path component"),
        );
    }
    for job_id in ["", " "] {
        let error = optimization(&fixture, &["rm", job_id, "--force"])
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("job_id {job_id:?} must not be blank"),
        );
    }
    assert!(outside.is_dir());
}
