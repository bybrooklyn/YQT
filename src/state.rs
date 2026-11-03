// SPDX-License-Identifier: GPL-3.0-only
//! Process-wide tokio runtime + RustyPipe client shared by the Qt objects.
use rustypipe::client::RustyPipe;
use std::sync::OnceLock;
use tokio::runtime::Runtime;

pub struct State {
    pub rt: Runtime,
    pub rp: RustyPipe,
}

/// Per-user data (cookies, session, RustyPipe cache). `dirs` picks the right place per OS; on Linux
/// that is `$XDG_DATA_HOME/ytm`, exactly where older versions already kept their files.
pub fn data_dir() -> std::path::PathBuf {
    let dir = dirs::data_dir().unwrap_or_else(|| ".".into()).join("ytm");
    std::fs::create_dir_all(&dir).ok();
    dir
}

pub fn st() -> &'static State {
    static S: OnceLock<State> = OnceLock::new();
    S.get_or_init(|| {
        let dir = data_dir();
        State {
            rt: tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime"),
            rp: RustyPipe::builder().storage_dir(dir).build().expect("rustypipe"),
        }
    })
}

/// Shared HTTP client for the small third-party/API calls (lyrics, likes, playlists).
pub fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        let n = &crate::config::get().network;
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(n.connect_timeout_secs))
            .timeout(std::time::Duration::from_secs(n.request_timeout_secs))
            .user_agent("ytm-qt/0.1 (GPL-3.0 YouTube Music client)")
            .build()
            .expect("http client")
    })
}
