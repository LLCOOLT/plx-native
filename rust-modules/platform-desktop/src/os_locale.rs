//! The OS's own language and regional-format choices, which i18n normalizes and prefers over `LANG`
//! (`LocaleReply::Os`). macOS and Windows only; elsewhere (Linux) the environment is the OS's answer.
/// macOS: the first entry of the user's preferred-languages list (System Settings > General >
/// Language & Region), and the current locale's identifier, which carries the Region choice
/// (`en_GB`, or `en_US@rg=gbzzzz` when the region differs from the language's).
#[cfg(target_os = "macos")]
mod imp {
    use std::os::raw::{c_char, c_void};
    type CFTypeRef = *const c_void;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFLocaleCopyPreferredLanguages() -> CFTypeRef;
        fn CFArrayGetCount(array: CFTypeRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
        fn CFLocaleCopyCurrent() -> CFTypeRef;
        fn CFLocaleGetIdentifier(locale: CFTypeRef) -> CFTypeRef;
        fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFRelease(object: CFTypeRef);
    }

    /// A borrowed CFString as Rust text; `None` for null or anything that does not fit.
    unsafe fn text(s: CFTypeRef) -> Option<String> {
        if s.is_null() {
            return None;
        }
        let mut buf = [0 as c_char; 128];
        if CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as isize, K_CF_STRING_ENCODING_UTF8) == 0 {
            return None;
        }
        std::ffi::CStr::from_ptr(buf.as_ptr()).to_str().ok().map(str::to_string)
    }

    pub fn read() -> (Option<String>, Option<String>) {
        unsafe {
            let languages = CFLocaleCopyPreferredLanguages();
            let ui = if languages.is_null() {
                None
            } else {
                let first = (CFArrayGetCount(languages) > 0).then(|| text(CFArrayGetValueAtIndex(languages, 0)));
                CFRelease(languages);
                first.flatten()
            };
            let current = CFLocaleCopyCurrent();
            let fmt = if current.is_null() {
                None
            } else {
                let id = text(CFLocaleGetIdentifier(current));
                CFRelease(current);
                id
            };
            (ui, fmt)
        }
    }
}

/// Windows: the user's display language (Settings > Time & language > Language) and the locale
/// its Regional format is set to, both as BCP-47 names (`en-GB`).
#[cfg(windows)]
mod imp {
    const LOCALE_NAME_MAX_LENGTH: usize = 85;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
        fn LCIDToLocaleName(locale: u32, name: *mut u16, len: i32, flags: u32) -> i32;
        fn GetUserDefaultLocaleName(name: *mut u16, len: i32) -> i32;
    }

    /// A wide name a kernel32 call wrote, whose length (with the terminator) it returned.
    fn text(buf: &[u16], written: i32) -> Option<String> {
        let n = usize::try_from(written).ok()?.checked_sub(1)?;
        (n > 0).then(|| String::from_utf16(&buf[..n]).ok()).flatten()
    }

    pub fn read() -> (Option<String>, Option<String>) {
        let mut ui = [0u16; LOCALE_NAME_MAX_LENGTH];
        let mut fmt = [0u16; LOCALE_NAME_MAX_LENGTH];
        unsafe {
            let ui_len = LCIDToLocaleName(u32::from(GetUserDefaultUILanguage()), ui.as_mut_ptr(), ui.len() as i32, 0);
            let fmt_len = GetUserDefaultLocaleName(fmt.as_mut_ptr(), fmt.len() as i32);
            (text(&ui, ui_len), text(&fmt, fmt_len))
        }
    }
}


#[cfg(any(target_os = "macos", windows))]
pub use imp::read;
