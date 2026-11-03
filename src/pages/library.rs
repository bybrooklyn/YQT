// SPDX-License-Identifier: GPL-3.0-only
//! Library tabs: playlists, albums and artists as a grid of cards; songs and history as dense lists.

use crate::errors::AppError;
use crate::pagedata::{PageData, SectionStyle};

/// Layout for a library tab.
pub fn style_for(tab: &str) -> SectionStyle {
    match tab {
        "songs" | "history" => SectionStyle::List,
        _ => SectionStyle::Grid, // playlists, albums, artists
    }
}

pub fn restyle(tab: &str, mut page: PageData) -> PageData {
    let style = style_for(tab);
    for s in &mut page.sections {
        s.style = style;
    }
    page
}

pub async fn load(tab: &str) -> Result<PageData, AppError> {
    let value = crate::backend::browse(format!("library:{tab}")).await.map_err(|e| super::error_from_ui_string(&e))?;
    Ok(restyle(tab, super::page_from_value(&value)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pagedata::{Item, ItemKind, Section};

    #[test]
    fn tabs_pick_cards_or_rows() {
        for tab in ["playlists", "albums", "artists", ""] {
            assert_eq!(style_for(tab), SectionStyle::Grid, "{tab}");
        }
        assert_eq!(style_for("songs"), SectionStyle::List);
        assert_eq!(style_for("history"), SectionStyle::List);
    }

    #[test]
    fn restyling_covers_every_section() {
        let item = Item { kind: ItemKind::Playlist, browse_id: "PL".into(), ..Default::default() };
        let page = PageData {
            sections: vec![
                Section { items: vec![item.clone()], ..Default::default() },
                Section { items: vec![item], ..Default::default() },
            ],
            ..Default::default()
        };
        assert!(restyle("albums", page).sections.iter().all(|s| s.style == SectionStyle::Grid));
    }
}
