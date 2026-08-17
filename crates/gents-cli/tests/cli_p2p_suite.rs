//! P2P CLI suites: pairing, network membership, scope templates. Every
//! test boots iroh-enabled server pairs; heaviest non-live group.

mod support;

#[path = "suites/cli_p2p.rs"]
mod cli_p2p;
#[path = "suites/cli_p2p_network.rs"]
mod cli_p2p_network;
#[path = "suites/cli_p2p_templates.rs"]
mod cli_p2p_templates;
