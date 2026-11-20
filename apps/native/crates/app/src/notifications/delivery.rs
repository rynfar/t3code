//! Platform delivery acceptance, the equivalent of the web's permission check and successful
//! `new Notification`. GPUI owns the response delegate, but its posting API has no result.

use gpui::{App, SystemNotification};
use t3_core::notifications::DeliveryOutcome;

pub async fn authorize() -> DeliveryOutcome {
    #[cfg(target_os = "macos")]
    {
        macos::authorize().await
    }
    #[cfg(target_os = "linux")]
    {
        DeliveryOutcome::Accepted
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        DeliveryOutcome::Unconfirmed
    }
}

pub fn post(
    notification: SystemNotification,
    runtime: tokio::runtime::Handle,
    cx: &App,
) -> impl Future<Output = DeliveryOutcome> + use<> {
    #[cfg(target_os = "macos")]
    let result = {
        let _ = (runtime, cx);
        macos::post(notification)
    };
    #[cfg(target_os = "linux")]
    let result = {
        let _ = cx;
        linux::post(notification, runtime)
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let result = {
        let _ = runtime;
        cx.show_system_notification(notification);
        std::future::ready(DeliveryOutcome::Unconfirmed)
    };
    result
}

/// Cross-platform freedesktop `notify-send` argument builder and exit-status mapper, compiled on
/// Linux and under `#[cfg(test)]` so unit tests verify the Linux delivery path on every host OS.
#[cfg(any(target_os = "linux", test))]
mod freedesktop {
    use std::io;
    use std::process::ExitStatus;
    use t3_core::notifications::DeliveryOutcome;

    pub(super) fn build_notify_send_args(title: &str, body: &str, tag: &str) -> Vec<String> {
        let mut args = vec![
            "--app-name".to_owned(),
            "T3 Code".to_owned(),
        ];
        if !tag.is_empty() {
            args.push("-h".to_owned());
            args.push(format!("string:x-canonical-private-synchronous:{tag}"));
        }
        args.push("--".to_owned());
        args.push(title.to_owned());
        if !body.is_empty() {
            args.push(body.to_owned());
        }
        args
    }

    pub(super) fn map_notify_send_exit_code(code: Option<i32>) -> DeliveryOutcome {
        match code {
            Some(0) => DeliveryOutcome::Accepted,
            _ => DeliveryOutcome::Failed,
        }
    }

    pub(super) fn map_notify_send_result(result: io::Result<ExitStatus>) -> DeliveryOutcome {
        match result {
            Ok(status) => map_notify_send_exit_code(status.code()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => DeliveryOutcome::Unavailable,
            Err(_) => DeliveryOutcome::Failed,
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use gpui::SystemNotification;
    use t3_core::notifications::DeliveryOutcome;

    pub async fn post(
        notification: SystemNotification,
        runtime: tokio::runtime::Handle,
    ) -> DeliveryOutcome {
        #[cfg(test)]
        {
            let _ = (notification, runtime);
            DeliveryOutcome::Unconfirmed
        }
        #[cfg(not(test))]
        {
            let args = super::freedesktop::build_notify_send_args(
                notification.title.as_ref(),
                notification.body.as_ref(),
                notification.tag.as_ref(),
            );
            let join = runtime.spawn(async move {
                let result = tokio::process::Command::new("notify-send")
                    .args(&args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await;
                super::freedesktop::map_notify_send_result(result)
            });
            join.await.unwrap_or(DeliveryOutcome::Failed)
        }
    }
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

#[cfg(test)]
mod tests {
    use std::io;
    use t3_core::notifications::DeliveryOutcome;

    use super::freedesktop::{build_notify_send_args, map_notify_send_exit_code, map_notify_send_result};

    #[test]
    fn notify_send_args_include_app_name_synchronous_tag_and_option_terminator() {
        assert_eq!(
            build_notify_send_args("Thread completed", "Fix notifications", "0123456789abcdef"),
            vec![
                "--app-name",
                "T3 Code",
                "-h",
                "string:x-canonical-private-synchronous:0123456789abcdef",
                "--",
                "Thread completed",
                "Fix notifications",
            ]
        );
        // Leading dashes in title or body must follow `--` so notify-send cannot treat them as flags.
        assert_eq!(
            build_notify_send_args("-u critical", "--expire-time=0", "tag-1"),
            vec![
                "--app-name",
                "T3 Code",
                "-h",
                "string:x-canonical-private-synchronous:tag-1",
                "--",
                "-u critical",
                "--expire-time=0",
            ]
        );
        // Empty tag omits the replacement hint; empty body omits the trailing body argument.
        assert_eq!(
            build_notify_send_args("Approval needed", "", ""),
            vec!["--app-name", "T3 Code", "--", "Approval needed"]
        );
    }

    #[test]
    fn notify_send_exit_status_and_io_errors_map_to_delivery_outcomes() {
        assert_eq!(map_notify_send_exit_code(Some(0)), DeliveryOutcome::Accepted);
        assert_eq!(map_notify_send_exit_code(Some(1)), DeliveryOutcome::Failed);
        assert_eq!(map_notify_send_exit_code(Some(127)), DeliveryOutcome::Failed);
        assert_eq!(map_notify_send_exit_code(None), DeliveryOutcome::Failed);

        assert_eq!(
            map_notify_send_result(Err(io::Error::new(io::ErrorKind::NotFound, "notify-send missing"))),
            DeliveryOutcome::Unavailable
        );
        assert_eq!(
            map_notify_send_result(Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))),
            DeliveryOutcome::Failed
        );
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(
                map_notify_send_result(Ok(std::process::ExitStatus::from_raw(0))),
                DeliveryOutcome::Accepted
            );
            assert_eq!(
                map_notify_send_result(Ok(std::process::ExitStatus::from_raw(1 << 8))),
                DeliveryOutcome::Failed
            );
        }
    }
}
