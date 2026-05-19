use anyhow::{anyhow, Result};

pub mod protocol;

mod context;
mod model;
mod output;
mod tools;
mod traversal;

pub use tools::execute_request;

use protocol::{GlobArgs, NativeFsRunnerRequest};

pub fn self_test() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "defra-native-fs-runner-self-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n")?;
    let output = execute_request(
        root.clone(),
        NativeFsRunnerRequest::Glob(GlobArgs {
            pattern: "**/*.rs".to_string(),
            path: Some(".".to_string()),
            max_matches: 10,
            raw_json: false,
        }),
    )?;
    let _ = std::fs::remove_dir_all(&root);
    if output.contains("src/main.rs") {
        Ok(())
    } else {
        Err(anyhow!(
            "self-test output did not include src/main.rs: {output}"
        ))
    }
}
