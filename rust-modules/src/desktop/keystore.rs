//! **The desktop's key store**: where the persisted Plex session's secret is protected on a Mac or
//! a Windows PC, in place of the television's key manager.
//!
//! - **macOS: the login Keychain.** The whole session is a generic-password item (service
//!   [`SERVICE`], account [`KEY`]); the envelope on disk carries only its name. Sign-out deletes the
//!   item. The calls are the `SecKeychain*GenericPassword` family: deprecated in favour of
//!   `SecItem*`, still exported by every macOS this app supports, and five plain C calls where the
//!   replacement needs a CFDictionary per query. An unsigned or ad-hoc-signed app's item is bound to
//!   the build that created it, so after an update macOS asks once whether the new build may read
//!   it; refusing leaves the session locked and the app asks for a fresh sign-in.
//! - **Windows: DPAPI** (`CryptProtectData`, user scope). The ciphertext is the envelope's `data`;
//!   only this Windows user on this PC can open it. Nothing to delete at sign-out: removing the
//!   file removes the secret.
//! - **Elsewhere (Linux): none.** `seal` answers `None` and the session falls back to the
//!   mode-0600 file, as the simulator always did. Said once per process.
use plx_base::eventlog::log;
use plx_platform::tv::secure::{Backend, Sealed};

/// The one name the session is stored under, in every backend.
const KEY: &str = "plxnative-session";
/// The Keychain service the item is filed under (what Keychain Access shows as "Where").
#[cfg(target_os = "macos")]
const SERVICE: &str = "com.beb.plxnative";

/// This platform's backend, if it has one.
fn native() -> Option<Backend> {
    if cfg!(target_os = "macos") {
        Some(Backend::MacKeychain)
    } else if cfg!(windows) {
        Some(Backend::WinDpapi)
    } else {
        None
    }
}

pub fn seal(plain: &[u8]) -> Option<Sealed> {
    let Some(backend) = native() else {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| log("session protection: no desktop key store; using the 0600 file fallback"));
        return None;
    };
    let data = match os::seal(plain) {
        Ok(data) => data,
        Err(status) => {
            log(&format!("session protection: {backend:?} refused the session (status {status}); using the 0600 file fallback"));
            return None;
        }
    };
    Some(Sealed { backend, key: KEY.into(), iv: String::new(), data })
}

pub fn open(sealed: &Sealed) -> Option<Vec<u8>> {
    // An envelope another platform (or the television) sealed is locked here, never guessed at.
    if Some(&sealed.backend) != native().as_ref() || sealed.key != KEY {
        return None;
    }
    os::open(&sealed.data)
        .map_err(|status| log(&format!("session protection: {:?} would not open the session (status {status})", sealed.backend)))
        .ok()
}

pub fn remove(backend: &Backend, key: &str) {
    if Some(backend) == native().as_ref() && key == KEY {
        os::remove();
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::{KEY, SERVICE};
    use std::os::raw::{c_char, c_void};

    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecKeychainFindGenericPassword(
            keychain_or_array: *const c_void,
            service_len: u32,
            service: *const c_char,
            account_len: u32,
            account: *const c_char,
            password_len: *mut u32,
            password: *mut *mut c_void,
            item: *mut *mut c_void,
        ) -> i32;
        fn SecKeychainAddGenericPassword(
            keychain: *mut c_void,
            service_len: u32,
            service: *const c_char,
            account_len: u32,
            account: *const c_char,
            password_len: u32,
            password: *const c_void,
            item: *mut *mut c_void,
        ) -> i32;
        fn SecKeychainItemModifyAttributesAndData(
            item: *mut c_void,
            attributes: *const c_void,
            len: u32,
            data: *const c_void,
        ) -> i32;
        fn SecKeychainItemFreeContent(attributes: *mut c_void, data: *mut c_void) -> i32;
        fn SecKeychainItemDelete(item: *mut c_void) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(object: *const c_void);
    }

    /// The item, and (when asked for) its contents, from the default keychain search list.
    unsafe fn find(want_data: bool) -> Result<(*mut c_void, Option<Vec<u8>>), i32> {
        let (mut len, mut data, mut item) = (0u32, std::ptr::null_mut(), std::ptr::null_mut());
        let status = SecKeychainFindGenericPassword(
            std::ptr::null(),
            SERVICE.len() as u32,
            SERVICE.as_ptr().cast(),
            KEY.len() as u32,
            KEY.as_ptr().cast(),
            if want_data { &mut len } else { std::ptr::null_mut() },
            if want_data { &mut data } else { std::ptr::null_mut() },
            &mut item,
        );
        if status != 0 {
            return Err(status);
        }
        let bytes = (want_data && !data.is_null())
            .then(|| std::slice::from_raw_parts(data.cast::<u8>(), len as usize).to_vec());
        if !data.is_null() {
            SecKeychainItemFreeContent(std::ptr::null_mut(), data);
        }
        Ok((item, bytes))
    }

    /// Store `plain` under the item, replacing what was there. The envelope's `data` stays empty:
    /// the secret is in the Keychain, not in the file.
    pub fn seal(plain: &[u8]) -> Result<String, i32> {
        unsafe {
            let status = match find(false) {
                Ok((item, _)) => {
                    let status = SecKeychainItemModifyAttributesAndData(
                        item,
                        std::ptr::null(),
                        plain.len() as u32,
                        plain.as_ptr().cast(),
                    );
                    CFRelease(item);
                    status
                }
                Err(ERR_SEC_ITEM_NOT_FOUND) => SecKeychainAddGenericPassword(
                    std::ptr::null_mut(),
                    SERVICE.len() as u32,
                    SERVICE.as_ptr().cast(),
                    KEY.len() as u32,
                    KEY.as_ptr().cast(),
                    plain.len() as u32,
                    plain.as_ptr().cast(),
                    std::ptr::null_mut(),
                ),
                Err(status) => status,
            };
            if status == 0 { Ok(String::new()) } else { Err(status) }
        }
    }

    pub fn open(_data: &str) -> Result<Vec<u8>, i32> {
        unsafe {
            let (item, bytes) = find(true)?;
            CFRelease(item);
            Ok(bytes.unwrap_or_default())
        }
    }

    pub fn remove() {
        unsafe {
            if let Ok((item, _)) = find(false) {
                SecKeychainItemDelete(item);
                CFRelease(item);
            }
        }
    }
}

#[cfg(windows)]
mod os {
    use super::KEY;
    use plx_base::b64;
    use std::os::raw::c_void;

    /// `CRYPTPROTECT_UI_FORBIDDEN`: fail rather than show a prompt.
    const UI_FORBIDDEN: u32 = 0x1;

    #[repr(C)]
    struct Blob {
        len: u32,
        data: *mut u8,
    }

    #[link(name = "crypt32")]
    extern "system" {
        fn CryptProtectData(
            input: *const Blob,
            description: *const u16,
            entropy: *const Blob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            output: *mut Blob,
        ) -> i32;
        fn CryptUnprotectData(
            input: *const Blob,
            description: *mut *mut u16,
            entropy: *const Blob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            output: *mut Blob,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
        fn GetLastError() -> u32;
    }

    fn blob(bytes: &[u8]) -> Blob {
        Blob { len: bytes.len() as u32, data: bytes.as_ptr().cast_mut() }
    }

    /// The output blob's bytes, and the blob handed back to `LocalFree`.
    unsafe fn take(out: Blob) -> Vec<u8> {
        let bytes = std::slice::from_raw_parts(out.data, out.len as usize).to_vec();
        LocalFree(out.data.cast());
        bytes
    }

    /// The name doubles as DPAPI's optional entropy, so a blob sealed by another program for this
    /// user does not open here by accident.
    pub fn seal(plain: &[u8]) -> Result<String, i32> {
        let mut out = Blob { len: 0, data: std::ptr::null_mut() };
        let ok = unsafe {
            CryptProtectData(
                &blob(plain),
                std::ptr::null(),
                &blob(KEY.as_bytes()),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(unsafe { GetLastError() } as i32);
        }
        Ok(b64::encode(&unsafe { take(out) }))
    }

    pub fn open(data: &str) -> Result<Vec<u8>, i32> {
        let sealed = b64::decode(data).ok_or(-1)?;
        let mut out = Blob { len: 0, data: std::ptr::null_mut() };
        let ok = unsafe {
            CryptUnprotectData(
                &blob(&sealed),
                std::ptr::null_mut(),
                &blob(KEY.as_bytes()),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                UI_FORBIDDEN,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(unsafe { GetLastError() } as i32);
        }
        Ok(unsafe { take(out) })
    }

    pub fn remove() {}
}

#[cfg(not(any(target_os = "macos", windows)))]
mod os {
    pub fn seal(_plain: &[u8]) -> Result<String, i32> {
        Err(0)
    }
    pub fn open(_data: &str) -> Result<Vec<u8>, i32> {
        Err(0)
    }
    pub fn remove() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_envelope_this_platform_did_not_seal_is_locked_here() {
        let tv = Sealed { backend: Backend::Keymanager3, key: KEY.into(), iv: "aa".into(), data: "AAAA".into() };
        assert_eq!(open(&tv), None);
        let other = if cfg!(windows) { Backend::MacKeychain } else { Backend::WinDpapi };
        assert_eq!(open(&Sealed { backend: other, key: KEY.into(), iv: String::new(), data: String::new() }), None);
    }

    #[test]
    fn the_backends_have_stable_names_on_disk() {
        let sealed = Sealed { backend: Backend::MacKeychain, key: KEY.into(), iv: String::new(), data: String::new() };
        assert!(serde_json::to_string(&sealed).unwrap().contains("\"backend\":\"mac_keychain\""));
        let sealed = Sealed { backend: Backend::WinDpapi, ..sealed };
        assert!(serde_json::to_string(&sealed).unwrap().contains("\"backend\":\"win_dpapi\""));
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    #[test]
    fn without_a_key_store_the_session_takes_the_file_fallback() {
        assert_eq!(seal(b"{}"), None);
    }
}
