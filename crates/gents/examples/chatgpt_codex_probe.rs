use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    anyhow::bail!(
        "ChatGPT Codex credentials are DefraDB documents; use `gents codex-login` and run through a configured behavior"
    )
}
