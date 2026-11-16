use std::path::Path;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use plist::Value;
use tokio::process::Command;

use super::{config, manager, ok, output, NativeServicePlatform, ScriptedRunner};

struct LoadedJob(Option<String>);

impl Drop for LoadedJob {
    fn drop(&mut self) {
        if let Some(target) = self.0.take() {
            let _ = std::process::Command::new("/bin/launchctl")
                .args(["bootout", &target])
                .output();
        }
    }
}

async fn starts(path: &Path) -> Result<usize> {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => Ok(contents.lines().count()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

#[tokio::test]
#[ignore = "requires a per-user launchd GUI domain"]
async fn loaded_launchd_job_restarts_after_clean_exit_and_stays_unloaded_after_stop() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let config = config(temp.path(), "gents");
    manager(
        config.clone(),
        NativeServicePlatform::Macos,
        ScriptedRunner::with_outputs([output(true, "501", ""), ok()]),
    )
    .install()?;
    let path = config.definition_path(NativeServicePlatform::Macos);
    let mut plist = Value::from_file(&path)?;
    let definition = plist.as_dictionary_mut().context("service dictionary")?;
    let label = format!("ai.gents.test.supervision.{}", uuid::Uuid::new_v4());
    let counter = temp.path().join("starts");
    definition.insert("Label".into(), Value::String(label.clone()));
    definition.insert(
        "ProgramArguments".into(),
        Value::Array(
            [
                "/bin/sh",
                "-c",
                r#"printf '%s\n' "$$" >> "$1""#,
                "gents-service-restart-fixture",
                counter.to_str().context("fixture counter path")?,
            ]
            .into_iter()
            .map(|value| Value::String(value.into()))
            .collect(),
        ),
    );
    plist.to_file_xml(&path)?;
    let uid = Command::new("/usr/bin/id").arg("-u").output().await?;
    ensure!(uid.status.success(), "could not identify the user domain");
    let uid: u32 = std::str::from_utf8(&uid.stdout)?.trim().parse()?;
    let domain = format!("gui/{uid}");
    let mut job = LoadedJob(Some(format!("{domain}/{label}")));
    let bootstrap = Command::new("/bin/launchctl")
        .args(["bootstrap", &domain])
        .arg(&path)
        .output()
        .await?;
    ensure!(
        bootstrap.status.success(),
        "bootstrap failed: {}",
        String::from_utf8_lossy(&bootstrap.stderr)
    );
    tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            if starts(&counter).await? >= 2 {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("launchd did not restart the cleanly exiting fixture")??;
    let stopped = Command::new("/bin/launchctl")
        .args(["bootout", job.0.as_deref().context("owned fixture job")?])
        .output()
        .await?;
    ensure!(
        stopped.status.success(),
        "bootout failed: {}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    job.0 = None;
    let after_stop = starts(&counter).await?;
    tokio::time::sleep(Duration::from_secs(12)).await;
    ensure!(
        starts(&counter).await? == after_stop,
        "unloaded job restarted"
    );
    Ok(())
}
