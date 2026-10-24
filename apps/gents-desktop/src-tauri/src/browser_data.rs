use std::path::Path;

/// The same isolated WebKit store is used by native E2E windows and their reset
/// helper. A malformed or missing test identifier must never select user data.
pub(crate) fn isolated_store_identifier() -> anyhow::Result<Option<[u8; 16]>> {
    #[cfg(all(feature = "native-e2e", debug_assertions))]
    if std::env::var("GENTS_NATIVE_E2E").ok().as_deref() == Some("1") {
        let value = std::env::var("GENTS_E2E_WEBVIEW_STORE_ID")?;
        anyhow::ensure!(
            value.len() == 32 && value.is_ascii(),
            "WebView store ID must contain 32 hex digits"
        );
        let mut identifier = [0; 16];
        for (index, byte) in identifier.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)?;
        }
        anyhow::ensure!(identifier != [0; 16], "WebView store ID must not be zero");
        return Ok(Some(identifier));
    }
    Ok(None)
}

pub fn lock_store() -> anyhow::Result<gents_desktop_core::home_reset::BrowserStoreLock> {
    let isolated = isolated_store_identifier()?.map(|identifier| {
        identifier
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    });
    Ok(gents_desktop_core::home_reset::lock_browser_store(
        isolated.as_deref(),
    )?)
}

pub fn check_to_receipt(receipt: &Path) -> anyhow::Result<()> {
    let result = lock_store().map(drop);
    let value = match &result {
        Ok(()) => serde_json::json!({"completed": true}),
        Err(error) => serde_json::json!({"completed": false, "error": format!("{error:#}")}),
    };
    write_receipt(receipt, &value)?;
    result
}

/// The default store requires the installed app's bundle identity; E2E can
/// only select its explicit UUID. This helper promises logical clearing only,
/// not archival or the absence of WebKit-managed cache directories.
pub fn clear_to_receipt(receipt: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    let result = macos::clear_to_receipt(receipt);
    #[cfg(not(target_os = "macos"))]
    let result: anyhow::Result<()> = Err(anyhow::anyhow!(
        "native browser data reset is supported on macOS"
    ));
    if let Err(error) = &result {
        let _ = write_receipt(
            receipt,
            &serde_json::json!({"completed": false, "error": format!("{error:#}")}),
        );
    }
    result
}

fn write_receipt(receipt: &Path, value: &serde_json::Value) -> anyhow::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(receipt)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(target_os = "macos")]
mod macos {
    use anyhow::{Context, Result};
    use block2::RcBlock;
    use objc2::{rc::Retained, AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::{NSBundle, NSDate, NSRunLoop, NSString, NSUUID};
    use objc2_web_kit::WKWebsiteDataStore;
    use std::{
        cell::Cell,
        path::Path,
        rc::Rc,
        time::{Duration, Instant},
    };

    /// Foundation's concrete UUID class reports a different byte-initializer
    /// encoding than NSUUID. The string initializer avoids that runtime ABI check.
    pub(super) fn store(mtm: MainThreadMarker) -> Result<Retained<WKWebsiteDataStore>> {
        let identifier = super::isolated_store_identifier()?;
        if let Some(identifier) = identifier {
            let hex: String = identifier
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let text = format!(
                "{}-{}-{}-{}-{}",
                &hex[..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..]
            );
            let identifier =
                NSUUID::initWithUUIDString(NSUUID::alloc(), &NSString::from_str(&text))
                    .context("invalid isolated WebView store identifier")?;
            // The identified store API is public on the macOS 14+ E2E target.
            return Ok(unsafe { WKWebsiteDataStore::dataStoreForIdentifier(&identifier, mtm) });
        }
        let bundle = NSBundle::mainBundle()
            .bundleIdentifier()
            .context("browser reset requires the installed Gents app bundle")?;
        anyhow::ensure!(
            bundle.to_string() == "com.source-inc.gents",
            "browser reset requires the Gents app bundle identity"
        );
        Ok(unsafe { WKWebsiteDataStore::defaultDataStore(mtm) })
    }

    pub(super) fn wait_for_completion(done: &Cell<bool>) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(60);
        let run_loop = NSRunLoop::currentRunLoop();
        while !done.get() {
            anyhow::ensure!(
                Instant::now() < deadline,
                "WebKit did not acknowledge browser data clearing within 60 seconds"
            );
            run_loop.runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.05));
        }
        Ok(())
    }

    fn clear(store: &WKWebsiteDataStore, mtm: MainThreadMarker) -> Result<()> {
        let done = Rc::new(Cell::new(false));
        let completed = Rc::clone(&done);
        let callback = RcBlock::new(move || completed.set(true));
        // All objects and the completion block stay alive while the main run
        // loop delivers WebKit's completion on its required main thread.
        unsafe {
            store.removeDataOfTypes_modifiedSince_completionHandler(
                &WKWebsiteDataStore::allWebsiteDataTypes(mtm),
                &NSDate::distantPast(),
                &callback,
            );
        }
        wait_for_completion(&done)
    }

    pub(super) fn clear_to_receipt(receipt: &Path) -> Result<()> {
        anyhow::ensure!(
            !receipt.try_exists()?,
            "browser clear receipt already exists"
        );
        let _store = super::lock_store()?;
        let mtm = MainThreadMarker::new().context("browser reset must run on the main thread")?;
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
        let store = store(mtm)?;
        clear(&store, mtm)?;
        super::write_receipt(receipt, &serde_json::json!({"completed": true}))?;
        Ok(())
    }
}

#[cfg(all(target_os = "macos", feature = "native-e2e", debug_assertions))]
pub fn verify_isolated_clear(receipt: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    use block2::RcBlock;
    use objc2::{runtime::AnyObject, MainThreadMarker};
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{
        ns_string, NSArray, NSDictionary, NSHTTPCookie, NSHTTPCookieDomain, NSHTTPCookieName,
        NSHTTPCookiePath, NSHTTPCookieValue,
    };
    use std::{cell::Cell, ptr::NonNull, rc::Rc};

    anyhow::ensure!(
        isolated_store_identifier()?.is_some(),
        "browser reset fixture requires an isolated UUID"
    );
    let mtm = MainThreadMarker::new().context("browser fixture must run on the main thread")?;
    let _app = NSApplication::sharedApplication(mtm);
    let store = macos::store(mtm)?;
    let cookies = unsafe { store.httpCookieStore() };
    let properties: objc2::rc::Retained<NSDictionary<_, AnyObject>> = NSDictionary::from_slices(
        unsafe {
            &[
                NSHTTPCookieDomain,
                NSHTTPCookieName,
                NSHTTPCookiePath,
                NSHTTPCookieValue,
            ]
        },
        &[
            ns_string!("gents-browser-reset.invalid").as_ref(),
            ns_string!("reset-fixture").as_ref(),
            ns_string!("/").as_ref(),
            ns_string!("present").as_ref(),
        ],
    );
    let cookie =
        unsafe { NSHTTPCookie::cookieWithProperties(&properties) }.context("fixture cookie")?;
    let done = Rc::new(Cell::new(false));
    let completed = done.clone();
    let callback = RcBlock::new(move || completed.set(true));
    unsafe {
        cookies.setCookie_completionHandler(&cookie, Some(&callback));
    }
    macos::wait_for_completion(&done)?;

    let count = || -> anyhow::Result<usize> {
        let done = Rc::new(Cell::new(false));
        let completed = done.clone();
        let count = Rc::new(Cell::new(0));
        let received = count.clone();
        let callback = RcBlock::new(move |values: NonNull<NSArray<NSHTTPCookie>>| {
            received.set(unsafe { values.as_ref() }.count());
            completed.set(true);
        });
        unsafe {
            cookies.getAllCookies(&callback);
        }
        macos::wait_for_completion(&done)?;
        Ok(count.get())
    };
    anyhow::ensure!(count()? == 1, "fixture cookie was not persisted");
    clear_to_receipt(receipt)?;
    anyhow::ensure!(
        count()? == 0,
        "browser clear completed with retained cookies"
    );
    Ok(())
}
