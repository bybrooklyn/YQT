// SPDX-License-Identifier: GPL-3.0-only
//! macOS: Keychain via the Security framework, default browser from the LaunchServices plist.
use super::{PlatformError, SecretStore};

pub struct Keychain;

impl SecretStore for Keychain {
    fn password(&self, service: &str, account: &str) -> Result<Option<String>, PlatformError> {
        use security_framework::passwords::get_generic_password;
        match get_generic_password(service, account) {
            Ok(bytes) => Ok(String::from_utf8(bytes).ok().filter(|s| !s.is_empty())),
            // errSecItemNotFound
            Err(e) if e.code() == -25300 => Ok(None),
            // errSecUserCanceled / errSecAuthFailed: the user said no
            Err(e) => Err(PlatformError::Failed(format!("Keychain error {}", e.code()))),
        }
    }
}

pub fn default_browser() -> Option<String> {
    let home = dirs::home_dir()?;
    let bytes = std::fs::read(home.join("Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist")).ok()?;
    super::parse_launchservices_plist(&bytes)
}
