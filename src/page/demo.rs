// SPDX-License-Identifier: GPL-3.0-only
//! Canned pages for the developer harness (`--features harness`, `ytm --qml <file>`): no network, no account.
//!
//! Targets: `demo:long` (3 pages of 40 songs), `home` (4 rows arriving 0.1–0.9 s apart), `album:demo`
//! (hero + numbered tracks), `demo:empty`, `demo:error:<kind>` (a failure of that kind), `library` (with both a
//! synthetic and YouTube's "Liked music").

use super::{BoxFut, PageSource};
use crate::errors::{AppError, ErrorKind};
use crate::pagedata::{Action, Hero, Item, ItemKind, PageData, Section, SectionStyle, Stat};
use crate::pages::{HomeRow, Request};
use std::time::Duration;

pub struct Demo;

fn song(n: usize, prefix: &str) -> Item {
    Item {
        kind: ItemKind::Song,
        title: format!("{prefix} song {n}"),
        subtitle: format!("Artist {} • Album {}", n % 7, n % 5),
        artist: format!("Artist {}", n % 7),
        video_id: format!("{prefix}{n:03}"),
        duration: format!("{}:{:02}", 2 + n % 4, (n * 7) % 60),
        artist_id: format!("UCdemo{}", n % 7),
        album_id: format!("MPREbdemo{}", n % 5),
        ..Default::default()
    }
}

fn card(kind: ItemKind, n: usize, title: &str) -> Item {
    Item {
        kind, title: format!("{title} {n}"), subtitle: format!("{kind:?} • demo"), browse_id: format!("demo{n}"), ..Default::default()
    }
}

fn section(id: &str, title: &str, style: SectionStyle, items: Vec<Item>) -> Section {
    Section { id: id.into(), title: title.into(), style, items, ..Default::default() }
}

fn long_page(first: usize, count: usize, has_more: bool) -> PageData {
    let items = (first..first + count).map(|n| song(n, "long")).collect();
    PageData {
        sections: vec![section("songs", "", SectionStyle::List, items)],
        pager: has_more.then_some(1),
        has_more,
        ..Default::default()
    }
}

const RICH_PLAYLIST_PAGER: u32 = 77;

const BIO: &str = "Demo Artist is a made-up band that exists only to fill this page with enough text to need a \"Read more\" link. \
They formed in a garage that never existed, released twelve albums that were never recorded, and toured cities that are \
not on any map. Critics who do not exist called the sound \"unmistakably synthetic\". The band split in 2019, reunited in \
2021, and has been on an indefinite hiatus ever since.";

fn rich_hero(kind: ItemKind, title: &str, subtitle: &str, stats: &[&str], actions: Vec<Action>) -> Hero {
    Hero {
        kind: Some(kind),
        title: title.into(),
        subtitle: subtitle.into(),
        description: "A short description of the page, shown under the title.".into(),
        stats: stats.iter().map(|s| Stat { text: (*s).into(), icon: String::new() }).collect(),
        actions,
        ..Default::default()
    }
}

/// An album with a hero, artist links, numbered tracks and shelves (`album:rich`).
fn rich_album() -> PageData {
    PageData {
        hero: Some(rich_hero(
            ItemKind::Album,
            "Demo Album",
            "Demo Artist",
            &["Album", "2024", "12 songs", "46 min"],
            vec![Action::Play, Action::Shuffle, Action::Download, Action::AddToQueue],
        )),
        sections: vec![
            section(
                "hero-artists",
                "",
                SectionStyle::Artists,
                vec![card(ItemKind::Artist, 1, "Demo Artist"), card(ItemKind::Artist, 2, "Guest Singer")],
            ),
            section("tracks", "", SectionStyle::NumberedList, (1..=12).map(|n| Item { number: n as u32, ..song(n, "alb") }).collect()),
            section("variants", "Other versions", SectionStyle::Cards, (1..=3).map(|n| card(ItemKind::Album, n, "Deluxe")).collect()),
            section(
                "more-by",
                "More by Demo Artist",
                SectionStyle::Cards,
                (1..=8).map(|n| card(ItemKind::Album, n + 10, "Older album")).collect(),
            ),
        ],
        ..Default::default()
    }
}

/// A playlist that continues (two pages), with the Save button (`playlist:rich`).
fn rich_playlist(first: usize, has_more: bool) -> PageData {
    PageData {
        hero: (first == 0).then(|| {
            rich_hero(
                ItemKind::Playlist,
                "Road trip",
                "You",
                &["Playlist", "60 songs"],
                vec![Action::Play, Action::Shuffle, Action::Download, Action::AddToQueue, Action::SaveToLibrary],
            )
        }),
        sections: vec![section(
            "tracks",
            "",
            SectionStyle::NumberedList,
            (first..first + 30).map(|n| Item { number: n as u32 + 1, ..song(n, "pl") }).collect(),
        )],
        pager: has_more.then_some(RICH_PLAYLIST_PAGER),
        has_more,
    }
}

/// An artist with a bio and a Wikipedia link, top songs and shelves (`artist:rich`).
fn rich_artist() -> PageData {
    let mut hero = rich_hero(ItemKind::Artist, "Demo Artist", "1.2M subscribers", &[], vec![Action::Play, Action::Shuffle, Action::Radio]);
    hero.description = BIO.into();
    hero.wikipedia_url = "https://en.wikipedia.org/wiki/Demo".into();
    PageData {
        hero: Some(hero),
        sections: vec![
            section("hero-radio", "", SectionStyle::Cards, vec![card(ItemKind::Playlist, 99, "Demo Artist Radio")]),
            section("top-songs", "Top songs", SectionStyle::List, (1..=5).map(|n| song(n, "top")).collect()),
            section("albums", "Albums", SectionStyle::Cards, (1..=7).map(|n| card(ItemKind::Album, n, "Album")).collect()),
            section("singles", "Singles & EPs", SectionStyle::Cards, (1..=4).map(|n| card(ItemKind::Album, n + 20, "Single")).collect()),
            section("similar", "Fans also like", SectionStyle::Artists, (1..=8).map(|n| card(ItemKind::Artist, n, "Similar")).collect()),
        ],
        ..Default::default()
    }
}

fn error_kind(name: &str) -> ErrorKind {
    match name {
        "network" => ErrorKind::Network,
        "auth" => ErrorKind::Auth,
        "parse" => ErrorKind::Parse,
        "notfound" => ErrorKind::NotFound,
        "ratelimit" => ErrorKind::RateLimit,
        "offline" => ErrorKind::Offline,
        "unavailable" => ErrorKind::Unavailable,
        "storage" => ErrorKind::Storage,
        _ => ErrorKind::Internal,
    }
}

impl PageSource for Demo {
    fn load(&self, req: Request) -> BoxFut<Result<PageData, AppError>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let t = req.target.as_str();
            if let Some(kind) = t.strip_prefix("demo:error:") {
                let k = error_kind(kind);
                let headline = match k {
                    ErrorKind::Auth => crate::auth::AUTH_MSG.to_owned(),
                    ErrorKind::Offline => "You're offline.".to_owned(),
                    _ => format!("Demo {kind} failure: something went wrong while loading."),
                };
                return Err(AppError::new(k, headline, "demo detail: HTTP 500 at /browse (cookie: SAPISID=secretvalue123)"));
            }
            match t {
                "demo:long" => Ok(long_page(0, 40, true)),
                "album:rich" => Ok(rich_album()),
                "playlist:rich" => Ok(rich_playlist(0, true)),
                "artist:rich" => Ok(rich_artist()),
                "library:albums" => Ok(PageData {
                    sections: vec![section(
                        "albums",
                        "Albums",
                        SectionStyle::Grid,
                        (1..=14).map(|n| card(ItemKind::Album, n, "Saved album")).collect(),
                    )],
                    ..Default::default()
                }),
                "library:songs" => Ok(long_page(0, 8, false)),
                "demo:empty" => Ok(PageData::default()),
                "album:demo" => Ok(PageData {
                    hero: Some(Hero {
                        kind: Some(ItemKind::Album),
                        title: "Demo Album".into(),
                        subtitle: "Album • Demo Artist • 2024".into(),
                        description: "A short description of the album, shown under the title.".into(),
                        stats: vec![
                            Stat { text: "12 songs".into(), icon: String::new() },
                            Stat { text: "46 min".into(), icon: String::new() },
                        ],
                        actions: vec![Action::Play, Action::Shuffle],
                        ..Default::default()
                    }),
                    sections: vec![section(
                        "tracks",
                        "",
                        SectionStyle::NumberedList,
                        (1..=12).map(|n| Item { number: n as u32, ..song(n, "alb") }).collect(),
                    )],
                    ..Default::default()
                }),
                "library:playlists" => Ok(PageData {
                    sections: vec![section(
                        "playlists",
                        "Playlists",
                        SectionStyle::List,
                        vec![
                            Item {
                                kind: ItemKind::Playlist,
                                title: "Liked music".into(),
                                subtitle: "Playlist".into(),
                                browse_id: "LIKED".into(),
                                ..Default::default()
                            },
                            card(ItemKind::Playlist, 1, "Road trip"),
                            Item {
                                kind: ItemKind::Playlist,
                                title: "Liked music".into(),
                                subtitle: "Playlist • You".into(),
                                browse_id: "LM".into(),
                                ..Default::default()
                            },
                            card(ItemKind::Playlist, 2, "Focus"),
                        ],
                    )],
                    ..Default::default()
                }),
                _ => Ok(long_page(0, 5, false)),
            }
        })
    }

    fn more(&self, pager: u32) -> BoxFut<Result<PageData, AppError>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            // the rich playlist has a second (and last) page of 30 songs
            if pager == RICH_PLAYLIST_PAGER {
                return Ok(PageData { pager: None, ..rich_playlist(30, false) });
            }
            // 3 pages in all: the pager id counts the pages already fetched
            let page = pager as usize;
            let mut p = long_page(40 * page, 40, page < 2);
            p.pager = (page < 2).then_some(pager + 1);
            Ok(p)
        })
    }

    fn signed_in(&self) -> bool {
        true
    }

    fn home_rows(&self) -> Vec<HomeRow> {
        let row = |slot: i32, delay: u64, title: &'static str, style: SectionStyle, kind: ItemKind| HomeRow {
            slot,
            load: Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                let items = (0..6).map(|n| if kind == ItemKind::Song { song(n, title) } else { card(kind, n, title) }).collect();
                Ok(vec![section(title, title, style, items)])
            }),
        };
        vec![
            row(0, 900, "Listen again", SectionStyle::Grid, ItemKind::Song),
            row(1, 300, "Trending", SectionStyle::List, ItemKind::Song),
            row(2, 100, "New albums", SectionStyle::Cards, ItemKind::Album),
            row(3, 600, "Top artists", SectionStyle::Artists, ItemKind::Artist),
        ]
    }
}
