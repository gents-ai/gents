mod forms;
mod git;
mod items;
mod menus;
mod packs;
mod prefs;
mod sections;
mod setup;
mod source;
mod tree;
mod usage;
mod work;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

pub(crate) use setup::connect_first_sign_in;

use anyhow::Context;
use gents_shell::source::Intent;
use tokio::runtime::Handle;

/// Runs the shell on `home`'s runtime, over its GraphQL, until the user quits. Its first tab
/// takes `agent` and sends `prompt`, or opens `session`. Returns the session the center pane
/// showed at quit.
pub(crate) async fn open(
    home: &Path,
    agent: Option<String>,
    prompt: Option<String>,
    session: Option<String>,
    runtime: Handle,
) -> anyhow::Result<Option<String>> {
    let home = home.to_path_buf();
    let (reading, saved, setup) = (home.clone(), home.clone(), home.clone());
    let keys = runtime
        .spawn_blocking(move || prefs::read_keys(&saved))
        .await
        .context("reading the saved key set")?;
    let work = runtime
        .spawn_blocking(move || work::work_for(&reading))
        .await
        .context("reading this home's GraphQL endpoint")?;
    let config = runtime
        .spawn_blocking(move || gents_service::home_state::read_finished_init_config(&setup))
        .await
        .context("reading this home's setup")??
        .context("this home is not set up: run `gents` to set it up")?;
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
    let cwd = std::env::current_dir().ok();
    // The runtime records no tool root another process can read, so the home's own setting
    // stands for it.
    let tree = config.tool_root.map(PathBuf::from).or_else(|| cwd.clone());
    let opening = source::Opening {
        node: config.node_did,
        agent,
        prompt,
        session,
        work,
        home,
        cwd,
        tree,
    };
    let source = runtime.spawn(source::run(
        opening,
        bridged,
        updates.clone(),
        runtime.clone(),
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
