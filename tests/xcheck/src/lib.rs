//! Type-checks the app's real Windows and macOS platform modules (included by path, unchanged) against the real
//! target APIs. `PlatformError`, `SecretStore` and `parse_launchservices_plist` are stand-ins with the same shapes
//! as in src/platform/mod.rs.
#![allow(dead_code)]

#[derive(Debug)]
pub enum PlatformError {
    Failed(String),
}

pub trait SecretStore {
    fn password(&self, service: &str, account: &str) -> Result<Option<String>, PlatformError>;
}

pub fn parse_launchservices_plist(_bytes: &[u8]) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
#[path = "../../../src/platform/macos.rs"]
mod macos;

#[cfg(windows)]
#[path = "../../../src/platform/windows.rs"]
mod windows;
