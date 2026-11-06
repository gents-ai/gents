mod client;
mod items;
mod source;

#[cfg(test)]
mod tests;

use anyhow::Context;
use gents_shell::keys::KeySet;
use gents_shell::source::Intent;
use tokio::runtime::Handle;

/// Runs the shell on the runtime at `remote` until the user quits. Its first tab takes `agent`
/// and sends `prompt`.
pub(crate) async fn open(
    remote: &str,
    agent: Option<String>,
    prompt: Option<String>,
    runtime: Handle,
) -> anyhow::Result<()> {
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
    let source = runtime.spawn(source::run(
        remote.to_string(),
        agent,
        prompt,
        bridged,
        updates.clone(),
        runtime.clone(),
        client::ANSWER_WAIT,
    ));
    let ui = runtime
        .spawn_blocking(move || {
            gents_shell::terminal::run_connected(KeySet::Vim, intents, updates, inputs)
        })
        .await;
    source.abort();
    ui.context("the shell's terminal thread panicked")?
        .context("running the shell")
}
