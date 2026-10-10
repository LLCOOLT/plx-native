//! The desktop port: the table the simulator (and, as it grows, the desktop app) installs in place
//! of the webOS one, and the entry that installs it (step L15b, docs/module-layers.md).
//!
//! It is a second implementation of the `plx_platform::tv` interfaces, not the webOS table running
//! its host arms: nothing here names `webos`, `keymanager`, `system` or `player::ffi`, which are
//! not compiled into this build. Each entry answers what the simulator answered when it ran the
//! webOS table, minus the lines that only made sense on a television (`webos: /etc/... unreadable`,
//! `devjail: soc=`), plus the answers a desktop has of its own: the OS language and regional format.
//! The rest a desktop app needs (a key store, its own window behaviour) belongs here as it is written.
use std::os::raw::{c_char, c_int, c_void};
use std::sync::Once;

use plx_base::eventlog::log;
use plx_platform::tv::{self, sandbox, secure, toast};

static PORT: tv::Port = tv::Port {
    probe_device,
    start_capability_probe,
    repair_sandbox,
    seal,
    open: |_| None,
    remove: |_, _| {},
    system_locale,
    system_utc_offset_s: || None,
    go_home,
    poll_home: nothing,
    deliver_toast,
    bind_window: |_| {},
    grab_surface,
    release_surface: nothing,
    arm_opaque_region: nothing,
    opaque_route: |_| {},
    clear_opaque_region: nothing,
    pump_bus: nothing,
    frame_probe_request: nothing,
    frame_probe_waiting: nothing,
    frame_probe_acquired: nothing,
    frame_probe_fields: String::new,
    sink: &plx_media::player::ffi_host::HostSink,
};

/// The simulator's entry (`src/bin/sim.rs`): install the port, then hand over to
/// `app::run_application`, the same body the webOS port's `plex_run` hands over to.
pub extern "C" fn plex_run(pms_host: *const c_char, pms_port: c_int) -> c_int {
    let _ = tv::install(&PORT);
    crate::app::run_application(pms_host, pms_port)
}

fn nothing() {}

/// Which desktop this is, for the log. The device identity `tv::device` carries is a television's
/// (firmware release, codename, model, board), so it stays empty here, which every reader already
/// prints as unknown rather than inventing a value. A desktop has no jail between the app and its
/// video path, so the sandbox verdict is the one a television outside the affected family gets.
fn probe_device() {
    log(&format!(
        "desktop: os={} arch={} family={}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::consts::FAMILY
    ));
    tv::device::publish_info(tv::device::Info::default());
    tv::device::publish_hardware(tv::device::Hardware::default());
    sandbox::publish(sandbox::Verdict::NotApplicable);
}

/// The Dolby Vision answer: the developer override when one is armed, else unknown from the host,
/// which is what the simulator has always published. Asking the display and decoders is the
/// desktop playback engine's job.
fn start_capability_probe() {
    use plx_platform::devcaps::dv::{self, DvCapability, DvProbe, ProbeSource};
    let probe = match dv::forced() {
        Some((capability, conflict)) => {
            if conflict {
                log("desktop-caps: dvcaps0 and dvcaps1 both armed; dvcaps0 wins");
            }
            let reason = if conflict { "override-conflict" } else { "override" };
            DvProbe { capability, source: ProbeSource::Override, reason }
        }
        None => DvProbe { capability: DvCapability::Unknown, source: ProbeSource::Host, reason: "host" },
    };
    dv::publish(probe);
    log(&format!(
        "desktop-caps: dolby-vision answer={} source={}",
        probe.capability.label(),
        probe.provenance()
    ));
    plx_machine::idle::invalidate();
}

/// There is no jail to repair. Never offered in practice: the verdict above never blocks.
fn repair_sandbox() -> Result<(), sandbox::Failure> {
    Err(sandbox::Failure::Unsupported)
}

/// No desktop key store yet, so the session falls back to the mode-0600 file, as it did when the
/// simulator ran the webOS key manager with no bus to reach it. Said once per process.
fn seal(_plain: &[u8]) -> Option<secure::Sealed> {
    static SAID: Once = Once::new();
    SAID.call_once(|| log("session protection: no desktop key store; using the 0600 file fallback"));
    None
}

/// The OS's own language and regional-format choices, which i18n normalizes and prefers over
/// `LANG`. A desktop's clock reads local time through libc, so `system_utc_offset_s` stays `None`.
/// Elsewhere (Linux) the environment is the OS's answer, so there is nothing to ask.
fn system_locale() -> tv::LocaleReply {
    #[cfg(any(target_os = "macos", windows))]
    {
        let (ui, fmt) = os_locale::read();
        tv::LocaleReply::Os { ui, fmt }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        tv::LocaleReply::NoPlatform
    }
}

/// macOS: the first entry of the user's preferred-languages list (System Settings > General >
/// Language & Region), and the current locale's identifier, which carries the Region choice
/// (`en_GB`, or `en_US@rg=gbzzzz` when the region differs from the language's).
#[cfg(target_os = "macos")]
mod os_locale {
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
mod os_locale {
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

/// BACK at a root: a desktop has no launcher to hand the screen to, so the press stays in the app.
fn go_home() {
    log("gohome: no system launcher on the desktop — the root press stays in the app");
}

fn deliver_toast(_message: &str, identity: toast::Identity) -> toast::Sent {
    log(&format!("toast: no system toast on the desktop — {identity:?} toast not sent"));
    toast::Sent { reply: None, outcome: toast::Outcome::NoBus }
}

/// What the window presents through. A desktop SDL window has no Wayland surface to borrow and no
/// hardware video plane beneath it, so this only reports what the driver granted.
fn grab_surface(_win: *mut c_void) {
    extern "C" {
        fn SDL_GetVersion(ver: *mut u8);
    }
    let mut version = [0u8; 3];
    unsafe { SDL_GetVersion(version.as_mut_ptr()) };
    let alpha = plx_gfx::gfx::log_framebuffer_bits();
    log(&format!("wm sdl={}.{}.{} desktop alpha={alpha}", version[0], version[1], version[2]));
}
