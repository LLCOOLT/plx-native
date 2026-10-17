//! The desktop window's own behaviour: a window a person can resize and take fullscreen. None of
//! it exists on a television, which owns the whole panel.
//!
//! Resizing is cheap here because the canvas is drawn offscreen at a size the window does not
//! choose (`plx_base::surface::probe`): a size change only re-letterboxes the blit and re-maps the
//! pointer (`surface::window_resized`), and the renderer never learns the window moved.
use std::os::raw::{c_int, c_void};

extern "C" {
    fn SDL_SetWindowFullscreen(win: *mut c_void, flags: u32) -> c_int;
    fn SDL_SetWindowMinimumSize(win: *mut c_void, w: c_int, h: c_int);
    fn SDL_EnableScreenSaver();
    fn SDL_DisableScreenSaver();
    fn SDL_CreateRGBSurfaceWithFormatFrom(
        pixels: *mut c_void,
        w: c_int,
        h: c_int,
        depth: c_int,
        pitch: c_int,
        format: u32,
    ) -> *mut c_void;
    fn SDL_SetWindowIcon(win: *mut c_void, icon: *mut c_void);
    fn SDL_FreeSurface(surface: *mut c_void);
}

/// `SDL_PIXELFORMAT_RGBA32`: bytes R, G, B, A in memory — `SDL_PIXELFORMAT_ABGR8888` on the
/// little-endian hosts every desktop build targets.
const SDL_PIXELFORMAT_RGBA32: u32 = 0x1676_2004;

/// The window's own icon (`tools/make-app-icons.py`): what the Windows taskbar and Alt-Tab and a
/// Linux title bar show while the app runs. A bundled Mac app takes its Dock icon from the bundle,
/// so there it only matters to an unbundled simulator.
const WINDOW_ICON: &[u8] = include_bytes!("../../../assets/app-icon/window-256.png");

/// `SDL_WINDOWEVENT`: resized, moved to another display, entered or left fullscreen.
pub(crate) const SDL_WINDOWEVENT: u32 = 0x200;
/// `SDL_WINDOW_FULLSCREEN_DESKTOP`: fullscreen at the display's own mode, so the switch is a
/// window-manager transition rather than a mode change, and leaving it restores the window.
const FULLSCREEN_DESKTOP: u32 = 0x1001;
/// `SDL_WINDOW_FULLSCREEN`, the bit both fullscreen flavours carry.
const FULLSCREEN: u32 = 0x1;
/// `SDLK_F11`: `SDL_SCANCODE_F11` (68) in SDL's scancode-keycode space.
const SDLK_F11: u32 = 0x4000_0044;
const SDLK_RETURN: u32 = 13;
const SDLK_F: u32 = b'f' as u32;
// `SDL_Keymod`, each pair being the left and right key.
const KMOD_CTRL: u16 = 0x0040 | 0x0080;
const KMOD_ALT: u16 = 0x0100 | 0x0200;
const KMOD_GUI: u16 = 0x0400 | 0x0800;
const KMOD_SHIFT: u16 = 0x0001 | 0x0002;

/// The smallest window the canvas is still legible in: a quarter of it.
const MIN_W: c_int = 480;
const MIN_H: c_int = 270;

/// Once, right after the window exists.
///
/// SDL keeps a display awake for as long as any SDL window is open, which suits a television and
/// not a laptop left on the home screen overnight, so the desktop lets the screensaver run. Holding
/// the display awake is video playback's to ask for, while it plays.
pub(crate) unsafe fn configure(win: *mut c_void) {
    SDL_SetWindowMinimumSize(win, MIN_W, MIN_H);
    SDL_EnableScreenSaver();
    set_icon(win);
}

/// Give the window [`WINDOW_ICON`]. SDL copies the pixels, so the decoded buffer and the surface
/// wrapping it are released at once. A decode or surface failure leaves the platform's default
/// icon and a log line — never a failed boot over a cosmetic.
unsafe fn set_icon(win: *mut c_void) {
    let Some((w, h, mut rgba)) = plx_gfx::img::img_decode_owned(WINDOW_ICON) else {
        plx_base::eventlog::log("window: icon did not decode; keeping the default");
        return;
    };
    let surface = SDL_CreateRGBSurfaceWithFormatFrom(
        rgba.as_mut_ptr() as *mut c_void,
        w as c_int,
        h as c_int,
        32,
        (w * 4) as c_int,
        SDL_PIXELFORMAT_RGBA32,
    );
    if surface.is_null() {
        plx_base::eventlog::log("window: icon surface refused; keeping the default");
        return;
    }
    SDL_SetWindowIcon(win, surface);
    SDL_FreeSurface(surface);
}

/// Hold the display awake while a video plays, and let it sleep again once it stops or pauses.
/// Called every frame; the SDL call happens only when the answer changes.
pub(crate) fn keep_display_awake(playing: bool) {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    static AWAKE: AtomicBool = AtomicBool::new(false);
    if AWAKE.swap(playing, Relaxed) != playing {
        unsafe {
            if playing {
                SDL_DisableScreenSaver();
            } else {
                SDL_EnableScreenSaver();
            }
        }
    }
}

/// Is this press the platform's fullscreen shortcut? F11 (Windows, Linux), Ctrl+Cmd+F (macOS) and
/// Alt+Enter (Windows) are all accepted everywhere: none of them means anything else in this app.
/// `keymod` is the press's `SDL_Keymod`. Pure, so the table is graded on the host.
pub(crate) fn toggles_fullscreen(sym: u32, keymod: u16) -> bool {
    let only = |wanted: u16| {
        let held = keymod & (KMOD_CTRL | KMOD_ALT | KMOD_GUI | KMOD_SHIFT);
        let groups = [KMOD_CTRL, KMOD_ALT, KMOD_GUI, KMOD_SHIFT];
        groups.iter().all(|&g| (held & g != 0) == (wanted & g != 0))
    };
    (sym == SDLK_F11 && only(0))
        || (sym == SDLK_F && only(KMOD_CTRL | KMOD_GUI))
        || (sym == SDLK_RETURN && only(KMOD_ALT))
}

/// Publish whether the window is fullscreen (`plx_base::surface::fullscreen`), from the window's
/// own flags — after a toggle, and on every window event, since the platform can leave fullscreen
/// on its own (macOS's green button, a window manager's shortcut).
pub(crate) unsafe fn sync_fullscreen(win: *mut c_void) {
    plx_base::surface::set_fullscreen(super::SDL_GetWindowFlags(win) & FULLSCREEN != 0);
}

/// Enter fullscreen, or leave it.
pub(crate) unsafe fn toggle_fullscreen(win: *mut c_void) {
    let full = super::SDL_GetWindowFlags(win) & FULLSCREEN != 0;
    let rc = SDL_SetWindowFullscreen(win, if full { 0 } else { FULLSCREEN_DESKTOP });
    sync_fullscreen(win);
    plx_base::eventlog::log(&format!(
        "window: fullscreen {} rc={rc}",
        if full { "off" } else { "on" }
    ));
}

/// What a desktop mouse button means to an interface built for one pointer button.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Button {
    Primary,
    Back,
    Ignored,
}

/// `SDL_MouseButtonEvent::button`: 1 left, 2 middle, 3 right, 4 and 5 the side buttons, of which
/// 4 (`SDL_BUTTON_X1`) is the one browsers and file managers read as Back. The right button is
/// Back too, as in other ten-foot interfaces on a desktop (Kodi, Plex HTPC): this interface has no
/// context menu for it to open, and a mouse with no side buttons otherwise has no way out of the
/// player, whose only exit on a television is the remote's BACK.
pub(crate) fn mouse_button(button: u8) -> Button {
    match button {
        1 => Button::Primary,
        3 | 4 => Button::Back,
        _ => Button::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded window icon is a real image at the size `tools/make-app-icons.py` writes, so
    /// `set_icon` never falls back to the platform default over a bad asset.
    #[test]
    fn the_window_icon_decodes_at_its_generated_size() {
        let (w, h, rgba) = plx_gfx::img::img_decode_owned(WINDOW_ICON).expect("the window icon decodes");
        assert_eq!((w, h), (256, 256));
        assert_eq!(rgba.len(), 256 * 256 * 4);
        // a rounded tile: the corner is transparent, the centre opaque
        assert_eq!(rgba[3], 0, "the corner pixel is outside the tile");
        assert_eq!(rgba[((128 * 256 + 128) * 4 + 3) as usize], 255, "the centre is inside it");
    }

    #[test]
    fn the_fullscreen_shortcuts_and_nothing_near_them() {
        assert!(toggles_fullscreen(SDLK_F11, 0));
        assert!(toggles_fullscreen(SDLK_F, 0x0040 | 0x0400)); // left Ctrl + left Cmd
        assert!(toggles_fullscreen(SDLK_F, 0x0080 | 0x0800)); // right Ctrl + right Cmd
        assert!(toggles_fullscreen(SDLK_RETURN, 0x0100));
        // Plain keys keep their meaning: RETURN is OK, 'f' types an f into Search.
        assert!(!toggles_fullscreen(SDLK_RETURN, 0));
        assert!(!toggles_fullscreen(SDLK_F, 0));
        assert!(!toggles_fullscreen(SDLK_F, 0x0400)); // Cmd+F alone
        assert!(!toggles_fullscreen(SDLK_F, 0x0040 | 0x0400 | 0x0001)); // with Shift
        assert!(!toggles_fullscreen(SDLK_F11, 0x0100));
        assert!(!toggles_fullscreen(SDLK_RETURN, 0x0100 | 0x0040));
        // Caps Lock and Num Lock are modifiers SDL reports too, and change nothing.
        assert!(toggles_fullscreen(SDLK_F11, 0x2000 | 0x1000));
    }

    #[test]
    fn only_the_primary_button_clicks_and_the_right_and_side_buttons_go_back() {
        assert_eq!(mouse_button(1), Button::Primary);
        assert_eq!(mouse_button(3), Button::Back);
        assert_eq!(mouse_button(4), Button::Back);
        for b in [0, 2, 5] {
            assert_eq!(mouse_button(b), Button::Ignored, "button {b}");
        }
    }
}
