//! macOS delivery acceptance, the equivalent of the web's permission check and successful
//! `new Notification`. GPUI owns the response delegate, but its posting API has no result.

use gpui::{App, SystemNotification};
use t3_core::notifications::DeliveryOutcome;

pub async fn authorize() -> DeliveryOutcome {
    #[cfg(target_os = "macos")]
    {
        macos::authorize().await
    }
    #[cfg(not(target_os = "macos"))]
    {
        DeliveryOutcome::Unconfirmed
    }
}

pub fn post(notification: SystemNotification, cx: &App) -> impl Future<Output = DeliveryOutcome> + use<> {
    #[cfg(target_os = "macos")]
    let result = {
        let _ = cx;
        macos::post(notification)
    };
    #[cfg(not(target_os = "macos"))]
    let result = {
        cx.show_system_notification(notification);
        std::future::ready(DeliveryOutcome::Unconfirmed)
    };
    result
}

#[cfg(target_os = "macos")]
mod macos {
    use block2::RcBlock;
    use gpui::SystemNotification;
    use objc2::{
        msg_send,
        rc::Retained,
        runtime::{AnyClass, AnyObject, Bool},
    };
    use objc2_foundation::NSString;
    use t3_core::notifications::DeliveryOutcome;

    pub async fn authorize() -> DeliveryOutcome {
        let Some(center) = center() else { return DeliveryOutcome::Unavailable };
        let (send, receive) = futures::channel::oneshot::channel();
        let send = std::sync::Mutex::new(Some(send));
        let callback = RcBlock::new(move |granted: Bool, error: *mut AnyObject| {
            if let Some(send) = send.lock().unwrap().take() {
                let _ = send.send(if !error.is_null() {
                    DeliveryOutcome::Failed
                } else if granted.as_bool() {
                    DeliveryOutcome::Accepted
                } else {
                    DeliveryOutcome::Denied
                });
            }
        });
        // SAFETY: UserNotifications' authorization callback is (BOOL, NSError *). Alert = 1<<2;
        // sound is played separately, so the system notification remains silent.
        unsafe {
            let _: () = msg_send![&*center, requestAuthorizationWithOptions: 1usize << 2, completionHandler: &*callback];
        }
        receive.await.unwrap_or(DeliveryOutcome::Failed)
    }

    pub async fn post(notification: SystemNotification) -> DeliveryOutcome {
        let Some(center) = center() else { return DeliveryOutcome::Unavailable };
        let (send, receive) = futures::channel::oneshot::channel();
        let send = std::sync::Mutex::new(Some(send));
        let callback = RcBlock::new(move |error: *mut AnyObject| {
            if let Some(send) = send.lock().unwrap().take() {
                let _ = send.send(if error.is_null() {
                    DeliveryOutcome::Accepted
                } else {
                    DeliveryOutcome::Failed
                });
            }
        });
        // SAFETY: these framework classes and selectors are the same API GPUI's macOS adapter
        // uses. Retained results keep content and request alive through submission.
        unsafe {
            let Some(content_class) = AnyClass::get(c"UNMutableNotificationContent") else {
                return DeliveryOutcome::Unavailable;
            };
            let Some(request_class) = AnyClass::get(c"UNNotificationRequest") else {
                return DeliveryOutcome::Unavailable;
            };
            let content: Retained<AnyObject> = msg_send![content_class, new];
            let _: () = msg_send![&*content, setTitle: &*NSString::from_str(&notification.title)];
            let _: () = msg_send![&*content, setBody: &*NSString::from_str(&notification.body)];
            let request: Retained<AnyObject> = msg_send![request_class, requestWithIdentifier: &*NSString::from_str(&notification.tag), content: &*content, trigger: std::ptr::null::<AnyObject>()];
            let _: () = msg_send![&*center, addNotificationRequest: &*request, withCompletionHandler: &*callback];
        }
        receive.await.unwrap_or(DeliveryOutcome::Failed)
    }

    fn center() -> Option<Retained<AnyObject>> {
        // SAFETY: the bundle guard matches GPUI: calling the notification center from an
        // unbundled dev binary raises an Objective-C exception instead of returning an error.
        unsafe {
            let bundle: Retained<AnyObject> = msg_send![AnyClass::get(c"NSBundle")?, mainBundle];
            let identifier: Option<Retained<NSString>> = msg_send![&*bundle, bundleIdentifier];
            identifier?;
            Some(msg_send![AnyClass::get(c"UNUserNotificationCenter")?, currentNotificationCenter])
        }
    }
}
