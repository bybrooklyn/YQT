// SPDX-License-Identifier: GPL-3.0-only
//! Stand-in for `mpris.rs` on platforms without MPRIS (Windows, macOS): same API, does nothing.
//! Media keys there need SMTC / MPRemoteCommandCenter bindings, which are not implemented yet.
#![allow(dead_code)]

#[derive(Clone, Default, PartialEq)]
pub struct Snap {
    pub has_track: bool,
    pub playing: bool,
    pub title: String,
    pub artist: String,
    pub art: String,
    pub video_id: String,
    pub length_us: i64,
    pub volume: f64,
    pub shuffle: bool,
    pub repeat: i32,
}

pub fn start(_forward: impl Fn(&str, i64) + Send + Sync + 'static) {}

pub fn update(_new: Snap, _position_us: i64) {}
