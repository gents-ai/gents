//! A browser beneath the app. The page is a WKWebView this module owns, in
//! the window's content view directly under the app's webview. Where the
//! page shows, the app leaves its own view unpainted, so the page is seen
//! through it, and anything the app draws there (menus, the sidebar,
//! dialogs) stays above the page and live.
//!
//! The app's webview stays as Tauri builds it, the window's content (no
//! multiwebview child), so its key handling is unchanged. A router view of
//! this module's own sits over the page's open area: it sends a click to the
//! page unless the app reported something of its own drawn at that point.
//!
//! The page keeps its own website data store, apart from the app's, gets no
//! IPC, and loads only web pages.

#[derive(Clone, Copy, serde::Deserialize)]
pub struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Rect {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// Where the page goes, in the app's CSS pixels: `bounds` is the
/// placeholder's box, `open` the part of it the app's layout shows (its
/// clipping ancestors), `solids` what the app draws over it.
#[derive(serde::Deserialize)]
pub struct Placement {
    bounds: Rect,
    open: Rect,
    solids: Vec<Rect>,
    radius: f64,
}

/// A command key pressed while the page had the keyboard, handed to the
/// app's own shortcuts: `key` as the key event names it, `key_code` the
/// macOS virtual key code, from which the app derives the event's `code`.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct HandedKey {
    key: String,
    key_code: u16,
    meta: bool,
    shift: bool,
    alt: bool,
    ctrl: bool,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PageState {
    url: String,
    title: String,
    loading: bool,
    can_go_back: bool,
    can_go_forward: bool,
}

/// What the browser is asked to open: a web page, or a file on this
/// machine together with the folder its page may read, the file's own.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum Target {
    Web(String),
    File {
        file: std::path::PathBuf,
        folder: std::path::PathBuf,
    },
}

/// A web address (`http`, `https`), or a local file as a `file://` URL, an
/// absolute path or one under `~/`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
async fn target(text: &str) -> Result<Target, String> {
    let text = text.trim();
    let path = match tauri::Url::parse(text) {
        Ok(url) => match url.scheme() {
            "http" | "https" => return Ok(Target::Web(url.to_string())),
            "file" => url
                .to_file_path()
                .map_err(|()| format!("{text} isn't a path on this machine"))?,
            other => return Err(format!("{other} pages can't open in the browser")),
        },
        Err(_) if text.starts_with('/') => std::path::PathBuf::from(text),
        Err(_) => match text.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME")
                .map(|home| std::path::PathBuf::from(home).join(rest))
                .ok_or("the home folder isn't known")?,
            None => return Err(format!("{text} isn't a web address or a file")),
        },
    };
    let shown = path.display().to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let file =
            std::fs::canonicalize(&path).map_err(|_| format!("There's no file at {shown}"))?;
        if !file.is_file() {
            return Err(format!("{shown} is a folder, not a file"));
        }
        let folder = file
            .parent()
            .map(std::path::Path::to_path_buf)
            .ok_or_else(|| format!("{shown} has no folder"))?;
        Ok(Target::File { file, folder })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(target_os = "macos")]
#[allow(unused_unsafe)]
mod mac {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use std::sync::Mutex;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool, NSObject, ProtocolObject};
    use objc2::{
        class, define_class, msg_send, AllocAnyThread, ClassType, DefinedClass, MainThreadOnly,
    };
    use objc2_app_kit::{
        NSAlert, NSEvent, NSEventModifierFlags, NSModalResponse, NSOpenPanel, NSResponder,
        NSTextField, NSView, NSWindowOrderingMode,
    };
    use objc2_foundation::{
        ns_string, MainThreadMarker, NSArray, NSDictionary, NSKeyValueChangeKey,
        NSKeyValueObservingOptions, NSObjectNSKeyValueObserverRegistration, NSObjectProtocol,
        NSPoint, NSRect, NSSize, NSString, NSURLRequest, NSURL, NSUUID,
    };
    use objc2_web_kit::{
        WKFrameInfo, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
        WKOpenPanelParameters, WKUIDelegate, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore,
        WKWindowFeatures,
    };
    use tauri::{AppHandle, Emitter};

    use super::{HandedKey, PageState, Placement, Rect, Target};

    /* the page's own website data, kept across launches */
    const STORE: &str = "5B0C7F3E-6D1A-4E8B-9C2F-7A4D1E6B3C90";
    const OBSERVED: [&str; 5] = ["URL", "title", "loading", "canGoBack", "canGoForward"];
    /* NSAlertFirstButtonReturn, NSModalResponseOK */
    const FIRST_BUTTON: NSModalResponse = 1000;
    const CHOSE: NSModalResponse = 1;

    /* An emit evaluates script in the app's webview, which waits on a lock
    Tauri may already hold around the WebKit call that led here (a load, a
    KVO change), so every emit leaves the current call first. */
    fn emit<T>(app: &AppHandle, event: &'static str, payload: T)
    where
        T: serde::Serialize + Clone + Send + 'static,
    {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = app.emit(event, payload);
        });
    }

    pub struct RouterIvars {
        main: Retained<WKWebView>,
        page: Retained<PageView>,
        solids: RefCell<Vec<Rect>>,
    }

    define_class!(
        /// Sits over the page's open area. A point the app draws over falls
        /// through to the app's webview beneath; any other goes to the page.
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        #[ivars = RouterIvars]
        pub struct Router;

        impl Router {
            #[unsafe(method_id(hitTest:))]
            fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
                route(self, point)
            }
        }
    );

    fn route(router: &Router, point: NSPoint) -> Option<Retained<NSView>> {
        let frame = router.frame();
        let inside = point.x >= frame.origin.x
            && point.y >= frame.origin.y
            && point.x < frame.origin.x + frame.size.width
            && point.y < frame.origin.y + frame.size.height;
        if !inside || router.isHidden() {
            return None;
        }
        let ivars = router.ivars();
        let superview = unsafe { router.superview() };
        let local = ivars
            .main
            .convertPoint_fromView(point, superview.as_deref());
        let zoom = unsafe { ivars.main.pageZoom() }.max(0.01);
        let (x, y) = (local.x / zoom, local.y / zoom);
        if ivars
            .solids
            .borrow()
            .iter()
            .any(|solid| solid.contains(x, y))
        {
            return None;
        }
        ivars.page.hitTest(point)
    }

    pub struct PageViewIvars {
        app: AppHandle,
        main: Retained<WKWebView>,
    }

    define_class!(
        /// The page. While it has the keyboard, a command key the page
        /// doesn't need for editing goes back to the app, with the keyboard.
        #[unsafe(super(WKWebView, NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[ivars = PageViewIvars]
        pub struct PageView;

        impl PageView {
            #[unsafe(method(performKeyEquivalent:))]
            fn perform_key_equivalent(&self, event: &NSEvent) -> Bool {
                if hand_over(self, event) {
                    Bool::YES
                } else {
                    unsafe { msg_send![super(self), performKeyEquivalent: event] }
                }
            }
        }
    );

    fn has_keyboard(view: &PageView) -> bool {
        let Some(window) = view.window() else {
            return false;
        };
        let Some(responder) = window.firstResponder() else {
            return false;
        };
        let responder: &NSResponder = &responder;
        let is_view: bool = unsafe { msg_send![responder, isKindOfClass: NSView::class()] };
        is_view && unsafe { msg_send![responder, isDescendantOf: view] }
    }

    fn hand_over(view: &PageView, event: &NSEvent) -> bool {
        let flags = event.modifierFlags();
        /* AppKit offers a key equivalent to every view in the window, so
        only a key pressed while the page has the keyboard is the page's */
        if !flags.contains(NSEventModifierFlags::Command) || !has_keyboard(view) {
            return false;
        }
        let key = event
            .charactersIgnoringModifiers()
            .map(|key| key.to_string())
            .unwrap_or_default();
        /* the page's own editing: copy, paste, cut, select all, undo and redo */
        if matches!(key.to_lowercase().as_str(), "c" | "v" | "x" | "a" | "z") {
            return false;
        }
        let ivars = view.ivars();
        if let Some(window) = view.window() {
            window.makeFirstResponder(Some(&ivars.main));
        }
        emit(
            &ivars.app,
            "browser-key",
            HandedKey {
                key,
                key_code: event.keyCode(),
                meta: true,
                shift: flags.contains(NSEventModifierFlags::Shift),
                alt: flags.contains(NSEventModifierFlags::Option),
                ctrl: flags.contains(NSEventModifierFlags::Control),
            },
        );
        true
    }

    pub struct PageIvars {
        app: AppHandle,
        /* the folder a local file opened with may read, which is also as
        far as its links may go */
        files: RefCell<Option<String>>,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[ivars = PageIvars]
        pub struct PageDelegate;

        unsafe impl NSObjectProtocol for PageDelegate {}

        unsafe impl WKNavigationDelegate for PageDelegate {
            #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
            fn decide(
                &self,
                _view: &WKWebView,
                action: &WKNavigationAction,
                handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>,
            ) {
                let url = unsafe { action.request().URL() };
                let scheme = url
                    .as_ref()
                    .and_then(|url| unsafe { url.scheme() })
                    .map(|scheme| scheme.to_string());
                /* web pages and what they embed, and a local page's own
                folder; a link to another scheme or elsewhere on the disk
                stays put */
                let allowed = match scheme.as_deref() {
                    None => true,
                    Some("file") => {
                        let path = url
                            .as_ref()
                            .and_then(|url| unsafe { url.path() })
                            .map(|path| path.to_string());
                        match (path, self.ivars().files.borrow().as_deref()) {
                            (Some(path), Some(folder)) => path.starts_with(folder),
                            _ => false,
                        }
                    }
                    Some(scheme) => matches!(scheme, "http" | "https" | "about" | "blob" | "data"),
                };
                handler.call((if allowed {
                    WKNavigationActionPolicy::Allow
                } else {
                    WKNavigationActionPolicy::Cancel
                },));
            }
        }

        unsafe impl WKUIDelegate for PageDelegate {
            /* a link that asks for a new window opens here: the browser has
            one page */
            #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
            fn create(
                &self,
                view: &WKWebView,
                _configuration: &WKWebViewConfiguration,
                action: &WKNavigationAction,
                _features: &WKWindowFeatures,
            ) -> Option<Retained<WKWebView>> {
                unsafe { view.loadRequest(&action.request()) };
                None
            }

            #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
            fn alert(
                &self,
                view: &WKWebView,
                message: &NSString,
                _frame: &WKFrameInfo,
                done: &block2::DynBlock<dyn Fn()>,
            ) {
                let done = done.copy();
                ask(view, message, &["OK"], None, move |_, _| done.call(()));
            }

            #[unsafe(method(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:))]
            fn confirm(
                &self,
                view: &WKWebView,
                message: &NSString,
                _frame: &WKFrameInfo,
                done: &block2::DynBlock<dyn Fn(Bool)>,
            ) {
                let done = done.copy();
                ask(view, message, &["OK", "Cancel"], None, move |answer, _| {
                    done.call((Bool::new(answer == FIRST_BUTTON),))
                });
            }

            #[unsafe(method(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:))]
            fn prompt(
                &self,
                view: &WKWebView,
                prompt: &NSString,
                default_text: Option<&NSString>,
                _frame: &WKFrameInfo,
                done: &block2::DynBlock<dyn Fn(*mut NSString)>,
            ) {
                let done = done.copy();
                let text = NSString::from_str(&default_text.map(|text| text.to_string()).unwrap_or_default());
                ask(view, prompt, &["OK", "Cancel"], Some(&text), move |answer, typed| {
                    match typed.filter(|_| answer == FIRST_BUTTON) {
                        Some(typed) => done.call((Retained::as_ptr(&typed).cast_mut(),)),
                        None => done.call((null_mut(),)),
                    }
                });
            }

            #[unsafe(method(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:))]
            fn choose_files(
                &self,
                view: &WKWebView,
                parameters: &WKOpenPanelParameters,
                _frame: &WKFrameInfo,
                done: &block2::DynBlock<dyn Fn(*mut NSArray<NSURL>)>,
            ) {
                let done = done.copy();
                let Some(window) = view.window() else {
                    done.call((null_mut(),));
                    return;
                };
                let mtm = MainThreadMarker::from(view);
                let panel = NSOpenPanel::openPanel(mtm);
                panel.setCanChooseFiles(true);
                panel.setCanChooseDirectories(unsafe { parameters.allowsDirectories() });
                panel.setAllowsMultipleSelection(unsafe { parameters.allowsMultipleSelection() });
                let chosen = panel.clone();
                let handler = RcBlock::new(move |answer: NSModalResponse| {
                    if answer == CHOSE {
                        let urls = chosen.URLs();
                        done.call((Retained::as_ptr(&urls).cast_mut(),));
                    } else {
                        done.call((null_mut(),));
                    }
                });
                panel.beginSheetModalForWindow_completionHandler(&window, &handler);
            }
        }

        impl PageDelegate {
            #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
            fn observe(
                &self,
                _key: Option<&NSString>,
                object: Option<&AnyObject>,
                _change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
                _context: *mut c_void,
            ) {
                if let Some(object) = object {
                    /* only the page is observed */
                    let view = unsafe { &*(object as *const AnyObject as *const WKWebView) };
                    report(&self.ivars().app, view);
                }
            }
        }
    );

    /* A page's alert, confirm or prompt, as a sheet on the window, titled by
    the page's host as browsers do, so it can't pass for the app's own. */
    fn ask(
        view: &WKWebView,
        message: &NSString,
        buttons: &[&str],
        input: Option<&NSString>,
        then: impl Fn(NSModalResponse, Option<Retained<NSString>>) + 'static,
    ) {
        let Some(window) = view.window() else {
            then(FIRST_BUTTON + 1, None);
            return;
        };
        let mtm = MainThreadMarker::from(view);
        let host = unsafe { view.URL() }
            .and_then(|url| unsafe { url.host() })
            .map(|host| host.to_string())
            .unwrap_or_else(|| "This page".to_owned());
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(&format!("{host} says")));
        alert.setInformativeText(message);
        for button in buttons {
            alert.addButtonWithTitle(&NSString::from_str(button));
        }
        let field = input.map(|text| {
            let field = NSTextField::textFieldWithString(text, mtm);
            field.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(280.0, 24.0),
            ));
            alert.setAccessoryView(Some(&field));
            field
        });
        let handler = RcBlock::new(move |answer: NSModalResponse| {
            then(answer, field.as_ref().map(|field| field.stringValue()));
        });
        alert.beginSheetModalForWindow_completionHandler(&window, Some(&handler));
    }

    fn report(app: &AppHandle, view: &WKWebView) {
        let state = unsafe {
            PageState {
                url: view
                    .URL()
                    .and_then(|url| url.absoluteString())
                    .map(|url| url.to_string())
                    .unwrap_or_default(),
                title: view
                    .title()
                    .map(|title| title.to_string())
                    .unwrap_or_default(),
                loading: view.isLoading(),
                can_go_back: view.canGoBack(),
                can_go_forward: view.canGoForward(),
            }
        };
        emit(app, "browser-state", state);
    }

    struct Browser {
        main: Retained<WKWebView>,
        page: Retained<PageView>,
        router: Retained<Router>,
        delegate: Retained<PageDelegate>,
    }

    struct Slot(Option<Browser>);
    // SAFETY: the slot is only read or written inside `with_webview`
    // closures, which AppKit runs on the main thread; `on_main` checks it.
    unsafe impl Send for Slot {}
    static BROWSER: Mutex<Slot> = Mutex::new(Slot(None));

    fn on_main<T>(work: impl FnOnce(MainThreadMarker, &mut Option<Browser>) -> T) -> Option<T> {
        let mtm = MainThreadMarker::new()?;
        let mut slot = BROWSER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Some(work(mtm, &mut slot.0))
    }

    fn draws_background(main: &WKWebView, draws: bool) {
        unsafe {
            let value: Retained<AnyObject> =
                msg_send![class!(NSNumber), numberWithBool: Bool::new(draws)];
            let _: () = msg_send![main, setValue: &*value, forKey: ns_string!("drawsBackground")];
        }
    }

    fn create(
        mtm: MainThreadMarker,
        app: &AppHandle,
        main: &Retained<WKWebView>,
    ) -> Option<Browser> {
        let superview = unsafe { main.superview() }?;
        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        if let Some(id) = NSUUID::initWithUUIDString(NSUUID::alloc(), &NSString::from_str(STORE)) {
            let store = unsafe { WKWebsiteDataStore::dataStoreForIdentifier(&id, mtm) };
            unsafe { config.setWebsiteDataStore(&store) };
        }
        /* sites tailor themselves to Safari; a bare WebKit agent gets
        their fallback pages */
        unsafe {
            config.setApplicationNameForUserAgent(Some(ns_string!("Version/26.0 Safari/605.1.15")))
        };
        let page: Retained<PageView> = {
            let this = PageView::alloc(mtm).set_ivars(PageViewIvars {
                app: app.clone(),
                main: main.clone(),
            });
            unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO, configuration: &*config] }
        };
        let delegate: Retained<PageDelegate> = {
            let this = PageDelegate::alloc(mtm).set_ivars(PageIvars {
                app: app.clone(),
                files: RefCell::new(None),
            });
            unsafe { msg_send![super(this), init] }
        };
        unsafe {
            page.setInspectable(true);
            page.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            page.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            for key in OBSERVED {
                page.addObserver_forKeyPath_options_context(
                    &delegate,
                    &NSString::from_str(key),
                    NSKeyValueObservingOptions::New,
                    null_mut(),
                );
            }
            let _: () = msg_send![&*page, setWantsLayer: Bool::YES];
        }
        let router: Retained<Router> = {
            let this = Router::alloc(mtm).set_ivars(RouterIvars {
                main: main.clone(),
                page: page.clone(),
                solids: RefCell::new(Vec::new()),
            });
            unsafe { msg_send![super(this), init] }
        };
        /* the app's webview stops painting a background of its own: where
        its page paints nothing, the browser beneath shows */
        draws_background(main, false);
        page.setHidden(true);
        router.setHidden(true);
        superview.addSubview_positioned_relativeTo(&page, NSWindowOrderingMode::Below, Some(main));
        superview.addSubview_positioned_relativeTo(
            &router,
            NSWindowOrderingMode::Above,
            Some(main),
        );
        Some(Browser {
            main: main.clone(),
            page,
            router,
            delegate,
        })
    }

    /* a rect in the app's CSS pixels, in the content view's coordinates */
    fn frame(main: &WKWebView, superview: &NSView, rect: Rect) -> NSRect {
        let zoom = unsafe { main.pageZoom() };
        let local = NSRect::new(
            NSPoint::new(rect.x * zoom, rect.y * zoom),
            NSSize::new(rect.width * zoom, rect.height * zoom),
        );
        main.convertRect_toView(local, Some(superview))
    }

    fn main_view(main: *mut c_void) -> Option<Retained<WKWebView>> {
        unsafe { Retained::retain(main.cast::<WKWebView>()) }
    }

    pub fn open(app: AppHandle, main: *mut c_void, target: Target) {
        let Some(main) = main_view(main) else { return };
        on_main(|mtm, slot| {
            if slot.is_none() {
                *slot = create(mtm, &app, &main);
            }
            let Some(browser) = slot.as_ref() else { return };
            let files = &browser.delegate.ivars().files;
            match target {
                Target::Web(url) => {
                    files.replace(None);
                    if let Some(url) = NSURL::URLWithString(&NSString::from_str(&url)) {
                        let request = NSURLRequest::requestWithURL(&url);
                        unsafe { browser.page.loadRequest(&request) };
                    }
                }
                Target::File { file, folder } => {
                    let mut root = folder.display().to_string();
                    if !root.ends_with('/') {
                        root.push('/');
                    }
                    let file =
                        NSURL::fileURLWithPath(&NSString::from_str(&file.display().to_string()));
                    let folder =
                        NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&root), true);
                    files.replace(Some(root));
                    unsafe {
                        browser
                            .page
                            .loadFileURL_allowingReadAccessToURL(&file, &folder)
                    };
                }
            }
        });
    }

    pub fn place(main: *mut c_void, placement: Placement) {
        let Some(main) = main_view(main) else { return };
        on_main(|_, slot| {
            let Some(browser) = slot.as_ref() else { return };
            let Some(superview) = (unsafe { main.superview() }) else {
                return;
            };
            let zoom = unsafe { main.pageZoom() };
            browser
                .page
                .setFrame(frame(&main, &superview, placement.bounds));
            browser
                .router
                .setFrame(frame(&main, &superview, placement.open));
            *browser.router.ivars().solids.borrow_mut() = placement.solids;
            unsafe {
                let layer: *mut AnyObject = msg_send![&*browser.page, layer];
                if !layer.is_null() {
                    let _: () = msg_send![layer, setCornerRadius: placement.radius * zoom];
                    let _: () = msg_send![layer, setMasksToBounds: Bool::YES];
                    /* kCALayerMinXMinYCorner | kCALayerMaxXMinYCorner: the
                    panel's bottom corners; the toolbar covers the top */
                    let _: () = msg_send![layer, setMaskedCorners: 3_usize];
                }
            }
            browser.page.setHidden(false);
            browser.router.setHidden(false);
        });
    }

    pub fn hide() {
        on_main(|_, slot| {
            if let Some(browser) = slot.as_ref() {
                browser.page.setHidden(true);
                browser.router.setHidden(true);
                browser.router.ivars().solids.borrow_mut().clear();
                /* the keyboard can't stay with a page no one sees */
                if let Some(window) = browser.page.window() {
                    window.makeFirstResponder(Some(&browser.main));
                }
            }
        });
    }

    pub fn go(action: &str) {
        on_main(|_, slot| {
            let Some(browser) = slot.as_ref() else { return };
            unsafe {
                match action {
                    "back" => drop(browser.page.goBack()),
                    "forward" => drop(browser.page.goForward()),
                    "reload" => drop(browser.page.reload()),
                    "stop" => browser.page.stopLoading(),
                    _ => {}
                }
            }
        });
    }

    pub fn close() {
        on_main(|_, slot| {
            if let Some(browser) = slot.take() {
                unsafe {
                    for key in OBSERVED {
                        browser
                            .page
                            .removeObserver_forKeyPath(&browser.delegate, &NSString::from_str(key));
                    }
                }
                if let Some(window) = browser.page.window() {
                    window.makeFirstResponder(Some(&browser.main));
                }
                browser.router.removeFromSuperview();
                browser.page.removeFromSuperview();
                draws_background(&browser.main, true);
            }
        });
    }
}

#[cfg(target_os = "macos")]
use tauri::Manager;

#[cfg(target_os = "macos")]
fn with_main(
    webview: &tauri::Webview,
    work: impl FnOnce(*mut std::ffi::c_void) + Send + 'static,
) -> Result<(), String> {
    webview
        .with_webview(move |platform| work(platform.inner()))
        .map_err(|error| error.to_string())
}

/* the latest `seq` applied; see `in_order` */
#[cfg(target_os = "macos")]
static APPLIED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Runs `work` unless a later command already ran. The commands are async,
/// each on a thread of its own, so they can reach the main thread out of
/// order; a placement arriving after the hide that followed it would put
/// the page back, unseen behind the app and taking its clicks. Each command
/// carries the window's increasing `seq`, and an older one is dropped.
#[cfg(target_os = "macos")]
fn in_order(
    webview: &tauri::Webview,
    seq: u64,
    work: impl FnOnce(*mut std::ffi::c_void) + Send + 'static,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    with_main(webview, move |main| {
        if APPLIED.fetch_max(seq, Ordering::SeqCst) <= seq {
            work(main);
        }
    })
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn browser_open(webview: tauri::Webview, url: String, seq: u64) -> Result<(), String> {
    let target = target(&url).await?;
    let app = webview.app_handle().clone();
    in_order(&webview, seq, move |main| mac::open(app, main, target))
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn browser_place(
    webview: tauri::Webview,
    placement: Placement,
    seq: u64,
) -> Result<(), String> {
    in_order(&webview, seq, move |main| mac::place(main, placement))
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn browser_hide(webview: tauri::Webview, seq: u64) -> Result<(), String> {
    in_order(&webview, seq, |_| mac::hide())
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn browser_go(webview: tauri::Webview, action: String) -> Result<(), String> {
    with_main(&webview, move |_| mac::go(&action))
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn browser_close(webview: tauri::Webview, seq: u64) -> Result<(), String> {
    in_order(&webview, seq, |_| mac::close())
}
