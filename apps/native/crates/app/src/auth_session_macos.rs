//! Native macOS `ASWebAuthenticationSession` wrapper for Clerk social OAuth (`com.t3tools.t3code://callback`).
//! Uses `objc2` + `block2` against `AuthenticationServices.framework` directly so no external browser
//! tab or localhost callback server is required.

use tokio::sync::oneshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthSessionError {
    Cancelled,
    Unavailable(String),
    Failed(String),
}

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use block2::RcBlock;
    use gpui::Window;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, NSObject};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{NSApplication, NSView, NSWindow};
    use objc2_foundation::{NSObjectProtocol, NSString, NSURL};
    use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::AuthSessionError;

    /// `ASWebAuthenticationSessionErrorCodeCanceledLogin` = 1.
    const AS_WEB_AUTH_ERROR_CANCELED_LOGIN: isize = 1;

    #[link(name = "AuthenticationServices", kind = "framework")]
    unsafe extern "C" {
        static ASWebAuthenticationSessionErrorDomain: *const AnyObject;
    }

    struct AnchorProviderIvars {
        window: Option<Retained<NSWindow>>,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[ivars = AnchorProviderIvars]
        struct T3AuthAnchorProvider;

        unsafe impl NSObjectProtocol for T3AuthAnchorProvider {}

        impl T3AuthAnchorProvider {
            #[unsafe(method(presentationAnchorForWebAuthenticationSession:))]
            fn presentation_anchor(&self, _session: *mut AnyObject) -> *mut NSWindow {
                if let Some(window) = &self.ivars().window {
                    return Retained::as_ptr(window).cast_mut();
                }
                let mtm = MainThreadMarker::from(self);
                let app = NSApplication::sharedApplication(mtm);
                if let Some(window) = app.keyWindow().or_else(|| app.mainWindow()) {
                    return Retained::as_ptr(&window).cast_mut();
                }
                std::ptr::null_mut()
            }
        }
    );

    impl T3AuthAnchorProvider {
        fn new(mtm: MainThreadMarker, window: Option<Retained<NSWindow>>) -> Retained<Self> {
            let this = mtm.alloc::<Self>().set_ivars(AnchorProviderIvars { window });
            // SAFETY: `T3AuthAnchorProvider` inherits `[NSObject init]`.
            unsafe { msg_send![super(this), init] }
        }
    }

    /// Holds a running `ASWebAuthenticationSession`, its presentation anchor provider, and completion
    /// block alive until the sign-in sheet finishes or is cancelled.
    pub struct ActiveAuthSession {
        session: Retained<AnyObject>,
        _provider: Retained<T3AuthAnchorProvider>,
        _block: RcBlock<dyn Fn(*mut AnyObject, *mut AnyObject)>,
        completed: Arc<AtomicBool>,
    }

    impl ActiveAuthSession {
        pub fn cancel(&self) {
            if !self.completed.swap(true, Ordering::AcqRel) {
                // SAFETY: `self.session` is a live `ASWebAuthenticationSession` on the main thread.
                unsafe {
                    let _: () = msg_send![&*self.session, cancel];
                }
            }
        }
    }

    impl Drop for ActiveAuthSession {
        fn drop(&mut self) {
            self.cancel();
        }
    }

    fn resolve_anchor_window(mtm: MainThreadMarker, window: Option<&mut Window>) -> Option<Retained<NSWindow>> {
        if let Some(window) = window
            && let Ok(handle) = HasWindowHandle::window_handle(window)
            && let RawWindowHandle::AppKit(appkit) = handle.as_raw()
        {
            // SAFETY: GPUI's AppKit handle points to its live content NSView on the main thread.
            let view: Option<Retained<NSView>> = unsafe { Retained::retain(appkit.ns_view.as_ptr().cast()) };
            if let Some(view) = view
                && let Some(ns_window) = view.window()
            {
                return Some(ns_window);
            }
        }
        let app = NSApplication::sharedApplication(mtm);
        app.keyWindow().or_else(|| app.mainWindow())
    }

    pub fn start(
        url: &str,
        callback_scheme: &str,
        window: Option<&mut Window>,
    ) -> Result<(ActiveAuthSession, tokio::sync::oneshot::Receiver<Result<String, AuthSessionError>>), AuthSessionError> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| AuthSessionError::Unavailable("ASWebAuthenticationSession requires the main thread.".into()))?;
        // Ensure AuthenticationServices.framework stays linked and loaded before class lookup.
        let _domain = unsafe { std::ptr::read_volatile(&ASWebAuthenticationSessionErrorDomain) };
        let cls = AnyClass::get(c"ASWebAuthenticationSession")
            .ok_or_else(|| AuthSessionError::Unavailable("ASWebAuthenticationSession is unavailable on this macOS version.".into()))?;

        let ns_url_str = NSString::from_str(url);
        let ns_url = NSURL::URLWithString(&ns_url_str)
            .ok_or_else(|| AuthSessionError::Failed("Invalid T3 Connect authorization URL.".into()))?;
        let ns_scheme = NSString::from_str(callback_scheme);

        let (tx, rx) = tokio::sync::oneshot::channel::<Result<String, AuthSessionError>>();
        let sender = Arc::new(Mutex::new(Some(tx)));
        let completed = Arc::new(AtomicBool::new(false));
        let block_completed = completed.clone();

        let block = RcBlock::new(move |callback_url: *mut AnyObject, error: *mut AnyObject| {
            block_completed.store(true, Ordering::Release);
            let outcome = if !callback_url.is_null() {
                // SAFETY: `callback_url` is a valid `NSURL` passed by AuthenticationServices.
                let abs: Option<Retained<NSString>> = unsafe { msg_send![callback_url, absoluteString] };
                match abs {
                    Some(text) => Ok(text.to_string()),
                    None => Err(AuthSessionError::Failed("T3 Connect returned an empty callback URL.".into())),
                }
            } else if !error.is_null() {
                // SAFETY: `error` is a valid `NSError` passed by AuthenticationServices.
                let code: isize = unsafe { msg_send![error, code] };
                if code == AS_WEB_AUTH_ERROR_CANCELED_LOGIN {
                    Err(AuthSessionError::Cancelled)
                } else {
                    let desc: Option<Retained<NSString>> = unsafe { msg_send![error, localizedDescription] };
                    Err(AuthSessionError::Failed(
                        desc.map(|s| s.to_string())
                            .unwrap_or_else(|| format!("T3 Connect sign-in failed (code {code}).")),
                    ))
                }
            } else {
                Err(AuthSessionError::Cancelled)
            };
            if let Ok(mut guard) = sender.lock()
                && let Some(tx) = guard.take()
            {
                let _ = tx.send(outcome);
            }
        });

        let anchor_window = resolve_anchor_window(mtm, window);
        let provider = T3AuthAnchorProvider::new(mtm, anchor_window);

        // SAFETY: `ASWebAuthenticationSession` responds to `alloc` and
        // `initWithURL:callbackURLScheme:completionHandler:`, returning a +1 retained instance (or null).
        let session: Option<Retained<AnyObject>> = unsafe {
            let alloc: *mut AnyObject = msg_send![cls, alloc];
            let raw: *mut AnyObject = msg_send![
                alloc,
                initWithURL: &*ns_url,
                callbackURLScheme: &*ns_scheme,
                completionHandler: &*block
            ];
            Retained::from_raw(raw)
        };
        let session = session.ok_or_else(|| {
            AuthSessionError::Failed("Could not initialize ASWebAuthenticationSession.".into())
        })?;

        let started: bool = unsafe {
            let _: () = msg_send![&*session, setPresentationContextProvider: &*provider];
            let _: () = msg_send![&*session, setPrefersEphemeralWebBrowserSession: false];
            msg_send![&*session, start]
        };
        if !started {
            return Err(AuthSessionError::Failed(
                "Could not start the macOS sign-in sheet.".into(),
            ));
        }

        Ok((
            ActiveAuthSession {
                session,
                _provider: provider,
                _block: block,
                completed,
            },
            rx,
        ))
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use gpui::Window;

    use super::AuthSessionError;

    pub struct ActiveAuthSession;

    impl ActiveAuthSession {
        pub fn cancel(&self) {}
    }

    pub fn start(
        _url: &str,
        _callback_scheme: &str,
        _window: Option<&mut Window>,
    ) -> Result<(ActiveAuthSession, tokio::sync::oneshot::Receiver<Result<String, AuthSessionError>>), AuthSessionError> {
        Err(AuthSessionError::Unavailable(
            "Native social sign-in sheet is only available on macOS.".into(),
        ))
    }
}

pub use imp::ActiveAuthSession;

/// Starts a native `ASWebAuthenticationSession` (macOS) for `url` listening for `callback_scheme`.
pub fn start(
    url: &str,
    callback_scheme: &str,
    window: Option<&mut gpui::Window>,
) -> Result<(ActiveAuthSession, oneshot::Receiver<Result<String, AuthSessionError>>), AuthSessionError> {
    imp::start(url, callback_scheme, window)
}
