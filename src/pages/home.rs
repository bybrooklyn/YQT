// SPDX-License-Identifier: GPL-3.0-only
//! Home: card shelves instead of one long list. Rows keep loading independently (`pages::home_loaders`) so a slow
//! row never holds up the others; this module decides how each row looks.

use crate::errors::AppError;
use crate::pagedata::{Hero, ItemKind, PageData, Section, SectionStyle};

/// Greeting for a local hour (0-23).
pub fn greeting(hour: u32) -> &'static str {
    match hour {
        5..=11 => "Good morning",
        12..=17 => "Good afternoon",
        18..=22 => "Good evening",
        _ => "Hello, night owl",
    }
}

/// The greeting for the current local time.
pub fn greeting_now() -> &'static str {
    let hour = cxx_qt_lib::QDateTime::current_date_time().time().hour();
    greeting(u32::try_from(hour).unwrap_or(12))
}

/// How a legacy row (identified by its section title) should look, and its stable id + display title/subtitle.
/// "More like Song X" becomes a "More like" kicker over the song's name.
pub fn restyle(mut s: Section) -> Section {
    if let Some(last) = s.title.strip_prefix("More like ") {
        s.subtitle = last.to_owned();
        s.title = "More like".to_owned();
        s.id = "more-like".to_owned();
        s.style = SectionStyle::Cards;
        return s;
    }
    s.style = match (s.title.as_str(), s.items.first().map(|i| i.kind)) {
        ("Listen again", _) => SectionStyle::Grid,
        (_, Some(ItemKind::Artist)) => SectionStyle::Artists,
        _ => SectionStyle::Cards,
    };
    s
}

pub fn restyle_all(sections: Vec<Section>) -> Vec<Section> {
    sections.into_iter().map(restyle).collect()
}

/// The Home page from rows that arrived in slot order (`rows` = `(slot, result)`); an error only if *every* row failed.
pub fn assemble(mut rows: Vec<(i32, Result<Vec<Section>, AppError>)>, greeting: &str) -> Result<PageData, AppError> {
    rows.sort_by_key(|(slot, _)| *slot);
    let mut sections = Vec::new();
    let mut first_err = None;
    for (_, row) in rows {
        match row {
            Ok(s) => sections.extend(s),
            Err(e) => first_err = first_err.or(Some(e)),
        }
    }
    let page = PageData { hero: Some(Hero { title: greeting.to_owned(), ..Default::default() }), sections, pager: None, has_more: false }
        .cleaned();
    match (page.item_count(), first_err) {
        (0, Some(e)) => Err(e),
        _ => Ok(page),
    }
}

/// Load every row concurrently and return the whole page (the non-progressive path).
pub async fn load() -> Result<PageData, AppError> {
    let handles: Vec<_> = super::home_loaders()
        .into_iter()
        .map(|row| {
            let slot = row.slot;
            (slot, tokio::spawn(row.load))
        })
        .collect();
    let mut rows = Vec::new();
    for (slot, h) in handles {
        rows.push((
            slot,
            h.await.unwrap_or_else(|e| Err(AppError::new(crate::errors::ErrorKind::Internal, "Home failed to load.", e.to_string()))),
        ));
    }
    assemble(rows, greeting_now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pagedata::Item;

    fn item(kind: ItemKind, id: &str) -> Item {
        Item {
            kind,
            video_id: if kind == ItemKind::Song { id.into() } else { String::new() },
            browse_id: if kind == ItemKind::Song { String::new() } else { id.into() },
            ..Default::default()
        }
    }
    fn sec(title: &str, items: Vec<Item>) -> Section {
        Section { id: crate::pages::slug(title), title: title.into(), items, ..Default::default() }
    }

    #[test]
    fn greeting_follows_the_clock() {
        assert_eq!(greeting(5), "Good morning");
        assert_eq!(greeting(11), "Good morning");
        assert_eq!(greeting(12), "Good afternoon");
        assert_eq!(greeting(17), "Good afternoon");
        assert_eq!(greeting(18), "Good evening");
        assert_eq!(greeting(22), "Good evening");
        assert_eq!(greeting(23), "Hello, night owl");
        assert_eq!(greeting(3), "Hello, night owl");
    }

    #[test]
    fn rows_get_their_layout() {
        let song = || vec![item(ItemKind::Song, "v")];
        assert_eq!(restyle(sec("Listen again", song())).style, SectionStyle::Grid);
        assert_eq!(restyle(sec("Trending", song())).style, SectionStyle::Cards);
        assert_eq!(restyle(sec("Top artists", vec![item(ItemKind::Artist, "UC1")])).style, SectionStyle::Artists);
        assert_eq!(restyle(sec("New albums & singles", vec![item(ItemKind::Album, "MPREb")])).style, SectionStyle::Cards);
    }

    #[test]
    fn more_like_becomes_a_kicker_over_the_song_name() {
        let s = restyle(sec("More like Bohemian Rhapsody", vec![item(ItemKind::Song, "v")]));
        assert_eq!((s.id.as_str(), s.title.as_str(), s.subtitle.as_str()), ("more-like", "More like", "Bohemian Rhapsody"));
        assert_eq!(s.style, SectionStyle::Cards);
    }

    #[test]
    fn rows_are_ordered_by_slot_and_one_failure_does_not_sink_the_page() {
        let err = AppError::new(crate::errors::ErrorKind::Network, "offline", "");
        let rows = vec![
            (2, Ok(vec![sec("New albums & singles", vec![item(ItemKind::Album, "A")])])),
            (1, Err(err)),
            (0, Ok(vec![sec("Listen again", vec![item(ItemKind::Song, "v")])])),
        ];
        let p = assemble(rows, "Good morning").unwrap();
        assert_eq!(p.sections.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(), ["Listen again", "New albums & singles"]);
        assert_eq!(p.hero.unwrap().title, "Good morning");
    }

    #[test]
    fn all_rows_failing_is_an_error_but_all_empty_is_an_empty_page() {
        let e = || AppError::new(crate::errors::ErrorKind::Network, "offline", "");
        assert_eq!(assemble(vec![(0, Err(e())), (1, Err(e()))], "x").unwrap_err().headline, "offline");
        assert!(assemble(vec![(0, Ok(vec![]))], "x").unwrap().sections.is_empty());
    }
}
