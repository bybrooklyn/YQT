// SPDX-License-Identifier: GPL-3.0-only
//! One running YQT per session: starting it again brings the window that is already there to the front instead of
//! opening a second player (two would fight over the audio device, the session file and the media keys).
//!
//! Done the way desktop apps do it on Linux: the first instance owns the bus name `dev.bybrooklyn.yqt` and serves
//! `org.freedesktop.Application`; a later one finds the name taken, calls `Activate` on it and exits.
//! `YQT_MULTI_INSTANCE=1` switches the check off (development and headless test runs).
use std::sync::OnceLock;
use std::time::Duration;

const NAME: &str = "dev.bybrooklyn.yqt";
const PATH: &str = "/dev/bybrooklyn/yqt";
/// Never let a slow or absent bus hold up the start.
const PATIENCE: Duration = Duration::from_secs(2);

/// What starting up found.
#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    /// This is the one instance (or there is no session bus to tell): carry on.
    First,
    /// Another instance is running and has been asked to show its window: exit.
    AlreadyRunning,
}

/// Should the check run at all? Not when the user asked for a second instance.
pub fn wanted(multi_instance_var: Option<&str>) -> bool {
    !matches!(multi_instance_var, Some(v) if !v.is_empty() && v != "0")
}

type Raise = Box<dyn Fn() + Send + Sync>;
static RAISE: OnceLock<Raise> = OnceLock::new();
/// An `Activate` that arrived before the window could be raised (it is still starting): shown as soon as it can.
static PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Keeps the bus name for as long as the app runs.
static CONN: OnceLock<zbus::Connection> = OnceLock::new();

/// How a later instance's `Activate` reaches the window. Called once the window exists.
pub fn on_activate(raise: impl Fn() + Send + Sync + 'static) {
    if RAISE.set(Box::new(raise)).is_ok() && PENDING.swap(false, std::sync::atomic::Ordering::Relaxed) {
        activate();
    }
}

fn activate() {
    match RAISE.get() {
        Some(raise) => raise(),
        None => PENDING.store(true, std::sync::atomic::Ordering::Relaxed),
    }
}

struct App;

#[zbus::interface(name = "org.freedesktop.Application")]
impl App {
    fn activate(&self, _platform_data: std::collections::HashMap<String, zbus::zvariant::OwnedValue>) {
        activate();
    }
    /// Nothing to open (the app takes no files), so this shows the window like `Activate`.
    fn open(&self, _uris: Vec<String>, _platform_data: std::collections::HashMap<String, zbus::zvariant::OwnedValue>) {
        activate();
    }
    fn activate_action(
        &self,
        _name: String,
        _parameter: Vec<zbus::zvariant::OwnedValue>,
        _platform_data: std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
    ) {
        activate();
    }
}

/// Claim the name, or hand over to the instance that has it. Call before the window is created.
pub fn claim() -> Claim {
    if !wanted(std::env::var("YQT_MULTI_INSTANCE").ok().as_deref()) {
        return Claim::First;
    }
    let found = crate::state::st().rt.block_on(async { tokio::time::timeout(PATIENCE, try_claim()).await });
    match found {
        Ok(Ok(claim)) => claim,
        // no session bus, or one that does not answer: nothing to coordinate with
        Ok(Err(e)) => {
            tracing::info!("single-instance check skipped: {e}");
            Claim::First
        }
        Err(_) => {
            tracing::info!("single-instance check skipped: the session bus did not answer in time");
            Claim::First
        }
    }
}

async fn try_claim() -> zbus::Result<Claim> {
    use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};
    let conn = zbus::connection::Builder::session()?.serve_at(PATH, App)?.build().await?;
    let bus = DBusProxy::new(&conn).await?;
    let name = zbus::names::WellKnownName::try_from(NAME)?;
    match bus.request_name(name, RequestNameFlags::DoNotQueue.into()).await? {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => {
            let _ = CONN.set(conn);
            Ok(Claim::First)
        }
        _ => {
            let no_data: std::collections::HashMap<String, zbus::zvariant::OwnedValue> = Default::default();
            conn.call_method(Some(NAME), PATH, Some("org.freedesktop.Application"), "Activate", &(no_data,)).await?;
            Ok(Claim::AlreadyRunning)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::wanted;

    #[test]
    fn the_check_can_be_switched_off_for_a_second_instance() {
        assert!(wanted(None));
        assert!(wanted(Some("")));
        assert!(wanted(Some("0")));
        assert!(!wanted(Some("1")));
    }
}
