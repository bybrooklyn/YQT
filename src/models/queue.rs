// SPDX-License-Identifier: GPL-3.0-only
//! The play queue rules. [`QueueState`] is plain Rust and unit-tested; the `Player` owns one and forwards each
//! [`Change`] to the `QueueModel` (`src/queuemodel.rs`), which keeps a mirror of the rows for QML.

#![allow(dead_code)] // a few accessors are API that is not used here yet

use super::Change;
use serde_json::Value;

/// What removing a row did besides shrinking the list.
#[derive(Debug, Clone, PartialEq)]
pub struct Removal<T> {
    pub change: Change<T>,
    /// The removed row was the one playing: the caller should load `replacement` (if any).
    pub was_current: bool,
    pub replacement: Option<usize>,
}

/// Queue contents, the playing row, and the shuffle "previous" history.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueState<T> {
    items: Vec<T>,
    current: Option<usize>,
    /// Indices visited through shuffle, so "previous" can retrace them.
    history: Vec<usize>,
    /// Counts replacements of the whole queue (see [`epoch`](Self::epoch)).
    epoch: u64,
}

impl<T> Default for QueueState<T> {
    fn default() -> Self {
        Self { items: Vec::new(), current: None, history: Vec::new(), epoch: 0 }
    }
}

impl<T: Clone> QueueState<T> {
    pub fn items(&self) -> &[T] {
        &self.items
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn current(&self) -> Option<usize> {
        self.current
    }
    pub fn get(&self, i: usize) -> Option<&T> {
        self.items.get(i)
    }
    /// Which queue this is: changes whenever the whole queue is replaced or cleared, never when it is only edited.
    /// The queue feed appends the rest of a long list only to the queue it was started for.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn set_current(&mut self, current: Option<usize>) {
        self.current = current.filter(|&i| i < self.items.len());
    }

    /// Replace the whole queue (nothing is playing afterwards until the caller sets a current row).
    pub fn replace(&mut self, items: Vec<T>) -> Change<T> {
        self.epoch += 1;
        self.items = items;
        self.current = None;
        self.history.clear();
        Change::Reset(self.items.clone())
    }

    pub fn clear(&mut self) -> Change<T> {
        self.replace(Vec::new())
    }

    /// Append; indices of everything else stay valid.
    pub fn push(&mut self, item: T) -> Change<T> {
        let at = self.items.len();
        self.items.push(item);
        Change::Insert { at, rows: vec![self.items[at].clone()] }
    }

    /// Append several rows at once (e.g. a fetched radio). Indices of everything else stay valid.
    pub fn extend(&mut self, rows: Vec<T>) -> Change<T> {
        let at = self.items.len();
        self.items.extend(rows.iter().cloned());
        Change::Insert { at, rows }
    }

    /// Put `list` (an album) around the playing row: the queue becomes the list, with the playing row standing
    /// where `same` first finds it, so what came before it in the list is behind it and the rest is ahead. The
    /// playing row itself is kept as it is (it is the one being played, whatever the list calls it). `None`, and
    /// nothing changed, when nothing is playing or the list does not hold the playing row.
    pub fn surround_current(&mut self, mut list: Vec<T>, same: impl Fn(&T, &T) -> bool) -> Option<Change<T>> {
        let playing = self.items.get(self.current?)?.clone();
        let at = list.iter().position(|i| same(i, &playing))?;
        list[at] = playing;
        self.epoch += 1;
        self.items = list;
        self.current = Some(at);
        self.history.clear();
        Some(Change::Reset(self.items.clone()))
    }

    /// Insert right after the playing row (at the start when nothing is playing).
    pub fn insert_next(&mut self, item: T) -> Change<T> {
        let at = self.current.map_or(0, |c| c + 1).min(self.items.len());
        self.insert_at(at, item)
    }

    pub fn insert_at(&mut self, at: usize, item: T) -> Change<T> {
        let at = at.min(self.items.len());
        self.items.insert(at, item.clone());
        if let Some(c) = self.current.as_mut() {
            if at <= *c {
                *c += 1;
            }
        }
        for h in &mut self.history {
            if at <= *h {
                *h += 1;
            }
        }
        Change::Insert { at, rows: vec![item] }
    }

    /// Remove a row. The playing row keeps being tracked; if it is the one removed, `current` becomes `None`
    /// and `replacement` says which row should take over.
    pub fn remove(&mut self, i: usize) -> Option<Removal<T>> {
        if i >= self.items.len() {
            return None;
        }
        self.items.remove(i);
        self.history.retain(|&h| h != i);
        for h in &mut self.history {
            if *h > i {
                *h -= 1;
            }
        }
        let mut was_current = false;
        match self.current {
            Some(c) if i < c => self.current = Some(c - 1),
            Some(c) if i == c => {
                self.current = None;
                was_current = true;
            }
            _ => {}
        }
        let replacement = (was_current && !self.items.is_empty()).then(|| i.min(self.items.len() - 1));
        Some(Removal { change: Change::Remove { at: i, count: 1 }, was_current, replacement })
    }

    /// Move the row at `from` so it ends up at `to`. The playing row stays the playing row.
    pub fn move_item(&mut self, from: usize, to: usize) -> Option<Change<T>> {
        let len = self.items.len();
        if from >= len || to >= len || from == to {
            return None;
        }
        let item = self.items.remove(from);
        self.items.insert(to, item);
        let remap = |i: usize| -> usize {
            if i == from {
                to
            } else if from < i && i <= to {
                i - 1
            } else if to <= i && i < from {
                i + 1
            } else {
                i
            }
        };
        self.current = self.current.map(remap);
        for h in &mut self.history {
            *h = remap(*h);
        }
        Some(Change::Move { from, to })
    }

    pub fn push_history(&mut self, i: usize) {
        self.history.push(i);
    }

    /// The last shuffled-to row that still exists.
    pub fn pop_history(&mut self) -> Option<usize> {
        while let Some(i) = self.history.pop() {
            if i < self.items.len() {
                return Some(i);
            }
        }
        None
    }

    pub fn history_len(&self) -> usize {
        self.history.len()
    }
}

/// A uniformly random row other than `current`, from a random number `r` in `0..len-1`. `None` for < 2 rows.
pub fn pick_other(len: usize, current: usize, r: usize) -> Option<usize> {
    if len < 2 {
        return None;
    }
    let r = r % (len - 1);
    Some(if r >= current { r + 1 } else { r })
}

// ---------------------------------------------------------------------------------------------------------
// Qt side

/// One queue row as the model keeps it: the song's JSON fields, read once into strings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueueRow {
    pub title: String,
    pub artist: String,
    pub thumbnail: String,
    pub duration: String,
    pub video_id: String,
    pub artist_id: String,
    pub album_id: String,
}

impl QueueRow {
    pub fn from_value(v: &Value) -> QueueRow {
        let s = |k: &str| v[k].as_str().unwrap_or("").to_owned();
        QueueRow {
            title: s("title"),
            artist: s("artist"),
            thumbnail: s("thumbnail"),
            duration: s("duration"),
            video_id: s("videoId"),
            artist_id: s("artistId"),
            album_id: s("albumId"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::apply;
    use super::*;

    fn q(n: usize) -> QueueState<char> {
        let mut s = QueueState::default();
        s.replace(('a'..).take(n).collect());
        s
    }

    /// Run `f`, apply its change to a mirror, and check the mirror equals the state.
    fn mirrored(s: &mut QueueState<char>, f: impl FnOnce(&mut QueueState<char>) -> Option<Change<char>>) {
        let mut mirror = s.items().to_vec();
        if let Some(c) = f(s) {
            apply(&mut mirror, &c);
        }
        assert_eq!(mirror, s.items());
    }

    #[test]
    fn an_album_goes_around_the_playing_song() {
        // 'C' plays alone; its album is a b c d, where the album calls the song 'c'
        let same = |a: &char, b: &char| a.eq_ignore_ascii_case(b);
        for (album, at) in [("abcd", 2), ("cabd", 0), ("abdc", 3), ("acbc", 1)] {
            let mut s = QueueState::default();
            s.replace(vec!['C']);
            s.set_current(Some(0));
            let before = s.epoch();
            let change = s.surround_current(album.chars().collect(), same).unwrap();
            assert_eq!(s.current(), Some(at), "{album}");
            assert_eq!(s.items()[at], 'C', "the row being played is kept, not swapped for the album's copy");
            assert_eq!(s.len(), 4);
            assert_eq!(change, Change::Reset(s.items().to_vec()));
            assert!(s.epoch() > before, "a list still being appended to the old queue must stop");
        }
    }

    #[test]
    fn an_album_without_the_playing_song_changes_nothing() {
        let mut s = q(3);
        s.set_current(Some(1));
        let before = s.clone();
        assert_eq!(s.surround_current(vec!['x', 'y'], |a, b| a == b), None);
        assert_eq!(s, before);
        let mut idle = q(3);
        assert_eq!(idle.surround_current(vec!['a'], |a, b| a == b), None, "nothing is playing");
    }

    #[test]
    fn the_playing_track_survives_every_move() {
        for len in 2..6 {
            for cur in 0..len {
                for from in 0..len {
                    for to in 0..len {
                        let mut s = q(len);
                        s.set_current(Some(cur));
                        let playing = s.items()[cur];
                        mirrored(&mut s, |s| s.move_item(from, to));
                        assert_eq!(s.items()[s.current().unwrap()], playing, "len {len} cur {cur} {from}->{to}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_epoch_changes_when_the_queue_is_replaced_or_cleared_not_when_it_is_edited() {
        let mut s = q(3);
        let e = s.epoch();
        s.push('x');
        s.extend(vec!['y', 'z']);
        s.move_item(0, 2);
        s.remove(1);
        s.set_current(Some(0));
        assert_eq!(s.epoch(), e, "edits leave it the same queue");
        s.replace(vec!['n']);
        assert_ne!(s.epoch(), e);
        let e = s.epoch();
        s.clear();
        assert_ne!(s.epoch(), e);
    }

    #[test]
    fn moves_that_do_nothing_report_nothing() {
        let mut s = q(3);
        assert_eq!(s.move_item(1, 1), None);
        assert_eq!(s.move_item(0, 3), None);
        assert_eq!(s.move_item(5, 0), None);
    }

    #[test]
    fn inserts_keep_the_playing_row_and_land_after_it() {
        let mut s = q(3);
        s.set_current(Some(1));
        assert_eq!(s.insert_next('x'), Change::Insert { at: 2, rows: vec!['x'] });
        assert_eq!(s.items(), ['a', 'b', 'x', 'c']);
        assert_eq!(s.current(), Some(1));
        // before the playing row: the index shifts
        s.insert_at(0, 'y');
        assert_eq!((s.current(), s.items()[1]), (Some(2), 'a'));
        // nothing playing: at the start
        let mut e = q(2);
        e.insert_next('z');
        assert_eq!(e.items(), ['z', 'a', 'b']);
    }

    #[test]
    fn append_does_not_disturb_anything() {
        let mut s = q(2);
        s.set_current(Some(1));
        s.push_history(0);
        mirrored(&mut s, |s| Some(s.push('z')));
        assert_eq!((s.current(), s.history_len(), s.len()), (Some(1), 1, 3));
    }

    #[test]
    fn extending_appends_a_batch() {
        let mut s = q(2);
        s.set_current(Some(0));
        mirrored(&mut s, |s| Some(s.extend(vec!['x', 'y'])));
        assert_eq!((s.len(), s.current()), (4, Some(0)));
    }

    #[test]
    fn removing_before_the_playing_row_shifts_it() {
        let mut s = q(4);
        s.set_current(Some(2));
        let r = s.remove(0).unwrap();
        assert!(!r.was_current && r.replacement.is_none());
        assert_eq!((s.current(), s.items()[2]), (Some(1), 'd'));
    }

    #[test]
    fn removing_after_the_playing_row_changes_nothing_for_it() {
        let mut s = q(4);
        s.set_current(Some(1));
        s.remove(3).unwrap();
        assert_eq!(s.current(), Some(1));
    }

    #[test]
    fn removing_the_playing_row_names_its_successor() {
        let mut s = q(3);
        s.set_current(Some(1));
        let r = s.remove(1).unwrap();
        assert!(r.was_current);
        assert_eq!((r.replacement, s.current()), (Some(1), None), "the next song slides into the same index");
        let mut s = q(3);
        s.set_current(Some(2));
        assert_eq!(s.remove(2).unwrap().replacement, Some(1), "removing the last falls back to the new last");
        let mut s = q(1);
        s.set_current(Some(0));
        let r = s.remove(0).unwrap();
        assert!(r.was_current && r.replacement.is_none() && s.is_empty());
    }

    #[test]
    fn removing_out_of_range_is_a_no_op() {
        let mut s = q(2);
        assert!(s.remove(2).is_none());
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn every_edit_is_reproducible_from_its_change() {
        let mut s = q(5);
        s.set_current(Some(2));
        mirrored(&mut s, |s| Some(s.insert_next('x')));
        mirrored(&mut s, |s| s.move_item(0, 4));
        mirrored(&mut s, |s| s.remove(3).map(|r| r.change));
        mirrored(&mut s, |s| Some(s.push('p')));
        mirrored(&mut s, |s| Some(s.clear()));
        assert!(s.is_empty() && s.current().is_none());
    }

    #[test]
    fn shuffle_history_follows_rows_around() {
        let mut s = q(5);
        s.set_current(Some(4));
        s.push_history(1); // 'b'
        s.move_item(1, 3); // 'b' is now at 3
        assert_eq!(s.pop_history(), Some(3));
        s.push_history(3);
        s.remove(0); // 'b'-position shifts to 2
        assert_eq!(s.pop_history(), Some(2));
        s.push_history(2);
        s.remove(2); // the row itself is gone
        assert_eq!(s.pop_history(), None);
    }

    #[test]
    fn replacing_the_queue_forgets_history_and_current() {
        let mut s = q(3);
        s.set_current(Some(1));
        s.push_history(0);
        s.replace(vec!['x']);
        assert_eq!((s.current(), s.history_len()), (None, 0));
    }

    #[test]
    fn shuffle_never_picks_the_current_row_and_reaches_all_others() {
        for len in 2..7 {
            for cur in 0..len {
                let picks: std::collections::BTreeSet<usize> = (0..len - 1).filter_map(|r| pick_other(len, cur, r)).collect();
                assert!(!picks.contains(&cur));
                assert_eq!(picks.len(), len - 1);
            }
        }
        assert_eq!(pick_other(1, 0, 0), None);
    }

    #[test]
    fn rows_are_read_from_song_json() {
        let r = QueueRow::from_value(&serde_json::json!({"title": "T", "videoId": "v", "albumId": "MPREb"}));
        assert_eq!((r.title.as_str(), r.video_id.as_str(), r.album_id.as_str(), r.artist.as_str()), ("T", "v", "MPREb", ""));
    }
}
