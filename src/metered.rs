// SPDX-License-Identifier: GPL-3.0-only
//! "Pause downloads on a metered connection / low battery": the adapters that answer those two questions
//! and the pure rule that turns the answers into a pause reason.
//!
//! * [`Metered`]: Linux asks NetworkManager over D-Bus (`org.freedesktop.NetworkManager` property
//!   `Metered`, via `zbus`); everywhere else, and when NetworkManager is absent, the answer is
//!   [`MeteredState::Unknown`], which never pauses, and [`Metered::note`] says so for the UI.
//! * [`BatteryProbe`]: the pure-Rust `battery` crate (reads sysfs / IOKit / WinAPI; no system library).
//!
//! Everything is behind traits so the tests use fakes.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeteredState {
    Metered,
    Unmetered,
    /// Could not be determined: treated as "not metered" (never pauses).
    Unknown,
}

pub trait Metered: Send + Sync {
    fn state(&self) -> MeteredState;
    /// A visible explanation when this adapter cannot tell (None = it works).
    fn note(&self) -> Option<&'static str>;
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BatteryStatus {
    /// 0..=100
    pub percent: f32,
    /// True when running on battery (not plugged in / charging).
    pub discharging: bool,
}

pub trait BatteryProbe: Send + Sync {
    /// `None` = no battery (desktop) or unreadable: never pauses.
    fn status(&self) -> Option<BatteryStatus>;
}

/// Why downloads are held back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    User,
    Offline,
    Metered,
    LowBattery,
}

impl PauseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            PauseReason::User => "user",
            PauseReason::Offline => "offline",
            PauseReason::Metered => "metered",
            PauseReason::LowBattery => "low_battery",
        }
    }
}

/// The pause rule. `user` and `offline` always win; metered / low battery only when their setting is on.
pub fn pause_reason(
    user_paused: bool,
    offline: bool,
    pause_on_metered: bool,
    metered: MeteredState,
    pause_on_low_battery: bool,
    low_battery_percent: u32,
    battery: Option<BatteryStatus>,
) -> Option<PauseReason> {
    if user_paused {
        return Some(PauseReason::User);
    }
    if offline {
        return Some(PauseReason::Offline);
    }
    if pause_on_metered && metered == MeteredState::Metered {
        return Some(PauseReason::Metered);
    }
    if pause_on_low_battery {
        if let Some(b) = battery {
            if b.discharging && b.percent <= low_battery_percent as f32 {
                return Some(PauseReason::LowBattery);
            }
        }
    }
    None
}

/// NetworkManager's `NMMetered` enum: 1 = yes, 3 = guess-yes are metered; 2 = no, 4 = guess-no unmetered; 0 = unknown.
pub fn from_nm_metered(v: u32) -> MeteredState {
    match v {
        1 | 3 => MeteredState::Metered,
        2 | 4 => MeteredState::Unmetered,
        _ => MeteredState::Unknown,
    }
}

// ---- null implementation ----------------------------------------------------------------------

#[cfg_attr(target_os = "linux", allow(dead_code))] // Linux uses NetworkManager; the other systems use this
pub struct NullMetered(pub &'static str);

impl Metered for NullMetered {
    fn state(&self) -> MeteredState {
        MeteredState::Unknown
    }
    fn note(&self) -> Option<&'static str> {
        Some(self.0)
    }
}

// ---- Linux: NetworkManager over D-Bus ---------------------------------------------------------

#[cfg(target_os = "linux")]
pub struct NetworkManager;

#[cfg(target_os = "linux")]
impl Metered for NetworkManager {
    fn state(&self) -> MeteredState {
        // zbus is async: run it on its own short-lived thread with a private runtime (like platform/linux.rs),
        // so this is safe to call from any thread, including inside the app's tokio runtime.
        let work = std::thread::Builder::new().name("ytm-metered".into()).spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
            rt.block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(3), async {
                    let conn = zbus::Connection::system().await.ok()?;
                    let proxy = zbus::Proxy::new(
                        &conn,
                        "org.freedesktop.NetworkManager",
                        "/org/freedesktop/NetworkManager",
                        "org.freedesktop.NetworkManager",
                    )
                    .await
                    .ok()?;
                    proxy.get_property::<u32>("Metered").await.ok()
                })
                .await
                .ok()
                .flatten()
            })
        });
        work.ok().and_then(|h| h.join().ok()).flatten().map_or(MeteredState::Unknown, from_nm_metered)
    }

    fn note(&self) -> Option<&'static str> {
        None
    }
}

/// The best adapter for this system.
pub fn system_metered() -> Box<dyn Metered> {
    #[cfg(target_os = "linux")]
    {
        Box::new(NetworkManager)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Box::new(NullMetered(
            "Metered-connection detection is not available on this system, so downloads will not pause on metered networks.",
        ))
    }
}

// ---- batteries ---------------------------------------------------------------------------------

/// Linux: `/sys/class/power_supply/*` (plain files, no library). Other systems: no battery information, which
/// never pauses downloads. (The `battery` crate was tried and dropped: it pulls in `nix 0.19`, which `cargo deny`
/// rejects for RUSTSEC-2021-0119.)
pub struct SystemBattery;

/// One power-supply entry → status, if it is a battery. `kind` = the `type` file, `capacity` and `status` the
/// files of the same names.
pub fn parse_power_supply(kind: &str, capacity: &str, status: &str) -> Option<BatteryStatus> {
    if !kind.trim().eq_ignore_ascii_case("battery") {
        return None;
    }
    let percent: f32 = capacity.trim().parse().ok().filter(|p| (0.0..=100.0).contains(p))?;
    Some(BatteryStatus { percent, discharging: status.trim().eq_ignore_ascii_case("discharging") })
}

impl BatteryProbe for SystemBattery {
    fn status(&self) -> Option<BatteryStatus> {
        #[cfg(target_os = "linux")]
        {
            let rd = std::fs::read_dir("/sys/class/power_supply").ok()?;
            let mut names: Vec<_> = rd.flatten().map(|e| e.path()).collect();
            names.sort();
            names.into_iter().find_map(|p| {
                let read = |f: &str| std::fs::read_to_string(p.join(f)).ok();
                parse_power_supply(&read("type")?, &read("capacity")?, &read("status")?)
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOW: Option<BatteryStatus> = Some(BatteryStatus { percent: 10.0, discharging: true });
    const LOW_PLUGGED: Option<BatteryStatus> = Some(BatteryStatus { percent: 10.0, discharging: false });
    const FULL: Option<BatteryStatus> = Some(BatteryStatus { percent: 90.0, discharging: true });

    fn rule(user: bool, offline: bool, pm: bool, m: MeteredState, pb: bool, b: Option<BatteryStatus>) -> Option<PauseReason> {
        pause_reason(user, offline, pm, m, pb, 20, b)
    }

    #[test]
    fn pause_table() {
        use MeteredState::*;
        use PauseReason as R;
        assert_eq!(rule(true, true, true, Metered, true, LOW), Some(R::User), "user wins");
        assert_eq!(rule(false, true, true, Metered, true, LOW), Some(R::Offline));
        assert_eq!(rule(false, false, true, Metered, true, LOW), Some(R::Metered));
        assert_eq!(rule(false, false, false, Metered, true, LOW), Some(R::LowBattery), "metered setting off");
        assert_eq!(rule(false, false, false, Metered, false, LOW), None, "both settings off");
        assert_eq!(rule(false, false, true, Unmetered, true, LOW_PLUGGED), None, "charging is fine");
        assert_eq!(rule(false, false, true, Unmetered, true, FULL), None);
        assert_eq!(rule(false, false, true, Unknown, true, None), None, "unknown / no battery never pauses");
        assert_eq!(
            rule(false, false, true, Unknown, true, Some(BatteryStatus { percent: 20.0, discharging: true })),
            Some(R::LowBattery),
            "threshold is inclusive"
        );
    }

    #[test]
    fn power_supply_files_parse() {
        let b = |p, d| Some(BatteryStatus { percent: p, discharging: d });
        assert_eq!(parse_power_supply("Battery\n", "37\n", "Discharging\n"), b(37.0, true));
        assert_eq!(parse_power_supply("Battery", "100", "Full"), b(100.0, false));
        assert_eq!(parse_power_supply("Battery", "5", "Charging"), b(5.0, false));
        assert_eq!(parse_power_supply("Battery", "5", "Not charging"), b(5.0, false));
        assert_eq!(parse_power_supply("Mains", "1", "Unknown"), None);
        assert_eq!(parse_power_supply("USB", "50", "Discharging"), None);
        for bad in ["", "abc", "-1", "101", "NaN"] {
            assert_eq!(parse_power_supply("Battery", bad, "Discharging"), None, "{bad:?}");
        }
    }

    #[test]
    fn network_manager_values() {
        use MeteredState::*;
        assert_eq!([0, 1, 2, 3, 4, 99].map(from_nm_metered), [Unknown, Metered, Unmetered, Metered, Unmetered, Unknown]);
    }

    #[test]
    fn the_null_adapter_never_pauses_and_says_why() {
        let n = NullMetered("not available");
        assert_eq!(n.state(), MeteredState::Unknown);
        assert_eq!(n.note(), Some("not available"));
    }

    struct Fake(MeteredState);
    impl Metered for Fake {
        fn state(&self) -> MeteredState {
            self.0
        }
        fn note(&self) -> Option<&'static str> {
            None
        }
    }
    struct FakeBattery(Option<BatteryStatus>);
    impl BatteryProbe for FakeBattery {
        fn status(&self) -> Option<BatteryStatus> {
            self.0
        }
    }

    #[test]
    fn fakes_plug_into_the_rule() {
        let (m, b): (Box<dyn Metered>, Box<dyn BatteryProbe>) = (Box::new(Fake(MeteredState::Metered)), Box::new(FakeBattery(LOW)));
        assert_eq!(pause_reason(false, false, true, m.state(), true, 20, b.status()), Some(PauseReason::Metered));
    }
}
