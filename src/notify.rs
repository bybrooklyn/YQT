// SPDX-License-Identifier: GPL-3.0-only
//! Desktop notifications, for errors only: something went wrong while the window was not being looked at (in the
//! background, minimised, or closed to the tray). Track changes and finished downloads are never announced.
//!
//! Sent over the desktop's own notification service (`org.freedesktop.Notifications`), so it needs no KDE library
//! and works on any Linux desktop. Elsewhere there is no such service and the in-window message stays.

/// Should this error go to the desktop instead of the window? Only while nobody is looking at the window, only
/// when the user has not switched it off, and only where there is a notification service to send it to.
pub fn goes_to_desktop(window_active: bool, enabled: bool) -> bool {
    cfg!(target_os = "linux") && enabled && !window_active
}

/// Show (or replace) the error notification. Returns at once; a desktop without a notification service is logged
/// and otherwise ignored.
pub fn error(summary: &str, body: &str) {
    // (a message can quote YouTube's reply: nothing secret may reach the notification history)
    let (summary, body) = (crate::logging::redact(summary), crate::logging::redact(body));
    #[cfg(target_os = "linux")]
    crate::state::st().rt.spawn(async move {
        if let Err(e) = linux::send(&summary, &body).await {
            tracing::info!("the error could not be shown as a desktop notification: {e}");
        }
    });
    #[cfg(not(target_os = "linux"))]
    let _ = (summary, body);
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};
    use zbus::zvariant::Value;

    /// The notification shown last: the next error replaces it instead of piling up beside it.
    static LAST: AtomicU32 = AtomicU32::new(0);

    pub async fn send(summary: &str, body: &str) -> zbus::Result<()> {
        let conn = zbus::Connection::session().await?;
        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        hints.insert("desktop-entry", Value::from("dev.bybrooklyn.yqt"));
        let actions: Vec<&str> = Vec::new();
        let reply = conn
            .call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                // app name, id to replace, icon, summary, body, actions, hints, timeout (-1: the desktop's default)
                &("YQT", LAST.load(Ordering::Relaxed), "yqt", summary, body, actions, hints, -1i32),
            )
            .await?;
        LAST.store(reply.body().deserialize::<u32>()?, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::goes_to_desktop;

    #[test]
    fn errors_reach_the_desktop_only_while_the_window_is_not_looked_at() {
        assert!(!goes_to_desktop(true, true), "the window shows it itself");
        assert!(!goes_to_desktop(false, false), "switched off in Settings");
        assert_eq!(goes_to_desktop(false, true), cfg!(target_os = "linux"));
    }

    /// The desktop's notification service takes the message (one real notification appears).
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore]
    async fn live_the_desktop_accepts_an_error_notification() {
        super::linux::send("YQT test", "Error notifications work. This one was sent by a test.").await.expect("the notification service answers");
    }
}
