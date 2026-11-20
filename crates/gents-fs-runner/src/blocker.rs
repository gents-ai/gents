use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

/// Pauses the scan of one directory so race tests can hold a traversal at a
/// known point, from inside this process or across the runner's process
/// boundary through the hidden `--block-*` flags.
///
/// `entered` receives the wall-clock entry time in nanoseconds since the Unix
/// epoch, published by rename so a reader never sees a partial write from a
/// process killed mid-write. The blocker waits until `gate` exists or the
/// directory is removed, else sleeps `pause`; `released` is written only when
/// the blocker returns on its own. Marker writes abort the runner on failure:
/// an unwritten marker must never read as preemption.
#[derive(Clone, Debug, Default)]
pub struct TraversalBlocker {
    pub dir: PathBuf,
    pub entered: Option<PathBuf>,
    pub gate: Option<PathBuf>,
    pub pause: Duration,
    pub released: Option<PathBuf>,
}

impl TraversalBlocker {
    /// Consumes `flag`'s value from `args` when `flag` is one of the hidden
    /// `--block-*` flags; returns whether it was.
    pub(crate) fn parse_flag(
        blocker: &mut Option<Self>,
        flag: &str,
        args: &mut impl Iterator<Item = String>,
    ) -> Result<bool> {
        if !matches!(
            flag,
            "--block-dir" | "--block-entered" | "--block-gate" | "--block-released" | "--block-ms"
        ) {
            return Ok(false);
        }
        let value = args
            .next()
            .with_context(|| format!("{flag} requires a value"))?;
        let blocker = blocker.get_or_insert_with(Self::default);
        match flag {
            "--block-dir" => blocker.dir = PathBuf::from(value),
            "--block-entered" => blocker.entered = Some(PathBuf::from(value)),
            "--block-gate" => blocker.gate = Some(PathBuf::from(value)),
            "--block-released" => blocker.released = Some(PathBuf::from(value)),
            _ => {
                blocker.pause = Duration::from_millis(
                    value
                        .parse()
                        .with_context(|| format!("--block-ms {value:?}"))?,
                )
            }
        }
        Ok(true)
    }

    /// The hidden flags that reproduce this blocker in a spawned runner.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = vec![
            "--block-dir".to_string(),
            self.dir.to_string_lossy().into_owned(),
            "--block-ms".to_string(),
            self.pause.as_millis().to_string(),
        ];
        for (flag, path) in [
            ("--block-entered", &self.entered),
            ("--block-gate", &self.gate),
            ("--block-released", &self.released),
        ] {
            if let Some(path) = path {
                args.push(flag.to_string());
                args.push(path.to_string_lossy().into_owned());
            }
        }
        args
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "test hook in the synchronous fs-runner process; the cross-process gate is a file"
    )]
    pub(crate) fn block(&self, dir: &Path) {
        if self.dir != dir {
            return;
        }
        if let Some(marker) = &self.entered {
            let entered_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after the Unix epoch")
                .as_nanos();
            let mut staged = marker.clone().into_os_string();
            staged.push(".staged");
            std::fs::write(&staged, entered_at.to_string()).expect("writing blocker entry marker");
            std::fs::rename(&staged, marker).expect("publishing blocker entry marker");
        }
        if let Some(gate) = &self.gate {
            while !gate.exists() && dir.exists() {
                std::thread::sleep(Duration::from_millis(2));
            }
        } else {
            std::thread::sleep(self.pause);
        }
        if let Some(marker) = &self.released {
            std::fs::write(marker, b"released").expect("writing blocker release marker");
        }
    }
}
