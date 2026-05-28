use anyhow::Result;
use rig::client::CompletionClient;
use rig::completion::Prompt;

#[tokio::main]
async fn main() -> Result<()> {
    let client = defra_agent::chatgpt_codex::build_responses_client("").await?;
    let agent = client
        .agent("gpt-5.2")
        .preamble("Answer with exactly one word.")
        .build();
    let response = agent.prompt("Say pong.").await?;
    println!("{}", response.trim());
    Ok(())
}
