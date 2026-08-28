//! Live-inference CLI suites; every test is #[ignore]d behind env gates.
//! The binary itself is feature-gated (`live-e2e`) so default builds skip
//! its link entirely — see the [[test]] block in Cargo.toml.

mod support;

#[path = "suites/cli_fleet_delegation_live.rs"]
mod cli_fleet_delegation_live;
#[path = "suites/cli_live.rs"]
mod cli_live;
#[path = "suites/cli_web_research_live.rs"]
mod cli_web_research_live;
