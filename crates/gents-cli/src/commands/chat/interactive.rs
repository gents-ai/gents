use std::collections::BTreeMap;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::stream::{FuturesUnordered, StreamExt};
use gents::config_client::GraphqlEndpoint;
use gents::plugin::approval::{self, Answer, Request};
use tokio::sync::mpsc;

use super::{approvals, GoalBackedSubmission};
use crate::RequestSubmitOptions;

type InputLine = io::Result<String>;

/// One reader owns stdin for both messages and plugin approval answers. A
/// detached OS thread is intentional: a blocking stdin read must not hold
/// Tokio's blocking-pool shutdown open after `/exit`.
fn terminal_lines() -> mpsc::Receiver<InputLine> {
    let (sender, receiver) = mpsc::channel(32);
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if sender.blocking_send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

fn prompt(label: &str) -> Result<()> {
    write!(io::stdout(), "{label}> ")?;
    io::stdout().flush()?;
    Ok(())
}

fn pending_approval(home: &Path, session: &str) -> Result<Option<Request>> {
    Ok(approval::pending(home)?
        .into_iter()
        .find(|request| approvals::mine(request, session)))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    home: &Path,
    graphql: &GraphqlEndpoint,
    node: &str,
    session: &str,
    agent: Option<&str>,
    mut goal: Option<GoalBackedSubmission<'_>>,
    label: &str,
    timeout_secs: u64,
    poll_secs: u64,
    verbose: bool,
) -> Result<()> {
    let mut lines = terminal_lines();
    let mut turns = FuturesUnordered::new();
    let mut accepting = true;
    let mut approval_prompt: Option<Request> = None;
    let mut failure = None;
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    prompt(label)?;

    loop {
        if !accepting {
            deny_shown_approval(home, &mut approval_prompt)
                .context("denying shown plugin approval after chat input closed")?;
        }
        if !accepting && turns.is_empty() {
            return failure.map_or(Ok(()), Err);
        }
        tokio::select! {
            line = lines.recv(), if accepting => {
                let Some(line) = line else {
                    accepting = false;
                    continue;
                };
                let line = match line.context("reading chat input") {
                    Ok(line) => line,
                    Err(error) => {
                        eprintln!("Could not read chat input: {error:#}");
                        failure = Some(error);
                        accepting = false;
                        continue;
                    }
                };
                match handle_approval_command(home, &mut approval_prompt, &line) {
                    Ok(false) => {}
                    outcome => {
                        if let Err(error) = outcome {
                            eprintln!("Could not record plugin approval: {error:#}");
                            failure = Some(error);
                        }
                        prompt(label)?;
                        continue;
                    }
                }
                let content = line.trim();
                if content.is_empty() {
                    prompt(label)?;
                    continue;
                }
                if matches!(content, "/exit" | "/quit" | "exit" | "quit") {
                    if !turns.is_empty() {
                        eprintln!("Waiting for submitted requests to finish. Use `gents request interrupt` to stop one.");
                    }
                    accepting = false;
                    continue;
                }
                // Commit in input order; only output observation runs concurrently.
                let submitted = async {
                let input = gents::lifecycle::prepare_user_message_input(
                    &gents::ConfigAccess::Graphql(graphql.clone()),
                    node, session, Default::default(), gents_protocol::request_input::QueueDelivery::Steer,
                ).await?;
                match goal.take() {
                    Some(goal) => crate::request_helpers::create_goal_backed_agent_request(
                        graphql, node, content, session, agent, goal.objective, goal.token_budget, input,
                    ).await,
                    None => crate::create_agent_request(
                        graphql, node, content, Some(session), agent,
                        RequestSubmitOptions { input: Some(input), ..Default::default() },
                    ).await,
                }
                }.await;
                match submitted {
                    Ok(submitted) => {
                        eprintln!("Submitted {}", submitted.request_id);
                        let endpoint = graphql.clone();
                        turns.push(async move {
                            let result = super::stream_turn_progress(
                                &endpoint, &submitted, BTreeMap::new(), timeout_secs, poll_secs,
                                verbose, super::ApprovalHandling::Observe(home), false,
                            ).await;
                            (submitted.request_id, result)
                        });
                    }
                    Err(error) => {
                        eprintln!("Could not submit message: {error:#}");
                        failure = Some(error);
                    }
                }
                prompt(label)?;
            }
            completed = turns.next(), if !turns.is_empty() => {
                if let Some((request_id, result)) = completed {
                    if let Err(error) = result {
                        eprintln!("Request {request_id}: {error:#}");
                        failure = Some(error);
                    }
                    if accepting && approval_prompt.is_none() {
                        prompt(label)?;
                    }
                }
            }
            _ = ticker.tick() => {
                if let Some(shown) = &approval_prompt {
                    if !approval::pending(home)?.iter().any(|request| request.id == shown.id) {
                        approval_prompt = None;
                    }
                }
                if approval_prompt.is_none() {
                    if let Some(request) = pending_approval(home, session)? {
                        if !accepting || !io::stdin().is_terminal() {
                            eprintln!("{} No terminal input available; denied.", request.prompt());
                            approval::decide(home, &request.id, Answer::Deny)?;
                        } else {
                            println!("{}", request.prompt());
                            println!("Use /approve {} once|file|folder|deny", request.id);
                            approval_prompt = Some(request);
                        }
                    }
                }
            }
        }
    }
}

fn handle_approval_command(home: &Path, shown: &mut Option<Request>, line: &str) -> Result<bool> {
    let line = line.trim();
    if !line.starts_with("/approve") {
        return Ok(false);
    }
    let words = line.split_whitespace().collect::<Vec<_>>();
    anyhow::ensure!(
        words.len() == 3 && words[0] == "/approve",
        "use /approve <request-id> once|file|folder|deny with the displayed request ID"
    );
    let answer = match words[2] {
        "once" => Answer::Once,
        "file" => Answer::AlwaysPath,
        "folder" => Answer::AlwaysFolder,
        "deny" => Answer::Deny,
        _ => anyhow::bail!("approval action must be once, file, folder, or deny"),
    };
    let request = shown
        .as_ref()
        .context("no displayed approval is pending; wait for its request ID")?;
    anyhow::ensure!(
        request.id == words[1],
        "approval ID does not match the displayed request; use /approve {} once|file|folder|deny",
        request.id
    );
    anyhow::ensure!(
        approval::pending(home)?
            .iter()
            .any(|pending| pending.id == request.id),
        "approval {} is no longer pending; wait for the current request ID",
        request.id
    );
    approval::decide(home, &request.id, answer)?;
    *shown = None;
    Ok(true)
}

fn deny_shown_approval(home: &Path, shown: &mut Option<Request>) -> Result<()> {
    if let Some(request) = shown.as_ref() {
        if approval::pending(home)?
            .iter()
            .any(|pending| pending.id == request.id)
        {
            approval::decide(home, &request.id, Answer::Deny)?;
        }
        *shown = None;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closed_input_denies_shown_approval_and_releases_waiting_call() {
        let home = tempfile::tempdir().unwrap();
        let request = Request::new(
            "test/plugin",
            &gents::plugin::allowed::Resolved {
                target: home.path().join("outside.txt"),
                is_dir: false,
            },
            gents::pack::BindAccess::Read,
            Some("chat-session".into()),
        );
        let waiting_home = home.path().to_owned();
        let waiting_request = request.clone();
        let waiter = tokio::spawn(async move {
            approval::ask(&waiting_home, &waiting_request, Duration::from_secs(5)).await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while approval::pending(home.path()).unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut shown = Some(request);
        deny_shown_approval(home.path(), &mut shown).unwrap();
        assert!(shown.is_none());
        assert!(!tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap());
        assert!(gents::plugin::allowed::list(home.path())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn closed_input_preserves_approval_on_decision_error() {
        let home = tempfile::NamedTempFile::new().unwrap();
        let request = Request::new(
            "test/plugin",
            &gents::plugin::allowed::Resolved {
                target: home.path().with_extension("outside"),
                is_dir: false,
            },
            gents::pack::BindAccess::Read,
            Some("chat-session".into()),
        );
        let mut shown = Some(request);
        assert!(deny_shown_approval(home.path(), &mut shown).is_err());
        assert!(shown.is_some());
    }

    #[test]
    fn buffered_shorthand_and_ordinary_chat_are_never_approval_commands() {
        let home = tempfile::tempdir().unwrap();
        let mut shown = None;
        for chat in [
            "",
            "yes",
            "o",
            "once",
            "file",
            "folder",
            "deny",
            "no",
            "please continue",
            "no need to change that file",
            "yes and inspect the next task",
        ] {
            assert!(
                !handle_approval_command(home.path(), &mut shown, chat).unwrap(),
                "{chat}"
            );
        }
    }

    #[tokio::test]
    async fn correlated_approval_requires_exact_pending_id_and_valid_action() {
        let home = tempfile::tempdir().unwrap();
        let request = Request::new(
            "test/plugin",
            &gents::plugin::allowed::Resolved {
                target: home.path().join("outside.txt"),
                is_dir: false,
            },
            gents::pack::BindAccess::Read,
            Some("chat-session".into()),
        );
        let id = request.id.clone();
        let waiting_home = home.path().to_owned();
        let waiting_request = request.clone();
        let waiter = tokio::spawn(async move {
            approval::ask(&waiting_home, &waiting_request, Duration::from_secs(5)).await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while approval::pending(home.path()).unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut shown = Some(request);
        for invalid in [
            "/approve wrong once".to_string(),
            format!("/approve {id} yes"),
            format!("/approve {id}"),
            format!("/approve {id} once extra"),
        ] {
            assert!(handle_approval_command(home.path(), &mut shown, &invalid).is_err());
            assert!(shown.is_some());
            assert_eq!(approval::pending(home.path()).unwrap().len(), 1);
            assert!(!waiter.is_finished());
        }
        // These may have been buffered before the prompt was displayed.
        for text in ["yes", "o", "file"] {
            assert!(!handle_approval_command(home.path(), &mut shown, text).unwrap());
            assert!(!waiter.is_finished());
        }
        assert!(
            handle_approval_command(home.path(), &mut shown, &format!("/approve {id} once"))
                .unwrap()
        );
        assert!(shown.is_none());
        assert!(tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap());
        assert!(
            handle_approval_command(home.path(), &mut shown, &format!("/approve {id} once"))
                .is_err()
        );
        assert!(gents::plugin::allowed::list(home.path())
            .unwrap()
            .is_empty());
    }
}
