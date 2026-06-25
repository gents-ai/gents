use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    anyhow::bail!(
        "ChatGPT Codex credentials are DefraDB documents; use `defra-agent codex-login` and run through a configured behavior"
    )
}
