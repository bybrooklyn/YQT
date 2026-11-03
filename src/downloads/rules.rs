// SPDX-License-Identifier: GPL-3.0-only
//! The smart-download planner: given what is on disk, what the user listens to and the rules, decide what to
//! download and what to remove. Pure (no I/O, no clock) so it is table-tested.
use std::collections::HashSet;

/// Guess for the size of a song we have not downloaded yet (≈ 4 minutes at 128 kbit/s).
pub const DEFAULT_SONG_BYTES: u64 = 4_000_000;

/// A song that is on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub id: String,
    pub size: u64,
    /// Unix seconds.
    pub added: i64,
    pub last_played: Option<i64>,
    /// Pinned directly or through a "keep offline" collection: exempt from every automatic removal.
    pub pinned: bool,
}

impl Stored {
    /// "How recently did the user care about this": last play, else when it was added.
    fn touched(&self) -> i64 {
        self.last_played.unwrap_or(self.added)
    }
}

/// The rules (a plain copy of the relevant `[downloads]` settings).
#[derive(Debug, Clone, PartialEq)]
pub struct Rules {
    pub smart_enabled: bool,
    pub keep_liked: bool,
    pub recent_n: usize,
    pub queue_ahead_n: usize,
    pub cap_bytes: u64,
    /// 0 = never.
    pub remove_unplayed_days: u32,
    /// Never let the disk get below this much free space because of a download.
    pub free_reserve_bytes: u64,
}

impl Rules {
    pub fn from_config(d: &crate::config::Downloads) -> Rules {
        Rules {
            smart_enabled: d.smart_enabled,
            keep_liked: d.keep_liked,
            recent_n: d.recent_n as usize,
            queue_ahead_n: d.queue_ahead_n as usize,
            cap_bytes: cap_bytes(d.cap_gb),
            remove_unplayed_days: d.remove_unplayed_days,
            free_reserve_bytes: 512 * 1024 * 1024,
        }
    }
}

/// `cap_gb` → bytes (decimal GB, like the storage bar says); nonsense (negative, NaN) = 0, which means "nothing is kept".
pub fn cap_bytes(cap_gb: f64) -> u64 {
    if cap_gb.is_finite() && cap_gb > 0.0 {
        (cap_gb * 1e9) as u64
    } else {
        0
    }
}

/// What the app knows right now.
#[derive(Debug, Clone, Default)]
pub struct PlanState {
    pub stored: Vec<Stored>,
    /// Ids being downloaded or queued already.
    pub in_flight: HashSet<String>,
    /// Liked songs, newest like first.
    pub liked: Vec<String>,
    /// Recently played, most recent first.
    pub recent: Vec<String>,
    /// The next queue entries, in play order.
    pub queue_ahead: Vec<String>,
    /// Songs of "keep offline" collections that are not necessarily downloaded yet.
    pub pinned_wanted: Vec<String>,
    /// Free space on the downloads volume, if known.
    pub free_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Pinned,
    QueueAhead,
    Recent,
    Liked,
    /// Over the storage cap.
    Cap,
    /// Not played for `remove_unplayed_days`.
    Unplayed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Download { id: String, why: Why },
    Evict { id: String, why: Why },
}

/// Free-space check: can `need` more bytes be written and still leave `reserve` free? Unknown free space = yes.
pub fn fits_free_space(free: Option<u64>, need: u64, reserve: u64) -> bool {
    free.is_none_or(|f| f >= need.saturating_add(reserve))
}

/// Ids the rules want kept offline, highest priority first, de-duplicated.
pub fn wanted(state: &PlanState, rules: &Rules) -> Vec<(String, Why)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut add = |ids: &[String], why: Why, limit: usize| {
        for id in ids.iter().take(limit) {
            if seen.insert(id.clone()) {
                out.push((id.clone(), why));
            }
        }
    };
    // collections the user pinned are explicit, so they work with smart downloads off as well
    add(&state.pinned_wanted, Why::Pinned, usize::MAX);
    if rules.smart_enabled {
        add(&state.queue_ahead, Why::QueueAhead, rules.queue_ahead_n);
        add(&state.recent, Why::Recent, rules.recent_n);
        if rules.keep_liked {
            add(&state.liked, Why::Liked, usize::MAX);
        }
    }
    out
}

/// Order in which unpinned songs are removed: songs the rules do not want first, then least recently
/// played (never-played ones by when they were added), oldest first.
pub fn eviction_order<'a>(stored: &'a [Stored], protected: &HashSet<&str>) -> Vec<&'a Stored> {
    let mut v: Vec<&Stored> = stored.iter().filter(|s| !s.pinned).collect();
    v.sort_by(|a, b| {
        let key = |s: &Stored| (protected.contains(s.id.as_str()), s.touched(), s.added, s.id.clone());
        key(a).cmp(&key(b))
    });
    v
}

/// Songs whose last play (or add) is older than `days`; pinned ones never.
pub fn unplayed_victims(stored: &[Stored], days: u32, now: i64) -> Vec<&Stored> {
    if days == 0 {
        return Vec::new();
    }
    let cutoff = now - i64::from(days) * 86_400;
    stored.iter().filter(|s| !s.pinned && s.touched() < cutoff).collect()
}

/// The plan: removals first (unplayed, then cap), then downloads that fit the cap and the free space.
pub fn plan(state: &PlanState, rules: &Rules, now: i64) -> Vec<Action> {
    let mut actions = Vec::new();
    let wanted = wanted(state, rules);
    let protected: HashSet<&str> = wanted.iter().map(|(id, _)| id.as_str()).collect();
    let mut removed: HashSet<&str> = HashSet::new();
    let mut used: u64 = state.stored.iter().map(|s| s.size).sum();

    // 1. not played for too long (wanted songs are kept: the rules just asked for them)
    for s in unplayed_victims(&state.stored, rules.remove_unplayed_days, now) {
        if !protected.contains(s.id.as_str()) {
            actions.push(Action::Evict { id: s.id.clone(), why: Why::Unplayed });
            removed.insert(&s.id);
            used = used.saturating_sub(s.size);
        }
    }
    // 2. over the cap
    if used > rules.cap_bytes {
        for s in eviction_order(&state.stored, &protected) {
            if used <= rules.cap_bytes {
                break;
            }
            if removed.insert(&s.id) {
                actions.push(Action::Evict { id: s.id.clone(), why: Why::Cap });
                used = used.saturating_sub(s.size);
            }
        }
    }
    // 3. what to fetch: in priority order, as long as it fits
    let have: HashSet<&str> = state.stored.iter().filter(|s| !removed.contains(s.id.as_str())).map(|s| s.id.as_str()).collect();
    let avg = if state.stored.is_empty() { DEFAULT_SONG_BYTES } else { (used / state.stored.len() as u64).max(1_000_000) };
    let mut free = state.free_bytes;
    for (id, why) in wanted {
        if have.contains(id.as_str()) || state.in_flight.contains(&id) {
            continue;
        }
        if used.saturating_add(avg) > rules.cap_bytes || !fits_free_space(free, avg, rules.free_reserve_bytes) {
            // Pinned collections are explicit: skip what does not fit, but keep looking is pointless once
            // the budget is spent, so stop here for everything.
            break;
        }
        used += avg;
        free = free.map(|f| f.saturating_sub(avg));
        actions.push(Action::Download { id, why });
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;
    const NOW: i64 = 100 * DAY;

    fn s(id: &str, size: u64, added_days_ago: i64, played_days_ago: Option<i64>, pinned: bool) -> Stored {
        Stored { id: id.into(), size, added: NOW - added_days_ago * DAY, last_played: played_days_ago.map(|d| NOW - d * DAY), pinned }
    }
    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }
    fn rules() -> Rules {
        Rules {
            smart_enabled: true,
            keep_liked: true,
            recent_n: 2,
            queue_ahead_n: 2,
            cap_bytes: 100_000_000,
            remove_unplayed_days: 0,
            free_reserve_bytes: 0,
        }
    }
    fn downloads(a: &[Action]) -> Vec<&str> {
        a.iter().filter_map(|x| if let Action::Download { id, .. } = x { Some(id.as_str()) } else { None }).collect()
    }
    fn evictions(a: &[Action]) -> Vec<&str> {
        a.iter().filter_map(|x| if let Action::Evict { id, .. } = x { Some(id.as_str()) } else { None }).collect()
    }

    #[test]
    fn caps_convert_to_bytes() {
        assert_eq!(cap_bytes(4.0), 4_000_000_000);
        assert_eq!(cap_bytes(0.5), 500_000_000);
        assert_eq!(cap_bytes(1000.0), 1_000_000_000_000, "no hard maximum");
        for bad in [0.0, -3.0, f64::NAN, f64::NEG_INFINITY] {
            assert_eq!(cap_bytes(bad), 0, "{bad}");
        }
    }

    #[test]
    fn default_config_gives_the_decided_rules() {
        let r = Rules::from_config(&crate::config::Config::default().downloads);
        assert!(!r.smart_enabled);
        assert_eq!(r.cap_bytes, 4_000_000_000);
    }

    #[test]
    fn smart_off_downloads_only_pinned_collections() {
        let mut r = rules();
        r.smart_enabled = false;
        let st = PlanState { liked: ids(&["l1"]), recent: ids(&["r1"]), pinned_wanted: ids(&["p1", "p2"]), ..Default::default() };
        assert_eq!(downloads(&plan(&st, &r, NOW)), ["p1", "p2"]);
    }

    #[test]
    fn smart_on_orders_pinned_queue_recent_liked_and_applies_limits() {
        let st = PlanState {
            queue_ahead: ids(&["q1", "q2", "q3"]),
            recent: ids(&["r1", "q1", "r2", "r3"]),
            liked: ids(&["l1", "r1"]),
            pinned_wanted: ids(&["p1"]),
            ..Default::default()
        };
        // queue-ahead limit 2, recent limit 2 (q1 is a duplicate and consumes its slot), liked all
        assert_eq!(downloads(&plan(&st, &rules(), NOW)), ["p1", "q1", "q2", "r1", "l1"]);
        let mut r = rules();
        r.keep_liked = false;
        assert_eq!(downloads(&plan(&st, &r, NOW)), ["p1", "q1", "q2", "r1"]);
    }

    #[test]
    fn already_stored_or_in_flight_songs_are_skipped() {
        let st = PlanState {
            stored: vec![s("q1", 1_000_000, 1, None, false)],
            in_flight: ["q2".to_owned()].into(),
            queue_ahead: ids(&["q1", "q2", "q3"]),
            ..Default::default()
        };
        assert_eq!(downloads(&plan(&st, &rules(), NOW)), Vec::<&str>::new(), "queue limit is 2: q3 is not wanted");
        let mut r = rules();
        r.queue_ahead_n = 3;
        assert_eq!(downloads(&plan(&st, &r, NOW)), ["q3"]);
    }

    #[test]
    fn eviction_is_oldest_unpinned_least_recently_played_first() {
        let stored = vec![
            s("pinned_old", 10, 90, None, true),
            s("played_recently", 10, 80, Some(1), false),
            s("never_played_old", 10, 60, None, false),
            s("played_long_ago", 10, 70, Some(30), false),
            s("never_played_new", 10, 2, None, false),
        ];
        let order: Vec<&str> = eviction_order(&stored, &HashSet::new()).iter().map(|x| x.id.as_str()).collect();
        assert_eq!(order, ["never_played_old", "played_long_ago", "never_played_new", "played_recently"]);
        // wanted songs go last even if old
        let protected: HashSet<&str> = ["never_played_old"].into();
        let order: Vec<&str> = eviction_order(&stored, &protected).iter().map(|x| x.id.as_str()).collect();
        assert_eq!(order.last(), Some(&"never_played_old"));
    }

    #[test]
    fn over_cap_evicts_until_under_and_stops() {
        let stored = vec![
            s("a", 40_000_000, 50, Some(40), false),
            s("b", 40_000_000, 40, Some(20), false),
            s("c", 40_000_000, 30, Some(1), false),
            s("pin", 40_000_000, 90, None, true),
        ];
        let st = PlanState { stored, ..Default::default() };
        let mut r = rules();
        r.cap_bytes = 100_000_000; // 160 MB stored: remove 60 MB+ → two songs
        let a = plan(&st, &r, NOW);
        assert_eq!(evictions(&a), ["a", "b"]);
        assert!(a.iter().all(|x| matches!(x, Action::Evict { why: Why::Cap, .. })));
    }

    #[test]
    fn pinned_songs_survive_even_when_nothing_else_can_be_removed() {
        let st = PlanState { stored: vec![s("pin", 500_000_000, 10, None, true)], ..Default::default() };
        assert!(evictions(&plan(&st, &rules(), NOW)).is_empty());
    }

    #[test]
    fn under_cap_removes_nothing() {
        let st = PlanState { stored: vec![s("a", 10_000_000, 5, None, false)], ..Default::default() };
        assert!(plan(&st, &rules(), NOW).is_empty());
    }

    #[test]
    fn unplayed_removal_table() {
        let stored = vec![
            s("fresh", 1, 5, None, false),
            s("stale_never_played", 1, 40, None, false),
            s("stale_played_long_ago", 1, 100, Some(35), false),
            s("played_lately", 1, 100, Some(3), false),
            s("pinned_stale", 1, 100, None, true),
        ];
        let v = |days| unplayed_victims(&stored, days, NOW).iter().map(|x| x.id.as_str()).collect::<Vec<_>>();
        assert!(v(0).is_empty(), "0 = never");
        assert_eq!(v(30), ["stale_never_played", "stale_played_long_ago"]);
        assert!(v(60).is_empty());
        let mut r = rules();
        r.remove_unplayed_days = 30;
        let st = PlanState { stored, ..Default::default() };
        let a = plan(&st, &r, NOW);
        assert_eq!(evictions(&a), ["stale_never_played", "stale_played_long_ago"]);
        assert!(a.iter().all(|x| matches!(x, Action::Evict { why: Why::Unplayed, .. })));
    }

    #[test]
    fn a_stale_song_the_rules_want_again_is_kept() {
        let st = PlanState { stored: vec![s("liked_old", 1, 100, None, false)], liked: ids(&["liked_old"]), ..Default::default() };
        let mut r = rules();
        r.remove_unplayed_days = 30;
        assert!(plan(&st, &r, NOW).is_empty());
    }

    #[test]
    fn downloads_respect_the_cap_budget() {
        let st = PlanState { stored: vec![s("x", 96_000_000, 1, None, false)], queue_ahead: ids(&["q1", "q2"]), ..Default::default() };
        let mut r = rules();
        r.cap_bytes = 100_000_000;
        // avg guess is the larger of used/len and 1 MB = 96 MB → nothing else fits
        assert!(downloads(&plan(&st, &r, NOW)).is_empty());
        let st = PlanState { queue_ahead: ids(&["q1", "q2"]), ..Default::default() };
        r.cap_bytes = DEFAULT_SONG_BYTES + 1; // room for exactly one default-size song
        assert_eq!(downloads(&plan(&st, &r, NOW)), ["q1"]);
    }

    #[test]
    fn free_space_check() {
        assert!(fits_free_space(None, u64::MAX, 5));
        assert!(fits_free_space(Some(10), 5, 5));
        assert!(!fits_free_space(Some(9), 5, 5));
        assert!(!fits_free_space(Some(0), 1, 0));
        assert!(fits_free_space(Some(u64::MAX), u64::MAX, 0));
        assert!(!fits_free_space(Some(10), u64::MAX, 1), "saturating add must not wrap around into 'fits'");
    }

    #[test]
    fn a_nearly_full_disk_blocks_new_downloads() {
        let st = PlanState { queue_ahead: ids(&["q1"]), free_bytes: Some(DEFAULT_SONG_BYTES), ..Default::default() };
        let mut r = rules();
        r.free_reserve_bytes = 100_000_000;
        assert!(downloads(&plan(&st, &r, NOW)).is_empty());
        r.free_reserve_bytes = 0;
        assert_eq!(downloads(&plan(&st, &r, NOW)), ["q1"]);
    }

    #[test]
    fn zero_cap_keeps_nothing_and_downloads_nothing() {
        let st = PlanState { stored: vec![s("a", 5, 1, None, false)], queue_ahead: ids(&["q1"]), ..Default::default() };
        let mut r = rules();
        r.cap_bytes = 0;
        let a = plan(&st, &r, NOW);
        assert_eq!(evictions(&a), ["a"]);
        assert!(downloads(&a).is_empty());
    }
}
