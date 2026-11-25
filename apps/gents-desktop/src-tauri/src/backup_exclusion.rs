//! iOS backs up an app's Application Support folder to iCloud by default,
//! and that is where this app keeps its identity key and its store (the
//! counterpart of Android's `allowBackup`). At launch the app's data folder
//! is marked excluded from backup, which covers everything in it. The same
//! Foundation call works on macOS, where it is tested; only iOS builds it
//! otherwise.

use std::path::Path;

use objc2_foundation::{NSNumber, NSString, NSURLIsExcludedFromBackupKey, NSURL};

/// Marks the app's data folder, `gents` under the platform's application
/// data folder (the parent of the desktop client's state,
/// `DesktopPaths::platform_default`), excluded from backup. A launch that
/// cannot mark it still starts; the failure is logged.
#[cfg(target_os = "ios")]
pub(crate) fn exclude_app_data() {
    let folder = gents_store::client_journal::paths::DesktopPaths::platform_default()
        .ok()
        .and_then(|paths| paths.root().parent().map(Path::to_path_buf));
    let Some(folder) = folder else {
        tracing::warn!("the app's data folder is unresolved; it is not excluded from backup");
        return;
    };
    if let Err(error) = exclude_from_backup(&folder) {
        tracing::warn!(folder = %folder.display(), %error, "the app's data folder is not excluded from backup");
    }
}

/// Creates `folder` if needed and marks it excluded from backup.
pub(crate) fn exclude_from_backup(folder: &Path) -> Result<(), String> {
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(&folder.to_string_lossy()));
    let excluded = NSNumber::numberWithBool(true);
    // SAFETY: the key is the documented `NSURLIsExcludedFromBackupKey`, whose
    // value is an NSNumber holding a boolean.
    unsafe { url.setResourceValue_forKey_error(Some(&excluded), NSURLIsExcludedFromBackupKey) }
        .map_err(|error| error.localizedDescription().to_string())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSNumber, NSString, NSURLIsExcludedFromBackupKey, NSURL};

    /// Reads the folder's backup exclusion afresh, from a new URL.
    fn excluded(folder: &std::path::Path) -> bool {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&folder.to_string_lossy()));
        let mut value: Option<Retained<AnyObject>> = None;
        // SAFETY: as in `exclude_from_backup`; the value read is an NSNumber.
        unsafe { url.getResourceValue_forKey_error(&mut value, NSURLIsExcludedFromBackupKey) }
            .expect("the folder's resource value reads");
        value
            .and_then(|value| value.downcast::<NSNumber>().ok())
            .is_some_and(|number| number.as_bool())
    }

    #[test]
    fn the_data_folder_is_created_and_reads_back_excluded_from_backup() {
        let home = tempfile::tempdir().unwrap();
        let folder = home.path().join("gents");
        assert!(!home.path().join("gents").exists());
        super::exclude_from_backup(&folder).unwrap();
        assert!(folder.is_dir());
        assert!(excluded(&folder));
        assert!(!excluded(home.path()), "only the data folder is marked");
    }
}
