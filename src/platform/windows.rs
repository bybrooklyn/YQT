// SPDX-License-Identifier: GPL-3.0-only
//! Windows: DPAPI and the registry (default browser ProgId, the Qt settings key).
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCopyTreeW, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE,
    REG_CREATED_NEW_KEY, REG_OPTION_NON_VOLATILE, RRF_RT_REG_SZ,
};

/// An open registry key under `HKEY_CURRENT_USER`, closed when dropped.
struct Key(HKEY);

impl Key {
    /// Open `HKCU\<path>` for reading; `None` if it does not exist (or cannot be opened).
    fn open(path: &str) -> Option<Key> {
        let path = wide(path);
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: `path` is a live NUL-terminated buffer and `key` a valid place for the handle the call returns.
        let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) };
        (rc == 0).then_some(Key(key))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from RegOpenKeyExW / RegCreateKeyExW and is closed exactly once.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

/// Does `HKCU\<path>` exist?
pub fn key_exists(path: &str) -> bool {
    Key::open(path).is_some()
}

/// Copy `HKCU\<old>` (its values and all subkeys) to `HKCU\<new>`. `Ok(false)` when there is nothing to copy or
/// `new` turns out to exist already: an existing key is never written to. A copy that fails half-way is removed
/// again, so `new` either holds everything or is absent.
pub fn copy_key(old: &str, new: &str) -> Result<bool, String> {
    let Some(src) = Key::open(old) else { return Ok(false) };
    let new_w = wide(new);
    let mut dst: HKEY = std::ptr::null_mut();
    let mut disposition = 0;
    // SAFETY: `new_w` is a live NUL-terminated buffer, the class and security attributes may be null, and `dst` /
    // `disposition` are valid places for the results.
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            new_w.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_READ | KEY_WRITE,
            std::ptr::null(),
            &mut dst,
            &mut disposition,
        )
    };
    if rc != 0 {
        return Err(format!("cannot create the registry key (error {rc})"));
    }
    let dst = Key(dst);
    if disposition != REG_CREATED_NEW_KEY {
        return Ok(false); // it was there after all (another instance got in first): leave it alone
    }
    // SAFETY: both handles are open keys; a null subkey name means "the source key itself".
    let rc = unsafe { RegCopyTreeW(src.0, std::ptr::null(), dst.0) };
    if rc != 0 {
        drop(dst);
        // SAFETY: `new_w` is a live NUL-terminated buffer; this removes only the key created just above.
        unsafe {
            RegDeleteTreeW(HKEY_CURRENT_USER, new_w.as_ptr());
        }
        return Err(format!("cannot copy the registry key (error {rc})"));
    }
    Ok(true)
}

/// Windows Data Protection API: decrypt a blob that only this user's session can.
pub fn dpapi_unprotect(data: &[u8]) -> Option<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    // SAFETY: `input` points at `data` for the duration of the call; on success `out` owns a
    // LocalAlloc'd buffer of `cbData` bytes, which is copied and then released with LocalFree.
    unsafe {
        let ok = CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), 0, &mut out);
        if ok == 0 || out.pbData.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as _);
        Some(bytes)
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice` → `ProgId`.
pub fn default_browser() -> Option<String> {
    let key = wide(r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice");
    let name = wide("ProgId");
    let mut buf = [0u16; 256];
    let mut bytes = (buf.len() * 2) as u32;
    // SAFETY: all pointers refer to live, NUL-terminated / correctly sized buffers for the call.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if rc != 0 {
        return None;
    }
    let len = (bytes as usize / 2).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..len]);
    let s = s.trim_end_matches('\0').to_owned();
    (!s.is_empty()).then_some(s)
}
