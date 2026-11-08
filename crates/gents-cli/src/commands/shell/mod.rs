mod client;
mod forms;
mod items;
mod menus;
mod packs;
mod prefs;
mod sections;
mod setup;
mod source;
mod tree;
mod work;

#[cfg(test)]
mod tests;

use std::path::Path;

use anyhow::Context;
use gents_shell::source::Intent;
use tokio::runtime::Handle;

/// Runs the shell on the runtime at `remote` until the user quits. Its first tab takes `agent`
/// and sends `prompt`. Jobs and the mailbox come from `home`'s GraphQL.
pub(crate) async fn open(
    home: &Path,
    remote: &str,
    agent: Option<String>,
    prompt: Option<String>,
    runtime: Handle,
) -> anyhow::Result<()> {
    let home = home.to_path_buf();
    let (reading, saved) = (home.clone(), home.clone());
    let keys = runtime
        .spawn_blocking(move || prefs::read_keys(&saved))
        .await
        .context("reading the saved key set")?;
    let work = runtime
        .spawn_blocking(move || work::work_for(&reading))
        .await
        .context("reading this home's GraphQL endpoint")?;
    let (updates, inputs) = std::sync::mpsc::channel();
    let (intents, intended) = std::sync::mpsc::channel::<Intent>();
    let (forward, bridged) = tokio::sync::mpsc::unbounded_channel();
    // Ends when the App drops its sender.
    runtime.spawn_blocking(move || {
        while let Ok(intent) = intended.recv() {
            if forward.send(intent).is_err() {
                return;
            }
        }
    });
    let opening = source::Opening {
        remote: remote.to_string(),
        agent,
        prompt,
        work,
        home,
    };
    let source = runtime.spawn(source::run(
        opening,
        bridged,
        updates.clone(),
        runtime.clone(),
        client::ANSWER_WAIT,
    ));
    let ui = runtime
        .spawn_blocking(move || {
            gents_shell::terminal::run_connected(keys, intents, updates, inputs)
        })
        .await;
    source.abort();
    ui.context("the shell's terminal thread panicked")?
        .context("running the shell")
}
