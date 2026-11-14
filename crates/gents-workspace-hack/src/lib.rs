//! Third-party feature unification for per-crate test builds.
//!
//! Workspace crates name this crate only as a dev-dependency, so every
//! `cargo test -p <crate>` resolves shared third-party crates with the same
//! features and reuses one compiled copy of the gents crates beneath it.
//! Production builds never depend on it, so their feature sets are unchanged.
