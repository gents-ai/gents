use std::path::Path;

use anyhow::{ensure, Context, Result};

/// Only an explicit user unlock action may call this entry point. It must run
/// in the same executable as the managed server so macOS custody approval binds
/// to that helper. Loading existing keys neither opens the database nor creates
/// replacement keys, changes custody, or exports secret bytes to the editor.
pub fn unlock_runtime_keys(home: &Path) -> Result<()> {
    ensure!(
        home.is_absolute(),
        "runtime home must be an absolute directory"
    );
    let home = std::fs::canonicalize(home)
        .with_context(|| format!("opening initialized runtime home {}", home.display()))?;
    let (config, _) = super::read_home(&home)?;
    if let Some(encryption) = config.store_encryption.as_ref() {
        drop(encryption.load(
            &gents::store_key::home_key_file(&home),
            &gents::home::default_data_dir(&home),
        )?);
    }
    let _identity = crate::home_state::load_initialized_home_identity(&home, &config)?;
    Ok(())
}
