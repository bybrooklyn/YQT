// SPDX-License-Identifier: GPL-3.0-only
//! The pure-Rust audio engine: HTTP-range reader -> demux (symphonia) -> decode (opus-rs / symphonia AAC)
//! -> resample (rubato) -> EQ (biquad) -> limiter -> output (cpal, or a null / WAV sink for tests).
//!
//! No FFmpeg, no Qt Multimedia. Everything runs on the engine's own threads; the Qt thread only sends commands.
// The offline renderer, WAV writer, analysis helpers and a few accessors are for the tests and tools; the app does not call them.
#![allow(dead_code)]
#[cfg(test)]
pub mod analysis;
#[cfg(test)]
mod codec_tests;
pub mod decode;
pub mod engine;
#[cfg(test)]
mod engine_tests;
pub mod eq;
pub mod limiter;
pub mod opus;
pub mod output;
pub mod presets;
pub mod resample;
pub mod source;
pub mod track;
