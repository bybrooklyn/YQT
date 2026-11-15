// SPDX-License-Identifier: GPL-3.0-only
//! Download requests made while the manager is still starting up (pure).
//!
//! Start-up reconciles the downloads folder and asks NetworkManager, which can take seconds; waiting for it on the Qt
//! thread would freeze the window. So "Download" and "Keep offline" are noted here and replayed, in the order they
//! were made, once start-up is done.
use super::manager::Item;

/// One request, as the UI made it.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Download these songs (one song, or an album / playlist).
    Download(Vec<Item>),
    /// Turn "Keep offline" for a collection on or off.
    KeepOffline { collection: String, items: Vec<Item>, on: bool },
}

#[derive(Debug, Default)]
pub struct Pending {
    requests: Vec<Request>,
}

impl Pending {
    /// Note a download. Songs already waiting are left out (asking twice downloads once). Returns the ids that were
    /// added, so the UI can show them as queued.
    pub fn download(&mut self, items: Vec<Item>) -> Vec<String> {
        let mut fresh: Vec<Item> = Vec::new();
        for it in items {
            if !self.is_queued(&it.id) && !fresh.iter().any(|f| f.id == it.id) {
                fresh.push(it);
            }
        }
        let ids = fresh.iter().map(|it| it.id.clone()).collect();
        if !fresh.is_empty() {
            self.requests.push(Request::Download(fresh));
        }
        ids
    }

    /// Note "Keep offline" on/off. Only the latest choice for a collection counts; it takes the place of an earlier one.
    pub fn keep_offline(&mut self, collection: &str, items: Vec<Item>, on: bool) {
        self.requests.retain(|r| !matches!(r, Request::KeepOffline { collection: c, .. } if c == collection));
        self.requests.push(Request::KeepOffline { collection: collection.to_owned(), items, on });
    }

    /// Forget a song's waiting download (the user removed it). `true` = there was one.
    pub fn cancel(&mut self, id: &str) -> bool {
        let mut found = false;
        for r in &mut self.requests {
            if let Request::Download(items) = r {
                let before = items.len();
                items.retain(|it| it.id != id);
                found |= items.len() != before;
            }
        }
        self.requests.retain(|r| !matches!(r, Request::Download(items) if items.is_empty()));
        found
    }

    /// Is a download of this song waiting?
    pub fn is_queued(&self, id: &str) -> bool {
        self.requests.iter().any(|r| matches!(r, Request::Download(items) if items.iter().any(|it| it.id == id)))
    }

    /// The waiting "Keep offline" choice for a collection, if any.
    pub fn kept_offline(&self, collection: &str) -> Option<bool> {
        self.requests.iter().rev().find_map(|r| match r {
            Request::KeepOffline { collection: c, on, .. } if c == collection => Some(*on),
            _ => None,
        })
    }

    /// Everything waiting, in the order it was asked for; afterwards nothing is.
    pub fn take(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> Item {
        Item { id: id.into(), title: format!("t-{id}"), ..Item::default() }
    }

    fn ids(r: &Request) -> Vec<&str> {
        match r {
            Request::Download(items) | Request::KeepOffline { items, .. } => items.iter().map(|it| it.id.as_str()).collect(),
        }
    }

    #[test]
    fn downloads_are_replayed_in_order_once() {
        let mut p = Pending::default();
        assert_eq!(p.download(vec![item("aaaaaaaaaaa")]), vec!["aaaaaaaaaaa"]);
        assert_eq!(p.download(vec![item("bbbbbbbbbbb"), item("ccccccccccc")]), vec!["bbbbbbbbbbb", "ccccccccccc"]);
        assert!(p.is_queued("bbbbbbbbbbb"));
        let got = p.take();
        assert_eq!(got.iter().map(ids).collect::<Vec<_>>(), vec![vec!["aaaaaaaaaaa"], vec!["bbbbbbbbbbb", "ccccccccccc"]]);
        // taken means gone: nothing runs twice
        assert!(p.take().is_empty());
        assert!(!p.is_queued("aaaaaaaaaaa"));
    }

    #[test]
    fn asking_twice_queues_a_song_once() {
        let mut p = Pending::default();
        p.download(vec![item("aaaaaaaaaaa")]);
        assert!(p.download(vec![item("aaaaaaaaaaa")]).is_empty());
        assert_eq!(p.download(vec![item("aaaaaaaaaaa"), item("bbbbbbbbbbb"), item("bbbbbbbbbbb")]), vec!["bbbbbbbbbbb"]);
        assert_eq!(p.take().iter().map(ids).collect::<Vec<_>>(), vec![vec!["aaaaaaaaaaa"], vec!["bbbbbbbbbbb"]]);
    }

    #[test]
    fn a_removed_song_is_not_downloaded_later() {
        let mut p = Pending::default();
        p.download(vec![item("aaaaaaaaaaa")]);
        p.download(vec![item("bbbbbbbbbbb"), item("ccccccccccc")]);
        assert!(p.cancel("aaaaaaaaaaa"));
        assert!(p.cancel("ccccccccccc"));
        assert!(!p.cancel("ccccccccccc"));
        assert!(!p.is_queued("aaaaaaaaaaa"));
        // the emptied request is dropped, not replayed as an empty download
        assert_eq!(p.take(), vec![Request::Download(vec![item("bbbbbbbbbbb")])]);
        // asking again after removing queues it again
        assert_eq!(p.download(vec![item("aaaaaaaaaaa")]), vec!["aaaaaaaaaaa"]);
    }

    #[test]
    fn the_latest_keep_offline_choice_wins_and_keeps_its_place() {
        let mut p = Pending::default();
        p.keep_offline("PL1", vec![item("aaaaaaaaaaa")], true);
        p.download(vec![item("bbbbbbbbbbb")]);
        p.keep_offline("PL2", vec![], true);
        assert_eq!(p.kept_offline("PL1"), Some(true));
        p.keep_offline("PL1", vec![item("aaaaaaaaaaa")], false);
        assert_eq!(p.kept_offline("PL1"), Some(false));
        assert_eq!(p.kept_offline("PL3"), None);
        // removing a song does not touch a collection's choice
        assert!(!p.cancel("aaaaaaaaaaa"));
        let got = p.take();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], Request::Download(vec![item("bbbbbbbbbbb")]));
        assert!(matches!(&got[1], Request::KeepOffline { collection, on: true, .. } if collection == "PL2"));
        assert!(matches!(&got[2], Request::KeepOffline { collection, on: false, .. } if collection == "PL1"));
    }
}
