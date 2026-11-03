// SPDX-License-Identifier: GPL-3.0-only
//! Windows: DPAPI and the registry (default browser ProgId).
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};

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
